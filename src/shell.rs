//! The right-click entry, as a shell extension loaded inside Explorer.
//!
//! Why this exists: Windows decides that clicking a menu item on a *single imported*
//! `.exe` means "run this file", and that decision comes before any command or
//! `DelegateExecute` handler of ours is reached (measured, see AGENTS.md). A context
//! menu handler is different - Explorer asks us for a menu item and then hands us the
//! invocation - so the shell never executes the selected file and its path can be
//! copied like any other.
//!
//! The handler is deliberately thin: it collects the selected paths from the shell's
//! `CF_HDROP` data and starts the app with them
//! (`CopyFilePath.exe copy-now "p1" "p2" ...`). Clipboard access, logging, error dialogs
//! and the spool all stay in one place, outside Explorer's process.
//!
//! Everything here runs in someone else's process: no panics, no unwraps, and every
//! failure is a return code (a panic would take Explorer down with it).

use std::{
    ffi::c_void,
    mem::size_of,
    os::windows::ffi::OsStringExt,
    ptr,
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex, OnceLock,
    },
};

use windows_sys::{
    core::{GUID, HRESULT, PCWSTR},
    Win32::{
        Foundation::{BOOL, HANDLE, HWND, S_FALSE, S_OK},
        Graphics::Gdi::{
            CreateCompatibleDC, CreateDIBSection, DeleteDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
            BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
        },
        System::{
            Com::FORMATETC,
            LibraryLoader::{GetModuleFileNameW, GetModuleHandleExW},
            Threading::{CreateProcessW, CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTUPINFOW},
        },
        UI::{
            Shell::{DragQueryFileW, ExtractIconExW},
            WindowsAndMessaging::{
                DestroyIcon, DrawIconEx, GetSystemMetrics, InsertMenuItemW, MENUITEMINFOW, DI_NORMAL,
                MFT_STRING, MIIM_BITMAP, MIIM_FTYPE, MIIM_ID, MIIM_STRING, SM_CXMENUCHECK,
            },
        },
    },
};

use crate::log;

/// Runs one callback with a panic barrier. Explorer is the host: a panic here would
/// otherwise unwind (or abort) inside its process, so every entry point goes through
/// this and turns a panic into a failed call plus a log line.
fn guarded<F: FnOnce() -> HRESULT>(what: &'static str, body: F) -> HRESULT {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(result) => result,
        Err(_) => {
            log::write(&format!("the shell handler panicked while running {what}"));
            E_FAIL
        }
    }
}

/// A panic in this DLL would take Explorer down with it, and the message would go
/// nowhere. This puts it in the log first.
fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            log::write(&format!("PANIC in the shell handler: {info}"));
            previous(info);
        }));
    });
}

/// The class Explorer instantiates. Registered per user under
/// `HKCU\Software\Classes\CLSID\{…}\InprocServer32`, pointing at the DLL.
pub const CLSID_TEXT: &str = "{6C1F0E2A-9A3D-4C77-B0E5-3F8A2D4B71C9}";
const CLSID: GUID = GUID::from_u128(0x6c1f0e2a_9a3d_4c77_b0e5_3f8a2d4b71c9);

const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_ISHELLEXTINIT: GUID = GUID::from_u128(0x000214e8_0000_0000_c000_000000000046);
const IID_ICONTEXTMENU: GUID = GUID::from_u128(0x000214e4_0000_0000_c000_000000000046);
const IID_ICLASSFACTORY: GUID = GUID::from_u128(0x00000001_0000_0000_c000_000000000046);

const E_NOINTERFACE: HRESULT = 0x8000_4002u32 as i32;
const E_POINTER: HRESULT = 0x8000_4003u32 as i32;
const E_FAIL: HRESULT = 0x8000_4005u32 as i32;
const E_NOTIMPL: HRESULT = 0x8000_4001u32 as i32;
const CLASS_E_NOAGGREGATION: HRESULT = 0x8004_0110u32 as i32;
const CLASS_E_CLASSNOTAVAILABLE: HRESULT = 0x8004_0111u32 as i32;

const CF_HDROP: u16 = 15;
const DVASPECT_CONTENT: u32 = 1;
const TYMED_HGLOBAL: u32 = 1;

const CMF_DEFAULTONLY: u32 = 0x0000_0001;

const GCS_VERBW: u32 = 4;
const GCS_HELPTEXTW: u32 = 5;
const GCS_VALIDATEW: u32 = 6;

const CMIC_MASK_UNICODE: u32 = 0x0000_4000;

const VERB: &str = "CopyFilePath";
const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x0000_0004;

// ---------------------------------------------------------------- vtables

#[repr(C)]
struct UnknownVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct ExtInitVtbl {
    base: UnknownVtbl,
    initialize: unsafe extern "system" fn(*mut c_void, *const c_void, *mut c_void, *mut c_void) -> HRESULT,
}

#[repr(C)]
struct ContextMenuVtbl {
    base: UnknownVtbl,
    query_context_menu: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, u32, u32, u32) -> HRESULT,
    invoke_command: unsafe extern "system" fn(*mut c_void, *const InvokeInfo) -> HRESULT,
    get_command_string: unsafe extern "system" fn(*mut c_void, usize, u32, *mut u32, *mut u8, u32) -> HRESULT,
}

#[repr(C)]
struct FactoryVtbl {
    base: UnknownVtbl,
    create_instance: unsafe extern "system" fn(*mut c_void, *mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    lock_server: unsafe extern "system" fn(*mut c_void, BOOL) -> HRESULT,
}

/// `CMINVOKECOMMANDINFOEX`; only the fields we read are used, but the layout has to be
/// the real one because Explorer passes the whole struct.
#[repr(C)]
struct InvokeInfo {
    cb_size: u32,
    f_mask: u32,
    hwnd: HWND,
    verb: *const u8,
    parameters: *const u8,
    directory: *const u8,
    show: i32,
    hot_key: u32,
    icon: HANDLE,
    title: *const u8,
    verb_w: *const u16,
    parameters_w: *const u16,
    directory_w: *const u16,
    title_w: *const u16,
    point_x: i32,
    point_y: i32,
}

/// The shell's `IDataObject` - only `Release` and `GetData` are ever called.
#[repr(C)]
struct DataObjectVtbl {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_data: unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut StorageMedium) -> HRESULT,
    rest: [usize; 8],
}

/// `STGMEDIUM` without pulling in the whole OLE feature set: the union is a pointer,
/// so `usize` matches its size and alignment on both x86 and x64.
#[repr(C)]
struct StorageMedium {
    tymed: u32,
    value: usize,
    unknown: *mut c_void,
}

// ---------------------------------------------------------------- objects

struct Shared {
    refs: AtomicU32,
    selection: Mutex<Vec<std::ffi::OsString>>,
    ext_init: *mut c_void,
    context_menu: *mut c_void,
}

#[repr(C)]
struct ExtInit {
    vtbl: &'static ExtInitVtbl,
    owner: *mut Shared,
}

#[repr(C)]
struct ContextMenu {
    vtbl: &'static ContextMenuVtbl,
    owner: *mut Shared,
}

struct Factory {
    /// Read by COM, not by Rust: this pointer is the object, and it has to be the
    /// first field or Explorer dereferences the refcount as a vtable.
    #[allow(dead_code)]
    vtbl: &'static FactoryVtbl,
    refs: AtomicU32,
}

#[repr(C)]
struct Head {
    vtbl: *const c_void,
    owner: *mut Shared,
}

unsafe fn owner_of(this: *mut c_void) -> *mut Shared {
    (*(this as *mut Head)).owner
}

/// `GUID` has no `PartialEq` here; compare the fields.
fn is(iid: &GUID, wanted: &GUID) -> bool {
    iid.data1 == wanted.data1
        && iid.data2 == wanted.data2
        && iid.data3 == wanted.data3
        && iid.data4 == wanted.data4
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn write_wide(destination: *mut u8, capacity: u32, text: &str) {
    if destination.is_null() || capacity == 0 {
        return;
    }
    let target = destination as *mut u16;
    let units: Vec<u16> = text.encode_utf16().take(capacity as usize - 1).collect();
    ptr::copy_nonoverlapping(units.as_ptr(), target, units.len());
    *target.add(units.len()) = 0;
}

// ---------------------------------------------------------------- IUnknown

unsafe extern "system" fn query_interface(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    *out = ptr::null_mut();

    let owner = owner_of(this);
    let target = if is(&*iid, &IID_IUNKNOWN) || is(&*iid, &IID_ISHELLEXTINIT) {
        (*owner).ext_init
    } else if is(&*iid, &IID_ICONTEXTMENU) {
        (*owner).context_menu
    } else {
        return E_NOINTERFACE;
    };

    add_ref(owner);
    *out = target;
    S_OK
}

unsafe fn add_ref(owner: *mut Shared) -> u32 {
    (*owner).refs.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe fn release(owner: *mut Shared) -> u32 {
    let previous = (*owner).refs.fetch_sub(1, Ordering::SeqCst);
    if previous == 0 {
        // Someone released an object they had already released. Counting down from
        // zero would eventually free the boxes twice - a crash inside Explorer, so
        // put the count back and carry on.
        (*owner).refs.fetch_add(1, Ordering::SeqCst);
        return 0;
    }
    let left = previous - 1;
    if left == 0 {
        LIVE.fetch_sub(1, Ordering::SeqCst);
        let boxed = Box::from_raw(owner);
        let (ext_init, context_menu) = (boxed.ext_init, boxed.context_menu);
        drop(boxed);
        drop(Box::from_raw(ext_init as *mut ExtInit));
        drop(Box::from_raw(context_menu as *mut ContextMenu));
    }
    left
}

unsafe extern "system" fn add_ref_unknown(this: *mut c_void) -> u32 {
    add_ref(owner_of(this))
}

unsafe extern "system" fn release_unknown(this: *mut c_void) -> u32 {
    release(owner_of(this))
}

// ---------------------------------------------------------------- IShellExtInit

unsafe extern "system" fn initialize(
    this: *mut c_void,
    _folder: *const c_void,
    data_object: *mut c_void,
    _key: *mut c_void,
) -> HRESULT {
    guarded("initialize", || unsafe { initialize_inner(this, data_object) })
}

unsafe fn initialize_inner(this: *mut c_void, data_object: *mut c_void) -> HRESULT {
    install_panic_hook();
    let owner = owner_of(this);
    let paths = read_selection(data_object);

    match (*owner).selection.lock() {
        Ok(mut guard) => *guard = paths,
        Err(poisoned) => *poisoned.into_inner() = paths,
    }
    S_OK
}

/// The shell hands over the selection as `CF_HDROP`; every selected item is in it.
unsafe fn read_selection(data_object: *mut c_void) -> Vec<std::ffi::OsString> {
    let mut paths = Vec::new();
    if data_object.is_null() {
        return paths;
    }

    let vtbl = *(data_object as *mut *const DataObjectVtbl);
    if vtbl.is_null() {
        return paths;
    }

    let format = FORMATETC {
        cfFormat: CF_HDROP,
        ptd: ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT,
        lindex: -1,
        tymed: TYMED_HGLOBAL,
    };
    let mut medium = StorageMedium {
        tymed: 0,
        value: 0,
        unknown: ptr::null_mut(),
    };

    if ((*vtbl).get_data)(data_object, &format, &mut medium) < 0 {
        return paths;
    }

    let drop = medium.value as *mut c_void;
    if !drop.is_null() {
        let count = DragQueryFileW(drop, u32::MAX, ptr::null_mut(), 0);
        for index in 0..count {
            let length = DragQueryFileW(drop, index, ptr::null_mut(), 0);
            if length == 0 {
                continue;
            }
            let mut buffer = vec![0u16; length as usize + 1];
            let written = DragQueryFileW(drop, index, buffer.as_mut_ptr(), buffer.len() as u32);
            if written > 0 {
                paths.push(std::ffi::OsString::from_wide(&buffer[..written as usize]));
            }
        }
    }

    // The data object is borrowed - the shell owns it and did not hand us a
    // reference, so releasing it here would take Explorer's own object away and the
    // shell would later use freed memory. The medium, on the other hand, is ours.
    ReleaseStgMedium(&mut medium);
    paths
}

#[link(name = "ole32")]
extern "system" {
    fn ReleaseStgMedium(medium: *mut StorageMedium);
}

// ---------------------------------------------------------------- IContextMenu

unsafe extern "system" fn query_context_menu(
    this: *mut c_void,
    menu: *mut c_void,
    index: u32,
    first_id: u32,
    _last_id: u32,
    flags: u32,
) -> HRESULT {
    guarded("query the menu", || unsafe {
        query_context_menu_inner(this, menu, index, first_id, flags)
    })
}

unsafe fn query_context_menu_inner(
    this: *mut c_void,
    menu: *mut c_void,
    index: u32,
    first_id: u32,
    flags: u32,
) -> HRESULT {
    install_panic_hook();
    if menu.is_null() || flags & CMF_DEFAULTONLY != 0 {
        return S_OK;
    }

    let owner = owner_of(this);
    let label = match (*owner).selection.lock() {
        Ok(guard) => label_for(&guard),
        Err(poisoned) => label_for(&poisoned.into_inner()),
    };

    // One call, item and icon together: the app's icon is a menu bitmap (a menu takes
    // an HBITMAP, not an HICON - see `menu_icon`).
    let mut text = wide(&label);
    let mut item: MENUITEMINFOW = std::mem::zeroed();
    item.cbSize = size_of::<MENUITEMINFOW>() as u32;
    item.fMask = MIIM_FTYPE | MIIM_ID | MIIM_STRING | MIIM_BITMAP;
    item.fType = MFT_STRING;
    item.wID = first_id;
    item.cch = (text.len() - 1) as u32;
    item.dwTypeData = text.as_mut_ptr();
    item.hbmpItem = menu_icon();
    if InsertMenuItemW(menu as *mut c_void, index, 1, &item) == 0 {
        return E_FAIL;
    }

    // The return value is the number of items added, packed as an id - not S_OK.
    // Anything else and the shell thinks this handler contributed nothing.
    1
}

/// "Copy folder path" for one folder, "Copy drive path" for one drive, otherwise
/// "Copy file path".
fn label_for(selection: &[std::ffi::OsString]) -> String {
    if let [only] = selection {
        let path = std::path::Path::new(only);
        if path.parent().is_none() {
            return "Copy drive path".into();
        }
        if path.is_dir() {
            return "Copy folder path".into();
        }
    }
    "Copy file path".into()
}

unsafe extern "system" fn invoke_command(this: *mut c_void, info: *const InvokeInfo) -> HRESULT {
    guarded("invoke the command", || unsafe { invoke_command_inner(this, info) })
}

unsafe fn invoke_command_inner(this: *mut c_void, info: *const InvokeInfo) -> HRESULT {
    if info.is_null() {
        return E_POINTER;
    }
    if !matches_our_verb(&*info) {
        return E_FAIL;
    }

    install_panic_hook();
    let owner = owner_of(this);
    let paths = match (*owner).selection.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    if paths.is_empty() {
        return E_FAIL;
    }

    if run_app(&paths) {
        S_OK
    } else {
        E_FAIL
    }
}

unsafe fn matches_our_verb(info: &InvokeInfo) -> bool {
    let numeric = (info.verb as usize) >> 16 == 0;
    if numeric {
        return (info.verb as usize) == 0;
    }

    if info.f_mask & CMIC_MASK_UNICODE != 0 && !info.verb_w.is_null() {
        return eq_ignore_case(&wide_string(info.verb_w), VERB);
    }

    let bytes = wide_string_from_ansi(info.verb);
    eq_ignore_case(&bytes, VERB)
}

unsafe fn wide_string(start: *const u16) -> Vec<u16> {
    let mut length = 0usize;
    while *start.add(length) != 0 {
        length += 1;
    }
    std::slice::from_raw_parts(start, length).to_vec()
}

unsafe fn wide_string_from_ansi(start: *const u8) -> Vec<u16> {
    let mut length = 0usize;
    while *start.add(length) != 0 {
        length += 1;
    }
    std::slice::from_raw_parts(start, length)
        .iter()
        .map(|byte| *byte as u16)
        .collect()
}

fn eq_ignore_case(text: &[u16], wanted: &str) -> bool {
    let wanted: Vec<u16> = wanted.encode_utf16().collect();
    text.len() == wanted.len()
        && text
            .iter()
            .zip(wanted.iter())
            .all(|(a, b)| *a == *b || (*a < 0x80 && (*a as u8).eq_ignore_ascii_case(&(*b as u8))))
}

unsafe extern "system" fn get_command_string(
    _this: *mut c_void,
    command: usize,
    kind: u32,
    _reserved: *mut u32,
    name: *mut u8,
    size: u32,
) -> HRESULT {
    guarded("describe the command", || unsafe {
        get_command_string_inner(command, kind, name, size)
    })
}

unsafe fn get_command_string_inner(
    command: usize,
    kind: u32,
    name: *mut u8,
    size: u32,
) -> HRESULT {
    install_panic_hook();

    if command != 0 {
        return E_FAIL;
    }

    match kind {
        GCS_VERBW => {
            write_wide(name, size, VERB);
            S_OK
        }
        GCS_HELPTEXTW => {
            write_wide(name, size, "Copy the full path to the clipboard");
            S_OK
        }
        GCS_VALIDATEW => S_OK,
        _ => E_NOTIMPL,
    }
}

// ---------------------------------------------------------------- running the app

/// Starts `CopyFilePath.exe copy-now "<path>" …` next to this DLL. No window, no wait.
unsafe fn run_app(paths: &[std::ffi::OsString]) -> bool {
    let Some(app) = app_path() else {
        log::write("shell handler: could not locate CopyFilePath.exe");
        return false;
    };

    let mut command: Vec<u16> = Vec::new();
    command.push('"' as u16);
    command.extend(app.encode_utf16());
    command.push('"' as u16);
    command.extend(" copy-now".encode_utf16());
    for path in paths {
        command.push(' ' as u16);
        command.push('"' as u16);
        command.extend(std::os::windows::ffi::OsStrExt::encode_wide(path.as_os_str()));
        command.push('"' as u16);
    }
    command.push(0);

    let application = wide(&app);
    let mut startup: STARTUPINFOW = std::mem::zeroed();
    startup.cb = size_of::<STARTUPINFOW>() as u32;
    let mut process: PROCESS_INFORMATION = std::mem::zeroed();

    let started = CreateProcessW(
        application.as_ptr(),
        command.as_mut_ptr(),
        ptr::null(),
        ptr::null(),
        0,
        CREATE_NO_WINDOW,
        ptr::null(),
        ptr::null(),
        &startup,
        &mut process,
    );

    if started == 0 {
        log::write("shell handler: could not start CopyFilePath.exe");
        return false;
    }

    if !process.hThread.is_null() {
        windows_sys::Win32::Foundation::CloseHandle(process.hThread);
    }
    if !process.hProcess.is_null() {
        windows_sys::Win32::Foundation::CloseHandle(process.hProcess);
    }
    true
}

/// This DLL's own path, with `CopyFilePath.exe` beside it.
fn app_path() -> Option<String> {
    static CACHE: OnceLock<Option<String>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let mut module: HANDLE = ptr::null_mut();
            let address = module_address as *const () as usize;
            let ok = unsafe {
                GetModuleHandleExW(
                    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
                    address as PCWSTR,
                    &mut module,
                )
            };
            if ok == 0 {
                return None;
            }

            let mut buffer = vec![0u16; 32768];
            let length =
                unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32) };
            if length == 0 {
                return None;
            }
            buffer.truncate(length as usize);

            let mut path = std::path::PathBuf::from(std::ffi::OsString::from_wide(&buffer));
            path.set_file_name("CopyFilePath.exe");
            Some(path.to_string_lossy().to_string())
        })
        .clone()
}

extern "system" fn module_address() {}

/// The exe's icon as a menu bitmap, built once and kept for the life of the DLL.
///
/// Menus take an `HBITMAP`, not an `HICON`, and only a 32-bpp DIB keeps the icon's
/// transparency - so the icon is extracted from the exe and drawn into one here.
fn menu_icon() -> *mut c_void {
    static BITMAP: OnceLock<usize> = OnceLock::new();
    let stored = BITMAP.get_or_init(|| unsafe {
        let Some(app) = app_path() else { return 0 };
        let path = wide(&app);

        let mut large: *mut c_void = ptr::null_mut();
        let mut small: *mut c_void = ptr::null_mut();
        if ExtractIconExW(path.as_ptr(), 0, &mut large, &mut small, 1) == 0 {
            log::write("shell handler: the exe has no icon to show");
            return 0;
        }
        let icon = if small.is_null() { large } else { small };
        if icon.is_null() {
            return 0;
        }

        let size = GetSystemMetrics(SM_CXMENUCHECK).max(16);
        let bitmap = bitmap_from_icon(icon, size);

        if !small.is_null() && small != large {
            DestroyIcon(small);
        }
        if !large.is_null() {
            DestroyIcon(large);
        }
        bitmap as usize
    });
    *stored as *mut c_void
}

/// A 32-bpp top-down DIB with `icon` drawn into it, or null.
unsafe fn bitmap_from_icon(icon: *mut c_void, size: i32) -> *mut c_void {
    let mut info: BITMAPINFO = std::mem::zeroed();
    info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = size;
    info.bmiHeader.biHeight = -size; // top-down
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;

    let screen = CreateCompatibleDC(ptr::null_mut());
    if screen.is_null() {
        return ptr::null_mut();
    }
    let mut bits: *mut c_void = ptr::null_mut();
    let bitmap = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
    if bitmap.is_null() {
        DeleteDC(screen);
        return ptr::null_mut();
    }

    let previous = SelectObject(screen, bitmap as HGDIOBJ);
    // The DIB starts as transparent black, which is what the alpha blend needs.
    DrawIconEx(screen, 0, 0, icon, size, size, 0, ptr::null_mut(), DI_NORMAL);
    SelectObject(screen, previous);
    DeleteDC(screen);
    bitmap
}

// ---------------------------------------------------------------- class factory

unsafe fn interface_for(owner: *mut Shared, iid: *const GUID) -> Option<*mut c_void> {
    if is(&*iid, &IID_IUNKNOWN) || is(&*iid, &IID_ISHELLEXTINIT) {
        Some((*owner).ext_init)
    } else if is(&*iid, &IID_ICONTEXTMENU) {
        Some((*owner).context_menu)
    } else {
        None
    }
}

unsafe fn new_handler() -> *mut Shared {
    LIVE.fetch_add(1, Ordering::SeqCst);
    let owner = Box::into_raw(Box::new(Shared {
        refs: AtomicU32::new(1),
        selection: Mutex::new(Vec::new()),
        ext_init: ptr::null_mut(),
        context_menu: ptr::null_mut(),
    }));

    let ext_init = Box::into_raw(Box::new(ExtInit {
        vtbl: &EXT_INIT_VTBL,
        owner,
    }));
    let context_menu = Box::into_raw(Box::new(ContextMenu {
        vtbl: &CONTEXT_MENU_VTBL,
        owner,
    }));

    (*owner).ext_init = ext_init.cast();
    (*owner).context_menu = context_menu.cast();
    owner
}

unsafe extern "system" fn create_instance(
    _this: *mut c_void,
    outer: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if !outer.is_null() {
        return CLASS_E_NOAGGREGATION;
    }
    if iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    *out = ptr::null_mut();

    let owner = new_handler();
    match interface_for(owner, iid) {
        Some(pointer) => {
            *out = pointer;
            S_OK
        }
        None => {
            release(owner);
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn lock_server(_this: *mut c_void, _lock: BOOL) -> HRESULT {
    S_OK
}

unsafe extern "system" fn factory_query_interface(
    _this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    *out = ptr::null_mut();
    if is(&*iid, &IID_IUNKNOWN) || is(&*iid, &IID_ICLASSFACTORY) {
        factory().refs.fetch_add(1, Ordering::SeqCst);
        *out = factory() as *const Factory as *mut c_void;
        S_OK
    } else {
        E_NOINTERFACE
    }
}

unsafe extern "system" fn factory_add_ref(_this: *mut c_void) -> u32 {
    factory().refs.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn factory_release(_this: *mut c_void) -> u32 {
    factory().refs.fetch_sub(1, Ordering::SeqCst) - 1
}

static EXT_INIT_VTBL: ExtInitVtbl = ExtInitVtbl {
    base: UnknownVtbl {
        query_interface,
        add_ref: add_ref_unknown,
        release: release_unknown,
    },
    initialize,
};

static CONTEXT_MENU_VTBL: ContextMenuVtbl = ContextMenuVtbl {
    base: UnknownVtbl {
        query_interface,
        add_ref: add_ref_unknown,
        release: release_unknown,
    },
    query_context_menu,
    invoke_command,
    get_command_string,
};

static FACTORY_VTBL: FactoryVtbl = FactoryVtbl {
    base: UnknownVtbl {
        query_interface: factory_query_interface,
        add_ref: factory_add_ref,
        release: factory_release,
    },
    create_instance,
    lock_server,
};

static FACTORY: OnceLock<Factory> = OnceLock::new();
/// Handler objects alive in this process.
static LIVE: AtomicU32 = AtomicU32::new(0);

fn factory() -> &'static Factory {
    FACTORY.get_or_init(|| Factory {
        vtbl: &FACTORY_VTBL,
        refs: AtomicU32::new(1),
    })
}

// ---------------------------------------------------------------- DLL exports

/// Called by Explorer (via CoCreateInstance) to get our class factory.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    class: *const GUID,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    install_panic_hook();
    if class.is_null() || iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    *out = ptr::null_mut();
    if !is(&*class, &CLSID) {
        return CLASS_E_CLASSNOTAVAILABLE;
    }

    // The factory lives for the lifetime of the DLL, so hand it out with a reference
    // per interface pointer and let COM release it as usual.
    factory().refs.fetch_add(1, Ordering::SeqCst);
    let pointer = factory() as *const Factory as *mut c_void;
    if is(&*iid, &IID_IUNKNOWN) || is(&*iid, &IID_ICLASSFACTORY) {
        *out = pointer;
        S_OK
    } else {
        factory_release(pointer);
        E_NOINTERFACE
    }
}

/// Explorer asks this before unloading the DLL. Saying yes when nothing is alive lets
/// it drop us - and with us the file lock that would otherwise keep the installed
/// folder alive after an uninstall or hold back an update.
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    // Always "in use": Explorer keeps a class factory reference we cannot observe, and
    // unloading the code out from under it is a hard crash in its process. The file
    // lock is dealt with by the versioned DLL name instead.
    S_FALSE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_class_id_matches_the_registered_text() {
        let digits: String = CLSID_TEXT
            .trim_matches(['{', '}'])
            .chars()
            .filter(|character| *character != '-')
            .collect();
        let text = format!(
            "{:08X}{:04X}{:04X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
            CLSID.data1,
            CLSID.data2,
            CLSID.data3,
            CLSID.data4[0],
            CLSID.data4[1],
            CLSID.data4[2],
            CLSID.data4[3],
            CLSID.data4[4],
            CLSID.data4[5],
            CLSID.data4[6],
            CLSID.data4[7]
        );
        assert_eq!(text, digits.to_uppercase());
    }

    #[test]
    fn verbs_are_compared_without_case() {
        let mut text: Vec<u16> = "copyfilepath".encode_utf16().collect();
        assert!(eq_ignore_case(&text, VERB));
        text[0] = 'x' as u16;
        assert!(!eq_ignore_case(&text, VERB));
    }

    #[test]
    fn labels_match_the_kind_of_item() {
        let here = std::env::temp_dir();
        let one_folder = vec![here.clone().into_os_string()];
        assert_eq!(label_for(&one_folder), "Copy folder path");

        let two = vec![here.clone().into_os_string(), here.into_os_string()];
        assert_eq!(label_for(&two), "Copy file path");
    }
}
