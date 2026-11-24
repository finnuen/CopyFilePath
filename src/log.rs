//! The one log file. Every failure ends up here with what caused it, because the
//! app has no console and a dialog the user dismissed is a message that is gone.
//!
//! Location: `%LOCALAPPDATA%\CopyFilePath\CopyFilePath.log`. Best effort only -
//! logging must never be the reason something fails.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

const MAX_BYTES: u64 = 256 * 1024;

pub fn path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(|base| PathBuf::from(base).join("CopyFilePath").join("CopyFilePath.log"))
}

pub fn write(event: &str) {
    let Some(path) = path() else { return };

    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if fs::metadata(&path).map(|meta| meta.len() > MAX_BYTES).unwrap_or(false) {
        let _ = fs::rename(&path, path.with_extension("log.old"));
    }

    let line = format!("{}  {event}\r\n", local_time());
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all(line.as_bytes());
    }
}

/// Local time, so a line can be compared with what the user saw happen.
fn local_time() -> String {
    use windows_sys::Win32::{Foundation::SYSTEMTIME, System::SystemInformation::GetLocalTime};

    let mut now = SYSTEMTIME {
        wYear: 0,
        wMonth: 0,
        wDayOfWeek: 0,
        wDay: 0,
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
    };
    unsafe { GetLocalTime(&mut now) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond
    )
}
