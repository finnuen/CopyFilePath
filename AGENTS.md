# CopyFilePath

A right-click entry for Windows Explorer: **Copy file path** puts the full path of the
selected file, folder or drive on the clipboard. One Rust binary of about 1.4 MB - the
Explorer helper is embedded in it - with no installer and no runtime beyond Windows
itself; it installs itself into `%LOCALAPPDATA%\CopyFilePath` (so the download can be
deleted) and writes its per-user registry entries. Nothing runs in the background - the
menu entry starts the .exe, it copies, it exits.

## Build

    build.bat                     # bump the minor digit, build, print the exe path

Requires `cargo` (rustup, MSVC target) and nothing else. The script finds cargo on PATH or
in `%USERPROFILE%\.cargo\bin`, names what is missing if it is neither, and ends with one
line:

    Built: E:\...\build\CopyFilePath_v1.1.exe  (1448 KB)

`build.bat` is a wrapper because Windows ships with script execution disabled; the work is
in `build.ps1`. `-KeepVersion` rebuilds at the current version, `-Version 1.2` sets it,
and a bare run moves the minor digit (`1.0` -> `1.1`). The project version is two digits - major.minor - and Cargo's third digit is padding that stays 0 and is never shown; `build.ps1` is what keeps it that way.

The version lives in `Cargo.toml` and nowhere else: `build.rs` writes it into the .exe's
file properties, `build.ps1` uses it in the file name and checks the two agree, and the app
prints `env!("CARGO_PKG_VERSION")`. A mismatch fails the build.

## Check that it still works

    cargo test                    # 20 tests over paths, versions, the batch format and the handler
    build\CopyFilePath_v<version>.exe selftest     # same table, no cargo needed

`--selftest`/`selftest` prints PASS/FAIL per check, writes the same lines to the log, and
exits 1 if anything failed - that exit code is the answer, since a windowed program has no
console of its own. It touches no registry key, no clipboard and no network. Sending it a
path to copy is the other half: `CopyFilePath.exe copy "%TEMP%\x.txt"` then read the
clipboard.

Do not run `install`, `uninstall` or the bare exe to test something else: those change the
user's Explorer menu and the copy in `%LOCALAPPDATA%`.

## Where things live

| Path | What it is |
| --- | --- |
| `src/lib.rs` | the crate both halves share: version, app name, module list |
| `src/main.rs` | entry point of the exe: command line -> mode; the order of work is in its doc comment |
| `src/shell.rs` | the Explorer handler DLL: menu item, selection, starting the exe |
| `src/install.rs` | the registry entries, the helper DLL, install / uninstall / self-delete |
| `src/copy_paths.rs` | one selection -> one clipboard entry (the multi-process spool) |
| `src/paths.rs` | what the shell hands over -> the path we copy |
| `src/update_check.rs` | the GitHub "newest release" check (WinHTTP, redirect only) |
| `src/version.rs` | reading and comparing version numbers |
| `src/ui.rs` | every dialog, the download link, console output for the self-test |
| `src/log.rs` | the one log file |
| `src/selftest.rs` | the checks; `cargo test` runs the same table |
| `build.rs` | generates the .rc (icon + version) from `Cargo.toml` |
| `build.ps1` / `build.bat` | bump, build, name, verify |
| `assets/icon.ico` | the icon, also the one in the menu entries |

Generated, never edit by hand: `target/`, `build/`, and the .rc that `build.rs` writes into
`target/`. `Cargo.lock` is committed - change dependencies through `Cargo.toml` only.

## What must keep working

- **`copy` is the hot path**: no window, no network, no dialog on success. Through the COM
  verb it is one call and a few milliseconds; the spool below only matters to hosts that
  run the command line instead (drag & drop, third-party file managers), where it waits up
  to 200 ms for the other processes of the same selection.
- **One process per item**: Explorer never passes a whole selection to a static verb - it
  starts one process per selected item, each with one path. The spool file in `%TEMP%` is
  what reassembles them, and `MultiSelectModel` = `Player` is what allows up to 100 items
  (`Document`, the default for legacy verbs, would stop at 15).
- **A late process must not win alone**: a process that starts after its siblings have
  already copied finds no spool, so it would put one path on the clipboard and replace the
  selection. `previous_copy()` folds the last copy in when this process started within
  `CONTINUATION` of it. Do **not** decide that with a file's creation time: NTFS restores
  it when a name is reused within 15 seconds ("file system tunneling"), and every
  selection reuses the spool name - an earlier version of this fix did exactly that and
  silently did nothing. The process start time is used instead.
- **Shortcuts**: for a `.lnk`, Explorer resolves the target and hands *that* path to verbs
  registered under `*` and `Directory`. Only the shortcut's own class, `lnkfile`, receives
  the `.lnk` path - that row in `HANDLER_CLASSES` is why "Copy file path" on a shortcut
  copies the shortcut, and removing it silently changes the answer to the target.
- **The menu item comes from a shell handler DLL, not a verb**: `install` writes
  `HKCU\…\Classes\{*|lnkfile|Directory|Drive}\shellex\ContextMenuHandlers\CopyFilePath` =
  our CLSID, and `HKCU\…\CLSID\{6C1F0E2A-…}\InprocServer32` = the helper DLL with
  `ThreadingModel = Apartment`. This is the only shape that works on a *single downloaded
  `.exe`*: Windows treats that click as "run this file" before any verb or
  `DelegateExecute` handler is reached (measured - a plain command, a COM delegate and the
  shell's own verbs all fail identically), while a handler is asked for its menu item and
  then performs the action itself, so the shell never executes the selected file.
- **The item's icon is a bitmap, not an icon**: a menu takes an `HBITMAP`, so the exe's
  icon is extracted with `ExtractIconExW` and drawn once into a 32-bpp top-down DIB
  (`bitmap_from_icon`), then cached - the bitmap has to outlive every menu it is put on.
  An image list's internal strip does **not** work as `hbmpItem` (tried; the item drew
  with an empty icon column).
- **The handler is deliberately thin**: it reads the selection from the shell's `CF_HDROP`
  and starts `CopyFilePath.exe copy-now "p1" "p2" …`. Clipboard, logging, spool and error
  dialogs stay in the exe; the DLL never touches the clipboard itself. `copy-now` is the
  no-spool path - the whole selection arrives in one call, so nothing is waited for.
- **`QueryContextMenu` must return the number of items added** (packed as an id), not
  `S_OK`. Returning 0 makes the shell ignore the handler on some classes - the item showed
  for `.exe` and not for `.txt`, which is how that bug was found.
- **The DLL is built from this crate** (`[lib] crate-type = ["rlib", "cdylib"]`,
  `src/shell.rs`), embedded into the exe as `RCDATA` id 100 by `build.rs`, and written to
  `%LOCALAPPDATA%\CopyFilePath\CopyFilePathShell_v<version>.dll` on install. `build.ps1`
  builds the library first, stages the DLL in `target/shell-helper/`, then builds the exe -
  the exe's build script cannot see a DLL produced by the same cargo run. A plain `cargo
  build` therefore produces an exe without a helper; install says so and names `build.bat`.
- **The helper's file name carries the version** so an update writes a new file instead of
  fighting the copy Explorer still has loaded; old ones are deleted on the next install.
  `DllCanUnloadNow` answers `S_FALSE` for ever: Explorer can hold a class-factory reference
  the DLL cannot see, and unloading the code under it is a crash *in Explorer*. Until it
  restarts, it keeps running the old helper - normal for shell extensions - and an
  uninstall cannot delete a loaded DLL, so it leaves a one-shot `RunOnce` entry that
  removes the folder at the next sign-in (skipped if the exe is back by then; `install`
  cancels it).
- **Nothing here may take the host down**: every shell callback runs through `guarded()`,
  which turns a panic into a failed call plus a log line, `release()` refuses to count below
  zero, and the icon handles are destroyed only once. A shell extension has no second
  chance - a crash there is Explorer restarting.
- **Empty space inside a folder** has no item to hand to a handler, so
  `Directory\Background\shell\CopyFilePath` stays a command verb (`copy "%V"`). Its key is
  rebuilt on install, because older versions put a `DelegateExecute` there that pointed at
  a class no longer registered (`REGDB_E_CLASSNOTREG` on click).
- **The CLSID must never change** (`shell::CLSID_TEXT`): every installed handler points at
  it. Its text and binary forms are two constants in one file, and a test keeps them in
  step.
- **A drive root**: Explorer sends `%1` as `C:"` - the backslash escapes the closing quote
  - so `paths::normalize` puts it back. Without that, the clipboard gets `C:`.
- **Menu entries** must point at `%LOCALAPPDATA%\CopyFilePath\CopyFilePath.exe` and the
  `Icon` must be the same path; a key whose .exe is gone counts as "not installed" so the
  dialog offers to repair it.
- **Uninstall while running from the installed copy**: Windows will not delete a running
  .exe, so `remove_installed_copy()` is called *after* the last dialog and hands the
  delete to a detached `cmd` when it is deleting the running file. Moving that call
  earlier leaves the folder behind.
- **Per-user only**: `HKCU\Software\Classes`, no admin rights, no HKLM, no services.

## Recipes

**A new menu target** (a class the right-click menu is missing): add it to
`HANDLER_CLASSES` in `src/install.rs`; the handler decides its label from the selection and
needs no per-class text. The class must be one the shell actually reaches for that kind of
item: `lnkfile` exists because a shortcut is otherwise handled through its target's class.
Empty space inside a folder is the one case that keeps a command verb (`BACKGROUND`).

**A new command-line mode**: add the match arm in `main()`, a `fn` beside it, and a line in
`help()`. Anything that can fail goes through `log::write` with the input in the message,
and a user-visible error says where the log is.

**A new dialog**: add it to `src/ui.rs`, never call `MessageBoxW` from anywhere else. The
logic modules return values, not windows - that is what keeps `selftest` possible.

**Add a check**: a row in `src/selftest.rs::checks()` (and a `#[test]` if it needs a
fixture). `cargo test` asserts the whole table passes, so there is one list, not two.

**A release** (only when the user asks for it):

    build.bat
    Copy-Item build\CopyFilePath_v1.1.exe "$env:TEMP\CopyFilePath.exe"
    gh release create 1.1 "$env:TEMP\CopyFilePath.exe" --title "CopyFilePath v1.1" --notes "- ..."

The asset is unversioned on purpose: that is what
`releases/latest/download/CopyFilePath.exe` in the README resolves, and it is what lets a
user replace the file without changing its name. Mirror the release to GitLab, per the
global rules, then delete the downloaded copy.

## Marked ground

- `target/` and `build/` - cargo and `build.ps1` write them.
- The generated .rc in `target/.../out` - it comes from `Cargo.toml` + `build.rs`.
- `.cargo/config.toml` - only used when cross-compiling from Linux/macOS with MinGW; on
  Windows the MSVC toolchain ignores it.

## Log

`%LOCALAPPDATA%\CopyFilePath\CopyFilePath.log` - one line per install, uninstall,
failed copy and failed update check, timestamped in local time, rotated to `.old` past
256 KB. Uninstalling removes the folder, log included; a failure that matters is logged
before the removal is attempted and shown in the dialog as well.
