//! What the shell hands us, turned into the path we put on the clipboard.
//!
//! Everything here works on UTF-16 code units rather than `String`: a file name is
//! UTF-16 to Windows, and a name holding an unpaired surrogate would come back
//! with a replacement character in it if it went through `to_string_lossy`.

use std::{
    ffi::{OsStr, OsString},
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::PathBuf,
};

const QUOTE: u16 = b'"' as u16;
const BACKSLASH: u16 = b'\\' as u16;

/// Explorer builds the command line as `"CopyFilePath.exe" copy "%1"`. A path that
/// ends in a backslash - a drive root, `D:\` - arrives as `D:"`, because the
/// command-line parser reads `\"` as an escaped quote. Put the backslash back.
pub fn normalize(path: &OsStr) -> OsString {
    let mut units: Vec<u16> = path.encode_wide().collect();
    if units.last() == Some(&QUOTE) {
        units.pop();
        units.push(BACKSLASH);
    }
    OsString::from_wide(&strip_verbatim(&units))
}

/// The shell reports long paths with a `\\?\` prefix; nobody wants that on the
/// clipboard, and `\\?\UNC\server\share` is the same path as `\\server\share`.
fn strip_verbatim(units: &[u16]) -> Vec<u16> {
    let verbatim: Vec<u16> = r"\\?\".encode_utf16().collect();
    if units.len() < verbatim.len() || !starts_with_ignore_case(units, &verbatim) {
        return units.to_vec();
    }

    let rest = &units[verbatim.len()..];
    let unc: Vec<u16> = r"UNC\".encode_utf16().collect();
    if starts_with_ignore_case(rest, &unc) {
        let mut out = vec![BACKSLASH, BACKSLASH];
        out.extend_from_slice(&rest[unc.len()..]);
        out
    } else {
        rest.to_vec()
    }
}

fn starts_with_ignore_case(units: &[u16], prefix: &[u16]) -> bool {
    units.len() >= prefix.len()
        && units[..prefix.len()]
            .iter()
            .zip(prefix)
            .all(|(a, b)| ascii_eq(*a, *b))
}

fn ascii_eq(a: u16, b: u16) -> bool {
    a == b || (a < 0x80 && b < 0x80 && (a as u8).eq_ignore_ascii_case(&(b as u8)))
}

/// "Full path" means absolute: a relative argument - `copy .\notes.txt`, or a path
/// dropped from a console - is resolved against the working directory.
pub fn absolute(path: OsString) -> OsString {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        return path.into_os_string();
    }
    std::path::absolute(&path)
        .or_else(|_| std::env::current_dir().map(|cwd| cwd.join(&path)))
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|_| path.into_os_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_wide(text: &str) -> OsString {
        OsString::from_wide(&text.encode_utf16().collect::<Vec<_>>())
    }

    #[test]
    fn a_quoted_drive_root_gets_its_backslash_back() {
        // What the shell hands over for the drive root `C:\`: the parser has eaten
        // the backslash as an escape, so the argument is `C:"`.
        let quoted_root = OsString::from(concat!("C", ":", "\""));
        assert_eq!(normalize(&quoted_root), from_wide("C:\\"));
    }

    #[test]
    fn a_verbatim_drive_path_loses_its_prefix() {
        assert_eq!(
            normalize(&from_wide(r"\\?\C:\Models\file.gguf")),
            from_wide(r"C:\Models\file.gguf")
        );
    }

    #[test]
    fn a_verbatim_unc_path_becomes_a_normal_unc_path() {
        assert_eq!(
            normalize(&from_wide(r"\\?\UNC\server\share\file.txt")),
            from_wide(r"\\server\share\file.txt")
        );
    }

    #[test]
    fn a_plain_path_is_left_alone() {
        assert_eq!(
            normalize(&from_wide(r"D:\Models\file.gguf")),
            from_wide(r"D:\Models\file.gguf")
        );
    }

    #[test]
    fn a_relative_path_is_made_absolute() {
        let made = absolute(from_wide(r"notes.txt"));
        assert!(PathBuf::from(&made).is_absolute());
    }

    #[test]
    fn an_absolute_path_is_unchanged() {
        let path = from_wide(r"C:\Models\file.gguf");
        assert_eq!(absolute(path.clone()), path);
    }
}
