//! CopyFilePath, in two shapes.
//!
//! * `CopyFilePath.exe` - everything the user runs: installs the menu, copies paths,
//!   checks for updates, runs the self-test.
//! * `copy_file_path.dll` - the right-click entry itself, loaded inside Explorer as a
//!   context menu handler. Windows blocks a single downloaded `.exe` before any menu
//!   *command* runs, but a handler is asked for its item and then does the work
//!   itself, so the path can be copied like any other. The DLL ships inside the exe
//!   as a resource and is written to `%LOCALAPPDATA%\CopyFilePath` on install.
//!
//! Both are built from this crate: `src/main.rs` is the program, `src/shell.rs` holds
//! the handler and the DLL exports, and the rest is shared.

pub mod copy_paths;
pub mod install;
pub mod log;
pub mod paths;
pub mod selftest;
pub mod shell;
pub mod ui;
pub mod update_check;
pub mod version;

use std::env;

pub const APP_NAME: &str = "Copy file path";

/// The project version - two digits. Cargo's third digit is its own business and is
/// never shown; `build.ps1` keeps it at 0.
pub fn version() -> String {
    version::short(env!("CARGO_PKG_VERSION"))
}
