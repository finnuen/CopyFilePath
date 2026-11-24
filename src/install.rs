//! The menu entries and the installed copy - the only state this app keeps.
//!
//! Everything lives under `HKEY_CURRENT_USER\Software\Classes`, so installing needs
//! no admin rights and uninstalling touches nothing else. Two files are installed to
//! `%LOCALAPPDATA%\CopyFilePath`: this exe, and the shell handler DLL that is embedded
//! in it as a resource (that is why the download stays one file).
//!
//! The menu item for a selected item comes from the handler DLL - the only route that
//! works for a single downloaded `.exe`, because Windows intercepts menu *commands*
//! on such a file before they run (see `shell.rs` and AGENTS.md). Empty space inside a
//! folder has no item to hand to a handler, so it keeps a plain command-line verb.

use std::{
    env,
    fs, io,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    ptr,
    thread,
    time::Duration,
};

use windows_sys::Win32::{
    System::LibraryLoader::{FindResourceW, LoadResource, LockResource, SizeofResource},
    UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST},
};
use winreg::{enums::*, RegKey};

use crate::{log, shell};

pub const EXE_NAME: &str = "CopyFilePath.exe";
const VERB_KEY: &str = "CopyFilePath";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Resource id of the handler DLL inside this exe (see `build.rs`).
const DLL_RESOURCE: u16 = 100;
/// `RT_RCDATA` as a resource type id.
const RT_RCDATA: u16 = 10;

/// The item classes the handler covers. `lnkfile` is not redundant: for a shortcut the
/// shell resolves the target for the generic classes, and only the shortcut's own
/// class keeps the `.lnk` path.
pub const HANDLER_CLASSES: &[&str] = &["*", "lnkfile", "Directory", "Drive"];

/// Empty space inside a folder: a command line, because there is no item for a handler.
pub const BACKGROUND: (&str, &str) = ("Directory\\Background", "Copy folder path");

/// Classes that carried a plain verb in older versions; its keys are removed on
/// install so an upgrade cannot leave two "Copy file path" entries behind.
const LEGACY_VERB_CLASSES: &[&str] = &["*", "lnkfile", "Directory", "Drive"];
/// The CLSID the previous version used for a `DelegateExecute` verb.
const LEGACY_CLSID: &str = "{A05AF837-A2E1-4E28-992C-1566E833604E}";

pub fn install_dir() -> io::Result<PathBuf> {
    let base = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "%LOCALAPPDATA% is not set"))?;
    Ok(base.join("CopyFilePath"))
}

/// Size of the handler DLL embedded in this exe, 0 when there is none (a plain
/// `cargo build` instead of `build.bat`).
pub fn helper_size() -> usize {
    unsafe {
        let resource = FindResourceW(
            ptr::null_mut(),
            DLL_RESOURCE as usize as *const u16,
            RT_RCDATA as usize as *const u16,
        );
        if resource.is_null() {
            return 0;
        }
        SizeofResource(ptr::null_mut(), resource) as usize
    }
}

pub fn installed_exe() -> io::Result<PathBuf> {
    Ok(install_dir()?.join(EXE_NAME))
}

/// The helper's file name carries the version, so an update writes a *new* file
/// instead of fighting a copy Explorer still has loaded. Old ones are removed on the
/// next install, as soon as nothing holds them.
pub fn installed_dll() -> io::Result<PathBuf> {
    Ok(install_dir()?.join(format!("CopyFilePathShell_v{}.dll", crate::version())))
}

/// Installed means the menu key is there *and* the files it points at exist. A key left
/// behind by a deleted folder would otherwise look installed and do nothing.
pub fn is_installed() -> bool {
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(format!(
        "Software\\Classes\\*\\shellex\\ContextMenuHandlers\\{VERB_KEY}"
    ));
    key.is_ok()
        && installed_exe().map(|exe| exe.exists()).unwrap_or(false)
        && installed_dll().map(|dll| dll.exists()).unwrap_or(false)
}

pub fn install() -> io::Result<()> {
    let source = env::current_exe()?;
    let dir = install_dir()?;
    fs::create_dir_all(&dir)?;

    let target = dir.join(EXE_NAME);
    if !same_path(&source, &target) {
        copy_with_retry(&source, &target)?;
    }

    let dll = installed_dll()?;
    extract_dll(&dll)?;
    remove_old_helpers(&dir, &dll);

    let exe = target.to_string_lossy().to_string();
    let dll_path = dll.to_string_lossy().to_string();
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    // The handler object: this DLL, loaded inside Explorer.
    let (class, _) = hkcu.create_subkey(format!("Software\\Classes\\CLSID\\{}", shell::CLSID_TEXT))?;
    class.set_value("", &"Copy file path")?;
    let (server, _) = class.create_subkey("InprocServer32")?;
    server.set_value("", &dll_path)?;
    server.set_value("ThreadingModel", &"Apartment")?;

    for item_class in HANDLER_CLASSES {
        let (key, _) = hkcu.create_subkey(format!(
            "Software\\Classes\\{item_class}\\shellex\\ContextMenuHandlers\\{VERB_KEY}"
        ))?;
        key.set_value("", &shell::CLSID_TEXT)?;
    }

    // Empty space inside a folder: no item to hand over, so a plain command line.
    // The key is rebuilt from scratch: older versions put a `DelegateExecute` here,
    // and a stale one points at a class that no longer exists.
    let (class_name, label) = BACKGROUND;
    let _ = hkcu.delete_subkey_all(format!("Software\\Classes\\{class_name}\\shell\\{VERB_KEY}"));
    let (background, _) =
        hkcu.create_subkey(format!("Software\\Classes\\{class_name}\\shell\\{VERB_KEY}"))?;
    background.set_value("MUIVerb", &label)?;
    background.set_value("Icon", &format!("\"{exe}\",0"))?;
    background.set_value("Position", &"Top")?;
    let (command, _) = background.create_subkey("command")?;
    command.set_value("", &format!("\"{exe}\" copy \"%V\""))?;

    remove_legacy(&hkcu);
    cancel_cleanup_after_reinstall();

    log::write(&format!("installed: {exe} + {dll_path}"));
    notify_shell();
    Ok(())
}

/// Removes the registry entries only. `remove_installed_copy()` is a separate step
/// because it can only run once the dialogs are closed - see its note.
pub fn uninstall() -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    for item_class in HANDLER_CLASSES {
        delete_tree(
            &hkcu,
            &format!("Software\\Classes\\{item_class}\\shellex\\ContextMenuHandlers\\{VERB_KEY}"),
        )?;
    }
    delete_tree(
        &hkcu,
        &format!("Software\\Classes\\CLSID\\{}", shell::CLSID_TEXT),
    )?;
    let (class_name, _) = BACKGROUND;
    delete_tree(
        &hkcu,
        &format!("Software\\Classes\\{class_name}\\shell\\{VERB_KEY}"),
    )?;

    remove_legacy(&hkcu);

    log::write("uninstalled: menu entries removed");
    notify_shell();
    Ok(())
}

/// Delete the installed files.
///
/// Call this last, after the confirmation dialog: Windows will not delete a running
/// .exe (or a DLL loaded into Explorer), so when we *are* that exe the delete has to
/// wait for this process to exit.
pub fn remove_installed_copy() {
    let Ok(dir) = install_dir() else { return };
    if !dir.exists() {
        return;
    }

    let running_from_it = env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .map(|parent| same_path(&parent, &dir))
        .unwrap_or(false);

    if !running_from_it {
        // The log lives inside the folder being removed: note the attempt first, or
        // writing it afterwards would put the folder straight back.
        log::write(&format!("removing {}", dir.display()));
        match fs::remove_dir_all(&dir) {
            Ok(()) => return,
            Err(e) => log::write(&format!("could not remove {}: {e}", dir.display())),
        }
    }

    // Leftovers Explorer will not release now are finished at the next sign-in.
    schedule_cleanup_at_logon(&dir);

    // Ping is only a one-second sleep here: it exists on every Windows, needs no
    // window, and gives this process time to exit before the folder is removed.
    let spawned = Command::new("cmd")
        .args(["/C", "ping", "-n", "3", "127.0.0.1", ">nul", "&", "rmdir", "/s", "/q"])
        .arg(&dir)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();

    match spawned {
        Ok(_) => log::write(&format!("scheduled removal of {}", dir.display())),
        Err(e) => log::write(&format!("could not schedule removal of {}: {e}", dir.display())),
    }
}

// ---------------------------------------------------------------- helpers

/// Write the handler DLL out of this exe's resources. A build without it (a plain
/// `cargo build`) says so instead of registering a handler that cannot load.
fn extract_dll(destination: &Path) -> io::Result<()> {
    let bytes = unsafe {
        let resource = FindResourceW(
            ptr::null_mut(),
            DLL_RESOURCE as usize as *const u16,
            RT_RCDATA as usize as *const u16,
        );
        if resource.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "this build has no shell helper embedded - build with build.bat",
            ));
        }
        let size = SizeofResource(ptr::null_mut(), resource) as usize;
        if size == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the embedded shell helper is empty - rebuild with build.bat",
            ));
        }
        let loaded = LoadResource(ptr::null_mut(), resource);
        if loaded.is_null() {
            return Err(io::Error::last_os_error());
        }
        let data = LockResource(loaded) as *const u8;
        if data.is_null() {
            return Err(io::Error::last_os_error());
        }
        std::slice::from_raw_parts(data, size).to_vec()
    };

    match fs::write(destination, &bytes) {
        Ok(()) => {
            log::write(&format!(
                "wrote {} ({} bytes)",
                destination.display(),
                bytes.len()
            ));
            Ok(())
        }
        // Explorer holds the DLL of the version it is running; the same version is
        // already on disk in that case, so there is nothing to repair.
        Err(e) if destination.exists() => {
            log::write(&format!(
                "{} is in use; keeping the copy already installed ({e})",
                destination.display()
            ));
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Best effort: old helper DLLs are locked while Explorer still has them loaded, and
/// simply go away on the next install (or when the folder is removed on uninstall).
fn remove_old_helpers(dir: &Path, keep: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let helper = name.starts_with("CopyFilePathShell") && (name.ends_with(".dll") || name.ends_with(".old"));
        if helper && path != keep {
            match fs::remove_file(&path) {
                Ok(()) => log::write(&format!("removed old helper {}", path.display())),
                Err(_) => log::write(&format!("old helper {} is still in use", path.display())),
            }
        }
    }
}

const RUN_ONCE_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\RunOnce";
const RUN_ONCE_VALUE: &str = "CopyFilePathRemover";

/// Explorer keeps the helper DLL loaded until it exits (or the user signs out), so an
/// uninstall sometimes cannot delete the folder now. This one-shot entry finishes the
/// job at the next sign-in, and does nothing at all if the program has been installed
/// again in the meantime - it only removes a folder that no longer holds the exe.
fn schedule_cleanup_at_logon(dir: &Path) {
    let command = format!(
        "cmd /C if not exist \"{}\" rmdir /s /q \"{}\"",
        dir.join(EXE_NAME).display(),
        dir.display()
    );
    match RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_ONCE_KEY) {
        Ok((key, _)) => {
            if let Err(e) = key.set_value(RUN_ONCE_VALUE, &command) {
                log::write(&format!("could not schedule the cleanup: {e}"));
            }
        }
        Err(e) => log::write(&format!("could not schedule the cleanup: {e}")),
    }
}

fn cancel_cleanup_after_reinstall() {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    // A plain `open_subkey` is read-only, and deleting from it fails without a word.
    if let Ok(key) = hkcu.open_subkey_with_flags(RUN_ONCE_KEY, KEY_SET_VALUE) {
        if let Err(e) = key.delete_value(RUN_ONCE_VALUE) {
            if e.kind() != io::ErrorKind::NotFound {
                log::write(&format!("could not cancel the pending cleanup: {e}"));
            }
        }
    }
}

fn remove_legacy(hkcu: &RegKey) {
    for item_class in LEGACY_VERB_CLASSES {
        let _ = hkcu.delete_subkey_all(format!(
            "Software\\Classes\\{item_class}\\shell\\{VERB_KEY}"
        ));
    }
    let _ = hkcu.delete_subkey_all(format!("Software\\Classes\\CLSID\\{LEGACY_CLSID}"));
}

fn delete_tree(hkcu: &RegKey, path: &str) -> io::Result<()> {
    match hkcu.delete_subkey_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            log::write(&format!("uninstall failed for {path}: {e}"));
            Err(e)
        }
    }
}

/// The file may briefly be busy while another instance copies itself over it, so
/// give a fresh copy a couple of seconds before reporting the failure.
fn copy_with_retry(source: &Path, target: &Path) -> io::Result<()> {
    let mut last = None;
    for _ in 0..10 {
        match fs::copy(source, target) {
            Ok(_) => {
                log::write(&format!("copied {} -> {}", source.display(), target.display()));
                return Ok(());
            }
            Err(e) => {
                last = Some(e);
                thread::sleep(Duration::from_millis(200));
            }
        }
    }
    let error = last.unwrap_or_else(|| io::Error::other("copy failed"));
    log::write(&format!(
        "copy {} -> {} failed: {error}",
        source.display(),
        target.display()
    ));
    Err(error)
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
}

fn notify_shell() {
    // Tell Explorer the associations changed, so the menu updates without a logoff.
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, ptr::null(), ptr::null()) };
}
