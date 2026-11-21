//! The confirmation shown after a snip: a small card with a thumbnail that
//! appears at the bottom of the monitor and goes away on its own.
//!
//! It never takes focus, so the window the user is about to paste into stays
//! active the whole time.

use std::ffi::c_void;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, BitBlt, CreateCompatibleBitmap,
    CreateCompatibleDC, CreateFontW, CreateSolidBrush, DIB_RGB_COLORS, DeleteDC, DeleteObject,
    EndPaint, FillRect, FrameRect, GetDC, GetMonitorInfoW, GetTextExtentPoint32W, HALFTONE,
    HBITMAP, HDC, HFONT, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, PAINTSTRUCT,
    ReleaseDC, SRCCOPY, SelectObject, SetBkMode, SetBrushOrgEx, SetStretchBltMode, SetTextColor,
    StretchDIBits, TRANSPARENT, TextOutW,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, KillTimer, MA_NOACTIVATE, RegisterClassW,
    SW_SHOWNOACTIVATE, SetTimer, ShowWindow, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_MOUSEACTIVATE,
    WM_PAINT, WM_RBUTTONDOWN, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use crate::capture::Crop;
use crate::util::{UiCell, wide};

const CLASS_NAME: &str = "snipdrop.toast";
const TIMER_ID: usize = 1;
const SUCCESS_MS: u32 = 1600;
const FAILURE_MS: u32 = 6000;

const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
const DWMWCP_ROUND: u32 = 2;

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

const BACKGROUND: COLORREF = rgb(0x1f, 0x1f, 0x1f);
const BORDER: COLORREF = rgb(0x4a, 0x4a, 0x4a);
const TEXT: COLORREF = rgb(0xf2, 0xf2, 0xf2);
const SUBTLE: COLORREF = rgb(0xa8, 0xa8, 0xa8);
const OK_ACCENT: COLORREF = rgb(0x3f, 0xb9, 0x50);
const ERROR_ACCENT: COLORREF = rgb(0xf8, 0x51, 0x49);

pub struct Message<'a> {
    pub ok: bool,
    pub title: &'a str,
    pub detail: &'a str,
    /// Shown as a thumbnail when present.
    pub crop: Option<&'a Crop>,
    /// Screen point that picks the monitor the toast appears on.
    pub anchor: POINT,
}

struct Thumbnail {
    bitmap: HBITMAP,
    width: i32,
    height: i32,
}

struct Toast {
    hwnd: HWND,
    ok: bool,
    title: Vec<u16>,
    detail: Vec<u16>,
    title_font: HFONT,
    detail_font: HFONT,
    thumbnail: Option<Thumbnail>,
    layout: Layout,
}

#[derive(Clone, Copy)]
struct Layout {
    width: i32,
    height: i32,
    padding: i32,
    accent_width: i32,
    text_x: i32,
    title_y: i32,
    detail_y: i32,
}

impl Drop for Toast {
    fn drop(&mut self) {
        // SAFETY: GDI objects created in `show`, released exactly once.
        unsafe {
            DeleteObject(self.title_font);
            DeleteObject(self.detail_font);
            if let Some(thumbnail) = &self.thumbnail {
                DeleteObject(thumbnail.bitmap);
            }
        }
    }
}

static TOAST: UiCell<Toast> = UiCell::new();

pub fn register_class(instance: *mut c_void) -> bool {
    let name = wide(CLASS_NAME);
    // SAFETY: the class struct is fully initialised and `name` outlives the call.
    unsafe {
        let mut class: WNDCLASSW = std::mem::zeroed();
        class.lpfnWndProc = Some(toast_proc);
        class.hInstance = instance;
        class.lpszClassName = name.as_ptr();
        RegisterClassW(&class) != 0
    }
}

/// Close the toast if one is showing. Returns whether there was one.
pub fn dismiss() -> bool {
    let Some(toast) = TOAST.take() else {
        return false;
    };
    // SAFETY: our own window; state is already taken so the messages
    // DestroyWindow sends find nothing to act on.
    unsafe {
        KillTimer(toast.hwnd, TIMER_ID);
        DestroyWindow(toast.hwnd);
    }
    true
}

pub fn show(instance: *mut c_void, message: &Message) {
    dismiss();

    // SAFETY: Win32/GDI calls with valid arguments. Every GDI object created
    // here ends up owned by the Toast (freed in Drop) or is released locally.
    unsafe {
        let monitor = MonitorFromPoint(message.anchor, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return;
        }
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) != 0 {
            dpi_x = 96;
        }
        let scale = |value: i32| value * dpi_x as i32 / 96;

        let face = wide("Segoe UI");
        let title_font = CreateFontW(
            -scale(15),
            0,
            0,
            0,
            600,
            0,
            0,
            0,
            1,
            0,
            0,
            5,
            0,
            face.as_ptr(),
        );
        let detail_font = CreateFontW(
            -scale(12),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            1,
            0,
            0,
            5,
            0,
            face.as_ptr(),
        );
        let title = wide(message.title);
        let detail = wide(message.detail);

        let screen = GetDC(null_mut());
        let title_size = measure(screen, title_font, &title);
        let detail_size = measure(screen, detail_font, &detail);
        let thumbnail = message
            .crop
            .and_then(|crop| make_thumbnail(screen, crop, scale(150), scale(84)));
        ReleaseDC(null_mut(), screen);

        let padding = scale(14);
        let gap = scale(6);
        let accent_width = scale(4);
        let text_height = title_size.cy + gap + detail_size.cy;
        let thumb_height = thumbnail.as_ref().map_or(0, |t| t.height);
        let thumb_width = thumbnail.as_ref().map_or(0, |t| t.width);
        let content_height = text_height.max(thumb_height);
        let text_x = accent_width
            + padding
            + if thumbnail.is_some() {
                thumb_width + padding
            } else {
                0
            };
        let layout = Layout {
            width: text_x + title_size.cx.max(detail_size.cx) + padding,
            height: content_height + padding * 2,
            padding,
            accent_width,
            text_x,
            title_y: padding + (content_height - text_height) / 2,
            detail_y: padding + (content_height - text_height) / 2 + title_size.cy + gap,
        };

        let work = info.rcWork;
        let x = work.left + ((work.right - work.left) - layout.width) / 2;
        let y = work.bottom - layout.height - scale(56);

        let ok = message.ok;
        TOAST.set(Toast {
            hwnd: null_mut(),
            ok,
            title,
            detail,
            title_font,
            detail_font,
            thumbnail,
            layout,
        });

        let class = wide(CLASS_NAME);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            null(),
            WS_POPUP,
            x,
            y,
            layout.width,
            layout.height,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            TOAST.take();
            return;
        }
        if let Some(toast) = TOAST.get() {
            toast.hwnd = hwnd;
        }

        // Rounded corners on Windows 11; silently ignored on older systems.
        let corner = DWMWCP_ROUND;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&raw const corner).cast(),
            size_of::<u32>() as u32,
        );

        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(
            hwnd,
            TIMER_ID,
            if ok { SUCCESS_MS } else { FAILURE_MS },
            None,
        );
    }
}

unsafe fn measure(dc: HDC, font: HFONT, text: &[u16]) -> SIZE {
    // SAFETY: `text` is NUL-terminated; the length passed excludes the NUL.
    unsafe {
        let previous = SelectObject(dc, font);
        let mut size = SIZE { cx: 0, cy: 0 };
        GetTextExtentPoint32W(dc, text.as_ptr(), text.len() as i32 - 1, &mut size);
        SelectObject(dc, previous);
        size
    }
}

/// Scale `crop` down (never up) to fit the given box.
unsafe fn make_thumbnail(
    screen: HDC,
    crop: &Crop,
    max_width: i32,
    max_height: i32,
) -> Option<Thumbnail> {
    if crop.width <= 0 || crop.height <= 0 {
        return None;
    }
    let fit = (max_width as f64 / crop.width as f64)
        .min(max_height as f64 / crop.height as f64)
        .min(1.0);
    let width = ((crop.width as f64 * fit).round() as i32).max(1);
    let height = ((crop.height as f64 * fit).round() as i32).max(1);

    // SAFETY: the memory DC and bitmap are created, used and (DC) released
    // here; the bitmap is returned to the caller, who owns it.
    unsafe {
        let dc = CreateCompatibleDC(screen);
        if dc.is_null() {
            return None;
        }
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        if bitmap.is_null() {
            DeleteDC(dc);
            return None;
        }
        let previous = SelectObject(dc, bitmap);

        let mut source: BITMAPINFO = std::mem::zeroed();
        source.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        source.bmiHeader.biWidth = crop.width;
        source.bmiHeader.biHeight = -crop.height; // top-down, like the crop
        source.bmiHeader.biPlanes = 1;
        source.bmiHeader.biBitCount = 32;
        source.bmiHeader.biCompression = BI_RGB;

        SetStretchBltMode(dc, HALFTONE);
        SetBrushOrgEx(dc, 0, 0, null_mut());
        StretchDIBits(
            dc,
            0,
            0,
            width,
            height,
            0,
            0,
            crop.width,
            crop.height,
            crop.bgrx.as_ptr().cast(),
            &source,
            DIB_RGB_COLORS,
            SRCCOPY,
        );

        SelectObject(dc, previous);
        DeleteDC(dc);
        Some(Thumbnail {
            bitmap,
            width,
            height,
        })
    }
}

unsafe fn paint(dc: HDC, toast: &Toast) {
    let layout = toast.layout;
    // SAFETY: drawing into the DC handed to WM_PAINT; brushes are deleted
    // and selections restored before returning.
    unsafe {
        let fill = |rect: RECT, color: COLORREF| {
            let brush = CreateSolidBrush(color);
            FillRect(dc, &rect, brush);
            DeleteObject(brush);
        };
        let full = RECT {
            left: 0,
            top: 0,
            right: layout.width,
            bottom: layout.height,
        };
        fill(full, BACKGROUND);
        let border = CreateSolidBrush(BORDER);
        FrameRect(dc, &full, border);
        DeleteObject(border);
        fill(
            RECT {
                left: 0,
                top: 0,
                right: layout.accent_width,
                bottom: layout.height,
            },
            if toast.ok { OK_ACCENT } else { ERROR_ACCENT },
        );

        if let Some(thumbnail) = &toast.thumbnail {
            let x = layout.accent_width + layout.padding;
            let y = (layout.height - thumbnail.height) / 2;
            let memory = CreateCompatibleDC(dc);
            let previous = SelectObject(memory, thumbnail.bitmap);
            BitBlt(
                dc,
                x,
                y,
                thumbnail.width,
                thumbnail.height,
                memory,
                0,
                0,
                SRCCOPY,
            );
            SelectObject(memory, previous);
            DeleteDC(memory);
            let frame = RECT {
                left: x - 1,
                top: y - 1,
                right: x + thumbnail.width + 1,
                bottom: y + thumbnail.height + 1,
            };
            let brush = CreateSolidBrush(BORDER);
            FrameRect(dc, &frame, brush);
            DeleteObject(brush);
        }

        SetBkMode(dc, TRANSPARENT as _);
        let previous = SelectObject(dc, toast.title_font);
        SetTextColor(dc, TEXT);
        TextOutW(
            dc,
            layout.text_x,
            layout.title_y,
            toast.title.as_ptr(),
            toast.title.len() as i32 - 1,
        );
        SelectObject(dc, toast.detail_font);
        SetTextColor(dc, SUBTLE);
        TextOutW(
            dc,
            layout.text_x,
            layout.detail_y,
            toast.detail.as_ptr(),
            toast.detail.len() as i32 - 1,
        );
        SelectObject(dc, previous);
    }
}

unsafe extern "system" fn toast_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            // SAFETY: standard BeginPaint/EndPaint pairing on our own window.
            unsafe {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let dc = BeginPaint(hwnd, &mut ps);
                if let Some(toast) = TOAST.get() {
                    paint(dc, toast);
                }
                EndPaint(hwnd, &ps);
            }
            0
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_TIMER | WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
            dismiss();
            0
        }
        // SAFETY: forwarding unhandled messages unchanged.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
