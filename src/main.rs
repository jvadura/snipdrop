//! snipdrop: press a hotkey, drag a rectangle, paste the result into a
//! terminal coding agent with Ctrl+V.
//!
//! One thread, one message loop, no framework. The hidden main window owns
//! the hotkey and the tray icon; `capture` handles selection, `save` and
//! `clipboard` deliver the snip, `toast` confirms it.

#![windows_subsystem = "windows"]

mod capture;
mod clipboard;
mod config;
mod save;
mod toast;
mod tray;
mod util;

use std::ffi::c_void;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows_sys::Win32::Graphics::Dwm::DwmFlush;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW, HICON, IDI_APPLICATION, KillTimer,
    LoadIconW, MF_CHECKED, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage,
    RegisterClassW, RegisterWindowMessageW, SW_SHOWNORMAL, SetForegroundWindow, SetTimer,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP,
    WM_CONTEXTMENU, WM_DESTROY, WM_HOTKEY, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_TIMER,
    WNDCLASSW, WS_OVERLAPPED,
};

use capture::Crop;
use config::Config;
use util::{APP_NAME, UiCell, log, wide};

const CLASS_NAME: &str = "snipdrop.main";
const MUTEX_NAME: &str = "Local\\snipdrop.single-instance";

const WM_TRAY: u32 = WM_APP + 1;
/// Posted by a second copy of the program to the one already running.
const WM_ALREADY_RUNNING: u32 = WM_APP + 2;
const HOTKEY_ID: i32 = 1;
const MOD_NOREPEAT: u32 = 0x4000;
const SNIP_TIMER: usize = 1;
/// Lets the tray menu finish closing before the screen is frozen.
const SNIP_DELAY_MS: u32 = 220;

const CMD_SNIP: usize = 1;
const CMD_FOLDER: usize = 2;
const CMD_EDIT: usize = 3;
const CMD_RELOAD: usize = 4;
const CMD_AUTOSTART: usize = 5;
const CMD_EXIT: usize = 6;

struct App {
    hwnd: HWND,
    instance: *mut c_void,
    icon: HICON,
    config: Config,
    taskbar_created: u32,
}

static APP: UiCell<App> = UiCell::new();

fn main() {
    std::panic::set_hook(Box::new(|info| log(&format!("panic: {info}"))));

    // SAFETY: start-up Win32 calls with valid, NUL-terminated arguments.
    unsafe {
        // A second copy would fight the first one for the hotkey. Launching
        // it again (desktop shortcut, Start menu) asks the running copy to
        // say it is there: its only UI may be hidden in the tray overflow.
        let mutex_name = wide(MUTEX_NAME);
        let mutex = CreateMutexW(null(), 0, mutex_name.as_ptr());
        if !mutex.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
            let running = FindWindowW(wide(CLASS_NAME).as_ptr(), wide(APP_NAME).as_ptr());
            if !running.is_null() {
                PostMessageW(running, WM_ALREADY_RUNNING, 0, 0);
            }
            return;
        }

        // Physical pixels everywhere: without this, captures on scaled
        // displays are blurry and selections land in the wrong place.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let instance = GetModuleHandleW(null());
        let class_name = wide(CLASS_NAME);
        let mut class: WNDCLASSW = std::mem::zeroed();
        class.lpfnWndProc = Some(main_proc);
        class.hInstance = instance;
        class.lpszClassName = class_name.as_ptr();
        if RegisterClassW(&class) == 0
            || !capture::register_class(instance)
            || !toast::register_class(instance)
        {
            log("startup: could not register window classes");
            return;
        }

        // Never shown; it exists to receive hotkey and tray messages.
        let title = wide(APP_NAME);
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            log("startup: could not create the main window");
            return;
        }

        let mut icon = tray::create_icon();
        if icon.is_null() {
            icon = LoadIconW(null_mut(), IDI_APPLICATION);
        }

        let loaded = config::load();
        APP.set(App {
            hwnd,
            instance,
            icon,
            config: loaded.config,
            // Explorer broadcasts this when it (re)starts; the icon has to
            // be added again or the app becomes unreachable.
            taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
        });

        add_tray_icon();
        let hotkey_ok = register_hotkey();
        report_startup(hotkey_ok, &loaded.warnings, loaded.first_run);
        prune_old_snips();

        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

fn tip() -> String {
    match APP.get() {
        Some(app) => format!("{APP_NAME} \u{2014} {} to snip", app.config.hotkey_text),
        None => APP_NAME.to_string(),
    }
}

fn add_tray_icon() {
    let text = tip();
    if let Some(app) = APP.get()
        && !tray::add(app.hwnd, app.icon, WM_TRAY, &text)
    {
        log("tray: could not add the icon");
    }
}

/// (Re)register the configured hotkey. Returns false when another program
/// already owns it.
fn register_hotkey() -> bool {
    let Some(app) = APP.get() else { return false };
    let hotkey = app.config.hotkey;
    // SAFETY: our own window handle and id.
    unsafe {
        UnregisterHotKey(app.hwnd, HOTKEY_ID);
        RegisterHotKey(app.hwnd, HOTKEY_ID, hotkey.mods | MOD_NOREPEAT, hotkey.vk) != 0
    }
}

fn report_startup(hotkey_ok: bool, warnings: &[String], first_run: bool) {
    let Some(app) = APP.get() else { return };
    let hotkey = app.config.hotkey_text.clone();
    let hwnd = app.hwnd;
    for warning in warnings {
        log(&format!("config: {warning}"));
    }
    if !hotkey_ok {
        log(&format!("hotkey: {hotkey} could not be registered"));
        tray::balloon(
            hwnd,
            "snipdrop: hotkey unavailable",
            &format!(
                "{hotkey} is already used by another program. Right-click the tray icon \u{2192} Edit config to choose another."
            ),
            true,
        );
    } else if let Some(first) = warnings.first() {
        tray::balloon(hwnd, "snipdrop: config problem", first, true);
    } else if first_run {
        tray::balloon(
            hwnd,
            "snipdrop is running",
            &format!("Press {hotkey}, drag a rectangle, then paste with Ctrl+V."),
            false,
        );
    }
}

/// Tell the user the app is alive and how to use it. Uses our own toast
/// rather than a Windows notification, which Do Not Disturb would swallow.
fn announce_running() {
    let Some(app) = APP.get() else { return };
    let instance = app.instance;
    let detail = format!(
        "Press {} to snip, or left-click the tray icon.",
        app.config.hotkey_text
    );
    let mut anchor = POINT { x: 0, y: 0 };
    // SAFETY: valid out-pointer.
    unsafe { GetCursorPos(&mut anchor) };
    toast::show(
        instance,
        &toast::Message {
            ok: true,
            title: "snipdrop is already running",
            detail: &detail,
            crop: None,
            anchor,
        },
    );
}

fn prune_old_snips() {
    if let Some(app) = APP.get() {
        save::prune(&app.config.save_dir, app.config.keep_days);
    }
}

fn start_snip() {
    let Some(app) = APP.get() else { return };
    let instance = app.instance;
    if capture::is_active() {
        return;
    }
    if toast::dismiss() {
        // Wait for the compositor so the old toast is not in the screenshot.
        // SAFETY: no preconditions.
        unsafe { DwmFlush() };
    }
    capture::begin(instance, deliver);
}

/// Called by `capture` with the selected pixels: save, copy, confirm.
fn deliver(crop: Crop, anchor: POINT) {
    let Some(app) = APP.get() else { return };
    let (hwnd, instance, config) = (app.hwnd, app.instance, app.config.clone());

    let formats = clipboard::Formats {
        path: config.clip_path,
        file: config.clip_file,
        image: config.clip_image,
    };
    let result = save::save_png(&config.save_dir, &crop)
        .and_then(|path| clipboard::set(hwnd, &path, &crop, &formats).map(|()| path));

    match result {
        Ok(path) => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let title = if config.clip_path || config.clip_file {
                "Snip copied \u{2014} paste with Ctrl+V"
            } else {
                "Snip copied to the clipboard"
            };
            let detail = format!(
                "{} \u{00d7} {}   \u{00b7}   {name}",
                crop.width, crop.height
            );
            toast::show(
                instance,
                &toast::Message {
                    ok: true,
                    title,
                    detail: &detail,
                    crop: Some(&crop),
                    anchor,
                },
            );
        }
        Err(error) => {
            log(&format!("snip failed: {error}"));
            let detail: String = error.chars().take(110).collect();
            toast::show(
                instance,
                &toast::Message {
                    ok: false,
                    title: "Snip failed",
                    detail: &detail,
                    crop: None,
                    anchor,
                },
            );
        }
    }

    save::prune(&config.save_dir, config.keep_days);
}

fn shell_open(file: &str, parameters: Option<&str>) {
    let verb = wide("open");
    let file = wide(file);
    let parameters = parameters.map(wide);
    // SAFETY: all strings are NUL-terminated and outlive the call.
    unsafe {
        ShellExecuteW(
            null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            parameters.as_ref().map_or(null(), |p| p.as_ptr()),
            null(),
            SW_SHOWNORMAL,
        );
    }
}

fn open_snips_folder() {
    let Some(app) = APP.get() else { return };
    let dir = app.config.save_dir.clone();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log(&format!(
            "open folder: cannot create {}: {e}",
            dir.display()
        ));
        return;
    }
    shell_open(&dir.to_string_lossy(), None);
}

fn edit_config() {
    let path = config::config_path();
    if !path.exists() {
        // Recreates the default file.
        config::load();
    }
    shell_open("notepad.exe", Some(&format!("\"{}\"", path.display())));
}

fn reload_config() {
    let loaded = config::load();
    let Some(app) = APP.get() else { return };
    app.config = loaded.config;
    let hwnd = app.hwnd;
    let hotkey = app.config.hotkey_text.clone();

    let hotkey_ok = register_hotkey();
    tray::set_tip(hwnd, &tip());
    if hotkey_ok && loaded.warnings.is_empty() {
        tray::balloon(
            hwnd,
            "snipdrop: config reloaded",
            &format!("Hotkey: {hotkey}"),
            false,
        );
    } else {
        report_startup(hotkey_ok, &loaded.warnings, false);
    }
    prune_old_snips();
}

fn toggle_autostart() {
    let enable = !tray::autostart_enabled();
    if let Err(e) = tray::set_autostart(enable) {
        log(&format!("autostart: {e}"));
        if let Some(app) = APP.get() {
            tray::balloon(app.hwnd, "snipdrop: autostart failed", &e, true);
        }
    }
}

fn show_menu(hwnd: HWND) {
    let snip_label = match APP.get() {
        Some(app) => format!("Snip\t{}", app.config.hotkey_text),
        None => "Snip".to_string(),
    };
    let autostart = if tray::autostart_enabled() {
        MF_CHECKED
    } else {
        0
    };

    // SAFETY: the menu is created, shown and destroyed here; label buffers
    // outlive the AppendMenuW calls that copy them.
    let command = unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        let item = |flags: u32, id: usize, label: &str| {
            AppendMenuW(menu, flags, id, wide(label).as_ptr());
        };
        item(MF_STRING, CMD_SNIP, &snip_label);
        item(MF_STRING, CMD_FOLDER, "Open snips folder");
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        item(MF_STRING, CMD_EDIT, "Edit config");
        item(MF_STRING, CMD_RELOAD, "Reload config");
        item(MF_STRING | autostart, CMD_AUTOSTART, "Start with Windows");
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        item(MF_STRING, CMD_EXIT, "Exit");

        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        // Without foreground the menu would not close on an outside click;
        // the WM_NULL afterwards is the documented companion to that.
        SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            cursor.x,
            cursor.y,
            0,
            hwnd,
            null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        command as usize
    };

    match command {
        // SAFETY: our own window handle.
        CMD_SNIP => unsafe {
            SetTimer(hwnd, SNIP_TIMER, SNIP_DELAY_MS, None);
        },
        CMD_FOLDER => open_snips_folder(),
        CMD_EDIT => edit_config(),
        CMD_RELOAD => reload_config(),
        CMD_AUTOSTART => toggle_autostart(),
        // SAFETY: our own window handle.
        CMD_EXIT => unsafe {
            DestroyWindow(hwnd);
        },
        _ => {}
    }
}

unsafe extern "system" fn main_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_HOTKEY if wparam == HOTKEY_ID as usize => {
            start_snip();
            0
        }

        WM_TRAY => {
            match (lparam & 0xFFFF) as u32 {
                // SAFETY: our own window handle.
                WM_LBUTTONUP => unsafe {
                    SetTimer(hwnd, SNIP_TIMER, SNIP_DELAY_MS, None);
                },
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            0
        }

        WM_ALREADY_RUNNING => {
            announce_running();
            0
        }

        WM_TIMER if wparam == SNIP_TIMER => {
            // SAFETY: our own window handle.
            unsafe { KillTimer(hwnd, SNIP_TIMER) };
            start_snip();
            0
        }

        WM_DESTROY => {
            capture::cancel();
            toast::dismiss();
            tray::remove(hwnd);
            // SAFETY: ends the message loop in `main`.
            unsafe {
                UnregisterHotKey(hwnd, HOTKEY_ID);
                PostQuitMessage(0);
            }
            0
        }

        _ => {
            if APP
                .get()
                .is_some_and(|app| message == app.taskbar_created && message != 0)
            {
                add_tray_icon();
                return 0;
            }
            // SAFETY: forwarding unhandled messages unchanged.
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
    }
}
