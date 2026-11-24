//! Every window this app shows, and the one link it opens.
//!
//! Nothing else in the program draws anything: the work modules return a result
//! and a message, and this file is what turns that message into a dialog. That is
//! what keeps the logic testable from a console.

use std::{io::Write, ptr};

use windows_sys::Win32::{
    System::Console::{AttachConsole, ATTACH_PARENT_PROCESS},
    UI::{
        Shell::ShellExecuteW,
        WindowsAndMessaging::{
            MessageBoxW, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK,
            MB_YESNO, SW_SHOWNORMAL,
        },
    },
};

use crate::APP_NAME;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn message(text: &str, flags: u32) -> i32 {
    unsafe {
        MessageBoxW(
            ptr::null_mut(),
            wide(text).as_ptr(),
            wide(APP_NAME).as_ptr(),
            flags,
        )
    }
}

pub fn info(text: &str) {
    message(text, MB_OK | MB_ICONINFORMATION);
}

pub fn error(text: &str) {
    message(text, MB_OK | MB_ICONERROR);
}

/// A question answered with Yes / No.
pub fn ask(text: &str) -> bool {
    message(text, MB_YESNO | MB_ICONQUESTION) == IDYES
}

/// The same, for a message that is not a question about the app itself.
pub fn offer(text: &str) -> bool {
    message(text, MB_YESNO | MB_ICONINFORMATION) == IDYES
}

pub fn open(url: &str) -> bool {
    let result = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            wide("open").as_ptr(),
            wide(url).as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    result as usize > 32 // the documented "greater than 32 means success"
}

/// Print a line for `--selftest`. The app is built as a windowed program, so it
/// has no console of its own; attach to the one that started it, if there is one,
/// and let the log carry the report when there is not.
pub fn console(text: &str) {
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    if let Ok(mut out) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
        let _ = out.write_all(text.as_bytes());
        let _ = out.write_all(b"\r\n");
    }
}
