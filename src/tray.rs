//! The notification-area icon, its balloon messages and the autostart switch.

use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON,
};

use crate::util::{APP_NAME, wide};

const ICON_ID: u32 = 1;
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn copy_into(target: &mut [u16], text: &str) {
    let limit = target.len() - 1;
    let mut written = 0;
    for unit in text.encode_utf16().take(limit) {
        target[written] = unit;
        written += 1;
    }
    target[written] = 0;
}

fn base(hwnd: HWND) -> NOTIFYICONDATAW {
    // SAFETY: NOTIFYICONDATAW is plain data; zero is a valid starting state.
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = ICON_ID;
    data
}

/// Add the icon. Also called again whenever Explorer restarts.
pub fn add(hwnd: HWND, icon: HICON, callback_message: u32, tip: &str) -> bool {
    let mut data = base(hwnd);
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = callback_message;
    data.hIcon = icon;
    copy_into(&mut data.szTip, tip);
    // SAFETY: `data` is fully initialised for the flags set.
    unsafe { Shell_NotifyIconW(NIM_ADD, &data) != 0 }
}

pub fn set_tip(hwnd: HWND, tip: &str) {
    let mut data = base(hwnd);
    data.uFlags = NIF_TIP;
    copy_into(&mut data.szTip, tip);
    // SAFETY: as in `add`.
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
}

pub fn remove(hwnd: HWND) {
    let data = base(hwnd);
    // SAFETY: as in `add`.
    unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
}

/// Show a notification from the tray icon.
pub fn balloon(hwnd: HWND, title: &str, text: &str, warning: bool) {
    let mut data = base(hwnd);
    data.uFlags = NIF_INFO;
    data.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_INFO };
    copy_into(&mut data.szInfoTitle, title);
    copy_into(&mut data.szInfo, text);
    // SAFETY: as in `add`.
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
}

/// Draw the tray icon at the system's small-icon size: a blue tile with
/// white crop marks. Generated at runtime so the binary needs no resources.
pub fn create_icon() -> HICON {
    // SAFETY: GDI objects are created, handed to CreateIconIndirect (which
    // copies them) and deleted here.
    unsafe {
        let size = GetSystemMetrics(SM_CXSMICON).clamp(16, 64);

        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = size;
        info.bmiHeader.biHeight = -size;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut bits = null_mut();
        let color = CreateDIBSection(null_mut(), &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if color.is_null() || bits.is_null() {
            return null_mut();
        }
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u8, (size * size * 4) as usize);
        render_icon(pixels, size as usize);

        let mask = CreateBitmap(size, size, 1, 1, null());
        let icon_info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&icon_info);
        DeleteObject(color);
        DeleteObject(mask);
        icon
    }
}

/// Fill `pixels` (BGRA, top-down, `size` x `size`) with the icon artwork.
fn render_icon(pixels: &mut [u8], size: usize) {
    const SAMPLES: usize = 4;
    for y in 0..size {
        for x in 0..size {
            let (mut tile, mut mark) = (0u32, 0u32);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let u = (x as f32 + (sx as f32 + 0.5) / SAMPLES as f32) / size as f32;
                    let v = (y as f32 + (sy as f32 + 0.5) / SAMPLES as f32) / size as f32;
                    if in_tile(u, v) {
                        tile += 1;
                        if in_marks(u, v) {
                            mark += 1;
                        }
                    }
                }
            }
            let total = (SAMPLES * SAMPLES) as u32;
            // Mix the tile colour with white by how much of the pixel the
            // crop marks cover; fully transparent pixels stay black.
            let blend = |tile_channel: u32| {
                (tile_channel * (tile - mark) + 255 * mark)
                    .checked_div(tile)
                    .unwrap_or(0) as u8
            };
            let offset = (y * size + x) * 4;
            pixels[offset] = blend(0xEB); // blue
            pixels[offset + 1] = blend(0x63); // green
            pixels[offset + 2] = blend(0x25); // red
            pixels[offset + 3] = (255 * tile / total) as u8;
        }
    }
}

/// Rounded square covering most of the unit square.
fn in_tile(u: f32, v: f32) -> bool {
    const INSET: f32 = 0.03;
    const RADIUS: f32 = 0.22;
    let (lo, hi) = (INSET, 1.0 - INSET);
    if u < lo || u > hi || v < lo || v > hi {
        return false;
    }
    let cx = u.clamp(lo + RADIUS, hi - RADIUS);
    let cy = v.clamp(lo + RADIUS, hi - RADIUS);
    (u - cx).powi(2) + (v - cy).powi(2) <= RADIUS * RADIUS
}

/// Four L-shaped crop marks around the centre.
fn in_marks(u: f32, v: f32) -> bool {
    const NEAR: f32 = 0.24;
    const THICK: f32 = 0.11;
    const ARM: f32 = 0.22;
    // Fold all four corners onto the top-left one.
    let a = if u > 0.5 { 1.0 - u } else { u };
    let b = if v > 0.5 { 1.0 - v } else { v };
    let horizontal = (NEAR..NEAR + ARM).contains(&a) && (NEAR..NEAR + THICK).contains(&b);
    let vertical = (NEAR..NEAR + THICK).contains(&a) && (NEAR..NEAR + ARM).contains(&b);
    horizontal || vertical
}

fn open_run_key(access: u32) -> Option<HKEY> {
    let key_name = wide(RUN_KEY);
    let mut key: HKEY = null_mut();
    // SAFETY: valid out-pointer; the key is closed by every caller.
    let status =
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key_name.as_ptr(), 0, access, &mut key) };
    (status == 0).then_some(key)
}

/// Is the app registered to start when the user signs in?
pub fn autostart_enabled() -> bool {
    let Some(key) = open_run_key(KEY_QUERY_VALUE) else {
        return false;
    };
    let name = wide(APP_NAME);
    // SAFETY: querying existence only, so no buffers are passed.
    unsafe {
        let status = RegQueryValueExW(
            key,
            name.as_ptr(),
            null(),
            null_mut(),
            null_mut(),
            null_mut(),
        );
        RegCloseKey(key);
        status == 0
    }
}

/// Register or unregister the running executable under HKCU\...\Run.
pub fn set_autostart(enabled: bool) -> Result<(), String> {
    let key = open_run_key(KEY_SET_VALUE).ok_or("Cannot open the Run registry key")?;
    let name = wide(APP_NAME);
    // SAFETY: `command` is a NUL-terminated UTF-16 buffer whose byte length
    // is passed alongside it; the key is closed before returning.
    unsafe {
        let status = if enabled {
            let exe = std::env::current_exe().map_err(|e| e.to_string());
            match exe {
                Ok(exe) => {
                    let command = wide(&format!("\"{}\"", exe.display()));
                    RegSetValueExW(
                        key,
                        name.as_ptr(),
                        0,
                        REG_SZ,
                        command.as_ptr().cast(),
                        (command.len() * 2) as u32,
                    )
                }
                Err(e) => {
                    RegCloseKey(key);
                    return Err(e);
                }
            }
        } else {
            RegDeleteValueW(key, name.as_ptr())
        };
        RegCloseKey(key);
        if status == 0 {
            Ok(())
        } else {
            Err(format!("Registry error {status}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_has_transparent_corners_and_an_opaque_marked_body() {
        for size in [16usize, 20, 24, 32] {
            let mut pixels = vec![0u8; size * size * 4];
            render_icon(&mut pixels, size);
            let at = |x: usize, y: usize| {
                let o = (y * size + x) * 4;
                (pixels[o], pixels[o + 1], pixels[o + 2], pixels[o + 3])
            };
            assert_eq!(at(0, 0).3, 0, "corner is transparent at {size}");
            assert_eq!(
                at(size / 2, size / 2),
                (0xEB, 0x63, 0x25, 255),
                "centre is the tile colour at {size}"
            );
            let white = pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| **p == [255, 255, 255, 255])
                .count();
            assert!(
                white >= 8,
                "crop marks are visible at {size}: {white} white pixels"
            );
        }
    }

    #[test]
    fn tip_text_is_truncated_and_terminated() {
        let mut buffer = [0xFFFFu16; 8];
        copy_into(&mut buffer, "a very long tooltip");
        assert_eq!(buffer[7], 0);
        assert_eq!(String::from_utf16_lossy(&buffer[..7]), "a very ");
    }
}
