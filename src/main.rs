//! Copy file path - adds "Copy file path" to the Windows Explorer right-click menu.
//!
//! Entry point. The order matters, and this is the whole of it:
//!
//!   1. read the command line
//!   2. interactive mode only: start the update check on a worker thread, so it
//!      runs while the user reads the prompt and costs nothing when they are done
//!   3. install / uninstall / copy
//!   4. offer the newer version, if the check found one
//!
//! Modes:
//!
//!   (no arguments)             install or uninstall, after asking
//!   install                    write the menu entries silently
//!   uninstall                  remove them
//!   copy "<path>"...           put the paths on the clipboard - what the menu runs
//!   "<path>"...                the same, for files dropped onto the exe
//!   check-update               ask GitHub for the newest release
//!   about                      show the version
//!   selftest                   run the checks; exit code 1 if any fails
//!
//! The `copy` mode is the hot path: no window, no network, and it must not wait
//! for anything it does not have to.
#![windows_subsystem = "windows"]

use std::{env, ffi::OsString, path::Path, thread};

use copy_file_path::{
    copy_paths, install, log, selftest, ui, update_check,
    update_check::Answer,
    version, APP_NAME,
};

use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDNO, IDYES, MB_ICONQUESTION, MB_YESNOCANCEL,
};

fn main() {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let command = args
        .first()
        .map(|a| a.to_string_lossy().to_ascii_lowercase());

    match command.as_deref() {
        None => interactive(),
        Some("install" | "--install" | "/install") => install_now(),
        Some("uninstall" | "--uninstall" | "/uninstall") => uninstall_now(),
        Some("copy") => copy(&args[1..]),
        // The shell handler hands over the whole selection at once, so it needs
        // neither the spool nor the merge window: this path copies immediately.
        Some("copy-now") => copy_now(&args[1..]),
        Some("about" | "--about" | "version" | "--version") => about(),
        Some("check-update" | "--check-update" | "update" | "--update") => check_update(),
        Some("selftest" | "--selftest") => {
            if !selftest::run() {
                std::process::exit(1);
            }
        }

        Some("help" | "--help" | "-h" | "/?") => help(),
        _ => dropped(&args),
    }
}

// ---------------------------------------------------------------- modes

fn interactive() {
    // Started before anything is shown, joined at the very end: a user who needs
    // four seconds to read the dialog gets the answer for free.
    let check = thread::spawn(update_check::check);

    let mut uninstalled = false;
    if install::is_installed() {
        let answer = ui::message(
            &format!(
                "{APP_NAME} {} is installed and in your right-click menu.\n\n\
                 Yes = Reinstall / update\nNo = Uninstall\nCancel = Close",
                version()
            ),
            MB_YESNOCANCEL | MB_ICONQUESTION,
        );
        if answer == IDYES {
            install_now();
        } else if answer == IDNO {
            uninstall_now();
            uninstalled = true;
        }
    } else if ui::ask(&format!(
        "{APP_NAME} {}\n\n\
         Add \"{APP_NAME}\" to the right-click menu?\n\n\
         It puts the full path (folder + file name) on the clipboard in one click.\n\
         No admin rights needed; run this file again to remove it.",
        version()
    )) {
        install_now();
    }

    if uninstalled {
        return; // no update offer for someone who just removed the app
    }
    if let Ok(Answer::Newer(tag)) = check.join() {
        offer(&tag);
    }
}

fn install_now() {
    match install::install() {
        Ok(()) => ui::info(&format!(
            "Done! \"{APP_NAME}\" is now in your right-click menu.\n\n\
             On Windows 11, click \"Show more options\" (or press Shift+F10) to see it."
        )),
        Err(e) => fail("Install", e),
    }
}

fn uninstall_now() {
    match install::uninstall() {
        Ok(()) => {
            ui::info(&format!(
                "\"{APP_NAME}\" was removed from the right-click menu."
            ));
            // Last, after the dialog is gone: see the note in `install.rs`.
            install::remove_installed_copy();
        }
        Err(e) => fail("Uninstall", e),
    }
}

fn copy(args: &[OsString]) {
    if let Err(e) = copy_paths::copy(args) {
        log::write(&format!("copy failed: {e}"));
        ui::error(&e);
    }
}

fn copy_now(args: &[OsString]) {
    if let Err(e) = copy_paths::copy_now(args) {
        log::write(&format!("copy failed: {e}"));
        ui::error(&e);
    }
}

/// Anything else on the command line is treated as a dropped file.
fn dropped(args: &[OsString]) {
    let existing: Vec<OsString> = args
        .iter()
        .filter(|a| Path::new(a).exists())
        .cloned()
        .collect();
    if existing.is_empty() {
        help();
    } else {
        copy(&existing);
    }
}

fn check_update() {
    match update_check::check() {
        Answer::Newer(tag) => offer(&tag),
        Answer::UpToDate => ui::info(&format!(
            "{APP_NAME} {} is the newest release.",
            version()
        )),
        Answer::NoRelease => ui::info(&format!(
            "{APP_NAME} {} - this project has no release on GitHub yet.",
            version()
        )),
        Answer::Unknown(why) => ui::info(&format!(
            "Could not check for updates.\n\n{why}\n\nNothing is wrong with the app."
        )),
    }
}

fn offer(tag: &str) {
    if ui::offer(&format!(
        "Version {tag} is available - this is {}.\n\n\
         Open the download page?",
        version()
    )) {
        if !ui::open(update_check::RELEASE_PAGE) {
            log::write("could not open the download page");
        }
    }
}

fn about() {
    ui::info(&format!(
        "{APP_NAME} {}\n\n\
         Adds \"{APP_NAME}\" to the Windows Explorer right-click menu and copies the\n\
         full path of the selection.\n\n\
         Updates: {}\n\
         Log: {}\n\n\
         Run with check-update to look for a newer release.",
        version(),
        update_check::RELEASE_PAGE,
        log_hint()
    ));
}

fn help() {
    ui::info(&format!(
        "{APP_NAME} {}\n\n\
         {}\n\n\
         CopyFilePath.exe                ask to install or uninstall\n\
         CopyFilePath.exe install        add the menu entries\n\
         CopyFilePath.exe uninstall      remove them\n\
         CopyFilePath.exe copy <path>    copy a path to the clipboard\n\
         CopyFilePath.exe check-update   look for a newer release\n\
         CopyFilePath.exe about          show the version\n\
         CopyFilePath.exe selftest       run the checks (exit code 1 on failure)",
        version(),
        update_check::RELEASE_PAGE
    ));
}

// ---------------------------------------------------------------- helpers

fn fail(what: &str, detail: impl std::fmt::Display) {
    log::write(&format!("{what} failed: {detail}"));
    ui::error(&format!("{what} failed:\n{detail}\n\nLog: {}", log_hint()));
}

fn log_hint() -> String {
    log::path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "no %LOCALAPPDATA%".into())
}
