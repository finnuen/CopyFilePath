//! One selection becomes one clipboard entry.
//!
//! Explorer starts a separate process per selected item, and each one only knows
//! its own path. They cooperate through a spool file: everyone appends their path,
//! one of them (whoever grabs the mutex) waits for the batch to stop growing, then
//! puts all of them on the clipboard together and deletes the file.
//!
//! Two things are easy to get wrong here, and both were real bugs:
//!
//! * A process that starts *after* its siblings have copied would begin a batch of
//!   its own and put one path on the clipboard, replacing the selection. So the
//!   last copy is remembered in a small record file, and a batch that starts right
//!   after one folds it in instead of replacing it.
//! * A process can append while the leader is reading, and a process that opens the
//!   file just as it is deleted gets nothing. The read is repeated until the file
//!   stops changing, and joining a batch that disappeared in the meantime starts a
//!   fresh one rather than failing.

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    ptr,
    time::{Duration, Instant, SystemTime},
};

use windows_sys::Win32::{
    Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0},
    System::Threading::{CreateMutexW, WaitForSingleObject},
};

use crate::{log, paths};

/// A batch older than this was left behind by a process that died before it could
/// clean up; a live batch never lasts more than a second or two.
const STALE_BATCH: Duration = Duration::from_secs(2);
/// Always wait at least this long: Explorer's per-item processes are launched one
/// after another, and the first one is the leader.
const SETTLE_MIN: Duration = Duration::from_millis(200);
/// Then stop as soon as the file has stopped growing.
const SETTLE_QUIET: Duration = Duration::from_millis(80);
const SETTLE_MAX: Duration = Duration::from_millis(1500);
/// How soon after a copy a new batch still counts as part of the same selection.
/// A person cannot make two deliberate copies closer together than this, while a
/// slow process launch of the same selection can easily take this long.
const CONTINUATION: Duration = Duration::from_millis(1200);


pub fn spool_path() -> PathBuf {
    std::env::temp_dir().join("CopyFilePath-selection.txt")
}

/// The paths put on the clipboard by the last copy, kept only to fold a late
/// process of the same selection back in.
fn recent_path() -> PathBuf {
    std::env::temp_dir().join("CopyFilePath-recent.txt")
}

/// Copy `selection` - plus whatever other processes appended meanwhile - to the
/// clipboard. Returns how many paths went in, 0 when another process did the work.
pub fn copy(selection: &[OsString]) -> Result<usize, String> {
    let spool = spool_path();
    let wanted: Vec<OsString> = selection
        .iter()
        .filter(|path| !path.is_empty())
        .map(|path| paths::absolute(paths::normalize(path)))
        .collect();
    if wanted.is_empty() {
        return Err("Nothing to copy.".into());
    }

    {
        let mut file =
            open_batch(&spool).map_err(|e| format!("Could not write {}: {e}", spool.display()))?;
        write_lines(&mut file, &wanted)
            .map_err(|e| format!("Could not write {}: {e}", spool.display()))?;
    }
    // When this process started, so a batch that only exists because this process
    // started late can be told apart from the user making a new selection. A file's
    // creation time cannot be used: NTFS restores it when a name is reused within
    // 15 seconds ("file system tunneling"), and every selection reuses the spool
    // name - that is what made the first version of this fix do nothing.
    let started = process_started();

    if !is_leader() {
        return Ok(0); // the leader reads this file and copies our path too
    }

    settle(&spool);
    let mut lines = read_stable(&spool);

    if let Some(previous) = previous_copy(started) {
        lines.extend(previous);
    }
    lines.retain(|line| !line.is_empty());
    if lines.is_empty() {
        lines = wanted; // the read failed, but our own paths are still good
    }
    sort_paths(&mut lines);

    // Clipboard first, then the record, then the batch file: a sibling that starts
    // its own batch can only do so once the file is gone, so it always finds the
    // record - and because it writes the clipboard last, its merged content wins
    // over this copy instead of the other way round.
    set_clipboard(&lines)?;
    // One line per copy, without the paths: it says the app was actually invoked,
    // which is the first thing to know when something else happened instead.
    log::write(&format!("copy: {} path(s)", lines.len()));
    write_record(&lines);
    let _ = fs::remove_file(&spool);
    Ok(lines.len())
}

/// Put a whole selection on the clipboard in one go.
///
/// The COM verb is handed the complete selection in a single call, so there is
/// nothing to wait for and no spool involved - this is the snappy path.
pub fn copy_now(selection: &[OsString]) -> Result<usize, String> {
    let mut lines: Vec<OsString> = selection
        .iter()
        .filter(|path| !path.is_empty())
        .map(|path| paths::absolute(paths::normalize(path)))
        .collect();
    if lines.is_empty() {
        return Err("Nothing to copy.".into());
    }

    sort_paths(&mut lines);
    set_clipboard(&lines)?;
    log::write(&format!("copy: {} path(s)", lines.len()));
    Ok(lines.len())
}

/// The paths of the copy that finished just before this process started, if any.
fn previous_copy(started: Option<u64>) -> Option<Vec<OsString>> {
    let started = started?;
    let recent = recent_path();
    let finished = fs::metadata(&recent).and_then(|meta| meta.modified()).ok()?;
    let finished = filetime(finished)?;
    // A process that started before that copy finished belongs to the same
    // selection (Explorer starts one process per item, and one of them can be
    // slow). One that started long after it is the user copying something else.
    if started.saturating_sub(finished) > CONTINUATION.as_nanos() as u64 / 100 {
        // Too old to be a continuation, and it holds the paths that were copied -
        // no reason to leave them in %TEMP% for the session.
        let _ = fs::remove_file(&recent);
        return None;
    }

    let previous = read_lines(&recent).unwrap_or_default();
    if previous.is_empty() {
        None
    } else {
        log::write(&format!(
            "folded {} path(s) from the previous copy into this one",
            previous.len()
        ));
        Some(previous)
    }
}

/// The current process's creation time, in the 100 ns units FILETIME uses.
fn process_started() -> Option<u64> {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetCurrentProcess, GetProcessTimes},
    };

    let empty = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut creation, mut exit, mut kernel, mut user) = (empty, empty, empty, empty);
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    if ok == 0 {
        return None;
    }
    Some((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

/// A `SystemTime` in the same 100 ns units, for comparing with `process_started`.
fn filetime(time: SystemTime) -> Option<u64> {
    const EPOCH: u64 = 116_444_736_000_000_000; // 1601 -> 1970, in 100 ns units
    let since_epoch = time.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(EPOCH + since_epoch.as_nanos() as u64 / 100)
}

fn write_record(lines: &[OsString]) {
    let Ok(mut file) = File::create(recent_path()) else {
        return; // losing the record only costs a late process its merge
    };
    let _ = write_lines(&mut file, lines);
}

/// Open the batch, creating it if this is the first process of the selection.
fn open_batch(spool: &Path) -> io::Result<File> {
    for _ in 0..5 {
        match OpenOptions::new().create_new(true).write(true).open(spool) {
            Ok(file) => return Ok(file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if is_stale(spool) {
                    let _ = fs::remove_file(spool);
                    if !spool.exists() {
                        continue;
                    }
                }
                match OpenOptions::new().append(true).open(spool) {
                    Ok(file) => return Ok(file),
                    // The batch was copied and deleted between the check above and
                    // this open: start a fresh one instead of failing the copy.
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        }
    }
    OpenOptions::new().append(true).open(spool)
}

fn is_stale(spool: &Path) -> bool {
    fs::metadata(spool)
        .and_then(|meta| meta.modified())
        .map(|modified| {
            modified
                .elapsed()
                .map(|age| age > STALE_BATCH)
                .unwrap_or(true)
        })
        .unwrap_or(true)
}

/// Wait until the batch stops growing, so no path arrives after it was read.
fn settle(spool: &Path) {
    let start = Instant::now();
    let mut last = file_len(spool);
    loop {
        std::thread::sleep(SETTLE_QUIET);
        let now = file_len(spool);
        let quiet = now == last;
        last = now;
        if start.elapsed() >= SETTLE_MAX || (quiet && start.elapsed() >= SETTLE_MIN) {
            return;
        }
    }
}

/// Read the batch, again if another process appended while the read was running.
fn read_stable(spool: &Path) -> Vec<OsString> {
    let mut lines = Vec::new();
    for _ in 0..5 {
        let before = file_len(spool);
        lines = read_lines(spool).unwrap_or_default();
        if file_len(spool) == before {
            break;
        }
        std::thread::sleep(SETTLE_QUIET);
    }
    lines
}

fn file_len(spool: &Path) -> u64 {
    fs::metadata(spool).map(|meta| meta.len()).unwrap_or(0)
}

/// Whoever takes the mutex gathers the batch; the others just exit.
fn is_leader() -> bool {
    let name: Vec<u16> = "Local\\CopyFilePathLeader"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mutex = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
    if mutex.is_null() {
        return true; // no mutex is no reason to drop the selection
    }
    let taken = unsafe { WaitForSingleObject(mutex, 0) };
    taken == WAIT_OBJECT_0 || taken == WAIT_ABANDONED
}

/// One path per line, written as UTF-16 code units. A file name is UTF-16 to
/// Windows; going through UTF-8 would have to replace the rare name that holds an
/// unpaired surrogate, and the clipboard would then hold a path that does not
/// exist.
pub(crate) fn encode_lines(paths: &[OsString]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for path in paths {
        for unit in path.encode_wide() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&0x000Au16.to_le_bytes()); // '\n'
    }
    bytes
}

pub(crate) fn decode_lines(bytes: &[u8]) -> Vec<OsString> {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();

    units
        .split(|unit| *unit == 0x000A)
        .filter(|line| !line.is_empty())
        .map(OsString::from_wide)
        .collect()
}

fn write_lines(file: &mut File, paths: &[OsString]) -> io::Result<()> {
    file.write_all(&encode_lines(paths))
}

fn read_lines(spool: &Path) -> io::Result<Vec<OsString>> {
    fs::read(spool).map(|bytes| decode_lines(&bytes))
}

/// Name order, once each. Explorer's per-item processes arrive in no particular
/// order, so sorting is what makes the clipboard look like the selection.
pub(crate) fn sort_paths(lines: &mut Vec<OsString>) {
    lines.retain(|line| !line.is_empty());
    lines.sort_by_key(|line| line.to_string_lossy().to_lowercase());
    lines.dedup();
}

/// CF_UNICODETEXT: UTF-16 with a terminating NUL, taken as-is by the clipboard.
fn set_clipboard(lines: &[OsString]) -> Result<(), String> {
    let mut units: Vec<u16> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            units.extend_from_slice(&[b'\r' as u16, b'\n' as u16]);
        }
        units.extend(line.encode_wide());
    }
    units.push(0);

    let bytes = unsafe { std::slice::from_raw_parts(units.as_ptr().cast::<u8>(), units.len() * 2) };

    // The clipboard has to be opened before data can go on it, and another program
    // can hold it open for a moment; try again rather than telling the user their
    // copy failed.
    let mut last = String::new();
    for attempt in 0..10 {
        match clipboard_win::raw::open() {
            Ok(()) => {
                let placed =
                    clipboard_win::raw::set(clipboard_win::formats::CF_UNICODETEXT, bytes);
                let _ = clipboard_win::raw::close();
                match placed {
                    Ok(()) => return Ok(()),
                    Err(e) => last = e.to_string(),
                }
            }
            Err(e) => last = e.to_string(),
        }
        if attempt < 9 {
            std::thread::sleep(Duration::from_millis(60));
        }
    }

    log::write(&format!("clipboard failed after 10 attempts: {last}"));
    Err(format!("Could not access the clipboard: {last}"))
}

#[cfg(test)]
mod tests {
    use super::{decode_lines, encode_lines, sort_paths};
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    fn from_wide(text: &str) -> OsString {
        OsString::from_wide(&text.encode_utf16().collect::<Vec<_>>())
    }

    #[test]
    fn a_batch_survives_the_round_trip_unchanged() {
        let paths = vec![
            from_wide(r"C:\Models\file.gguf"),
            from_wide(r"\\server\share\Ωmega ünïcode.txt"),
            from_wide(r"D:\"),
        ];
        assert_eq!(decode_lines(&encode_lines(&paths)), paths);
    }

    #[test]
    fn sorting_is_case_insensitive_and_removes_duplicates() {
        let mut paths = vec![
            from_wide(r"C:\b.txt"),
            from_wide(r"C:\A.txt"),
            from_wide(r"C:\a.txt"),
            from_wide(r"C:\C.txt"),
        ];
        sort_paths(&mut paths);
        let sorted: Vec<String> = paths
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect();
        assert_eq!(sorted, vec![r"C:\A.txt", r"C:\a.txt", r"C:\b.txt", r"C:\C.txt"]);

        let mut duplicates = vec![from_wide(r"C:\x"), from_wide(r"C:\x")];
        sort_paths(&mut duplicates);
        assert_eq!(duplicates.len(), 1);
    }
}
