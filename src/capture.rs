//! Region selection: freeze the desktop into a bitmap, show it dimmed in a
//! topmost window spanning every monitor, and let the user drag a rectangle.
//!
//! Selecting on a frozen copy (rather than on the live desktop) is what makes
//! menus, tooltips and hover states capturable, and it means the pixels saved
//! are exactly the pixels the user saw while dragging.

use std::ffi::c_void;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, BitBlt, CAPTUREBLT, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, EndPaint, FrameRect, GetDC,
    GetStockObject, HBITMAP, HDC, HGDIOBJ, InvalidateRect, PAINTSTRUCT, ReleaseDC, SRCCOPY,
    SelectObject, WHITE_BRUSH,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetForegroundWindow, GetSystemMetrics,
    IDC_CROSS, IsWindow, LoadCursorW, RegisterClassW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOW, SetCursor, SetForegroundWindow, ShowWindow,
    WM_ACTIVATE, WM_CAPTURECHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONDOWN, WM_SETCURSOR, WM_SYSKEYDOWN, WNDCLASSW,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::util::{UiCell, log, wide};

const CLASS_NAME: &str = "snipdrop.overlay";
/// Selections smaller than this on either side are treated as a stray click.
const MIN_SIZE: i32 = 4;
/// How dark the non-selected area is, out of 256.
const DIM_LEVEL: u32 = 120;

/// A cropped piece of the screen: top-down rows of BGRX pixels.
pub struct Crop {
    pub width: i32,
    pub height: i32,
    pub bgrx: Vec<u8>,
}

/// A 32-bit top-down DIB section selected into its own memory DC.
struct Dib {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
    len: usize,
}

impl Dib {
    fn new(width: i32, height: i32) -> Option<Dib> {
        // SAFETY: plain GDI calls; every handle is checked and released on
        // the failure paths, and on success ownership moves into the Dib.
        unsafe {
            let dc = CreateCompatibleDC(null_mut());
            if dc.is_null() {
                return None;
            }
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = width;
            info.bmiHeader.biHeight = -height; // negative = top-down
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;
            let mut bits: *mut c_void = null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bitmap.is_null() || bits.is_null() {
                DeleteDC(dc);
                return None;
            }
            let previous = SelectObject(dc, bitmap);
            Some(Dib {
                dc,
                bitmap,
                previous,
                bits: bits.cast(),
                len: width as usize * height as usize * 4,
            })
        }
    }

    fn pixels(&self) -> &[u8] {
        // SAFETY: `bits` points at `len` bytes owned by the DIB section,
        // which lives as long as `self`.
        unsafe { std::slice::from_raw_parts(self.bits, self.len) }
    }

    fn pixels_mut(&mut self) -> &mut [u8] {
        // SAFETY: as in `pixels`, and `&mut self` guarantees exclusivity.
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.len) }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        // SAFETY: handles were created in `new` and are released exactly once.
        unsafe {
            SelectObject(self.dc, self.previous);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

struct Overlay {
    hwnd: HWND,
    /// Screen coordinates of the overlay's top-left corner.
    origin: POINT,
    width: i32,
    height: i32,
    /// The untouched screenshot.
    original: Dib,
    /// The screenshot darkened; what the unselected area shows.
    dimmed: Dib,
    /// What is on screen right now: dimmed plus the bright selection.
    composed: Dib,
    previous_foreground: HWND,
    dragging: bool,
    anchor: POINT,
    /// The selection currently drawn into `composed`.
    drawn: Option<RECT>,
}

static OVERLAY: UiCell<Overlay> = UiCell::new();

/// What the caller does with a finished snip. `anchor` is a screen point
/// inside the selection, used to pick the monitor for the confirmation.
pub type OnSnip = fn(crop: Crop, anchor: POINT);

static ON_SNIP: UiCell<OnSnip> = UiCell::new();

pub fn register_class(instance: *mut c_void) -> bool {
    let name = wide(CLASS_NAME);
    // SAFETY: the class struct is fully initialised and `name` outlives the call.
    unsafe {
        let mut class: WNDCLASSW = std::mem::zeroed();
        class.lpfnWndProc = Some(overlay_proc);
        class.hInstance = instance;
        class.hCursor = LoadCursorW(null_mut(), IDC_CROSS);
        class.lpszClassName = name.as_ptr();
        RegisterClassW(&class) != 0
    }
}

pub fn is_active() -> bool {
    OVERLAY.is_some()
}

/// Freeze the screen and show the selection overlay.
pub fn begin(instance: *mut c_void, on_snip: OnSnip) {
    if is_active() {
        return;
    }
    ON_SNIP.set(on_snip);

    // SAFETY: Win32 calls with valid arguments; resources are owned by Dib
    // values or released before returning.
    unsafe {
        let previous_foreground = GetForegroundWindow();
        let origin = POINT {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
        };
        let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if width <= 0 || height <= 0 {
            log("snip: virtual screen has no size");
            return;
        }

        let (Some(original), Some(mut dimmed), Some(mut composed)) = (
            Dib::new(width, height),
            Dib::new(width, height),
            Dib::new(width, height),
        ) else {
            log(&format!(
                "snip: could not allocate {width}x{height} bitmaps"
            ));
            return;
        };

        let screen = GetDC(null_mut());
        let copied = BitBlt(
            original.dc,
            0,
            0,
            width,
            height,
            screen,
            origin.x,
            origin.y,
            SRCCOPY | CAPTUREBLT,
        );
        ReleaseDC(null_mut(), screen);
        if copied == 0 {
            log("snip: BitBlt from the screen failed");
            return;
        }

        for (dst, src) in dimmed.pixels_mut().iter_mut().zip(original.pixels()) {
            *dst = ((*src as u32 * DIM_LEVEL) >> 8) as u8;
        }
        composed.pixels_mut().copy_from_slice(dimmed.pixels());

        // The state has to exist before the window does: creation already
        // delivers messages to overlay_proc.
        OVERLAY.set(Overlay {
            hwnd: null_mut(),
            origin,
            width,
            height,
            original,
            dimmed,
            composed,
            previous_foreground,
            dragging: false,
            anchor: POINT { x: 0, y: 0 },
            drawn: None,
        });

        let class = wide(CLASS_NAME);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class.as_ptr(),
            null(),
            WS_POPUP,
            origin.x,
            origin.y,
            width,
            height,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            OVERLAY.take();
            log("snip: could not create the overlay window");
            return;
        }
        match OVERLAY.get() {
            Some(overlay) => overlay.hwnd = hwnd,
            None => {
                // Cancelled while the window was still being created.
                DestroyWindow(hwnd);
                return;
            }
        }
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
}

/// Close the overlay without producing a snip.
pub fn cancel() {
    close();
}

/// Tear the overlay down and give focus back to whoever had it.
/// Returns the state so `finish` can still read the pixels.
fn close() -> Option<Overlay> {
    // Taking the state first makes every message that arrives during the
    // teardown (WM_ACTIVATE, WM_CAPTURECHANGED, ...) a no-op.
    let overlay = OVERLAY.take()?;
    // SAFETY: the handles belong to this overlay; IsWindow guards against the
    // previously focused window having closed in the meantime.
    unsafe {
        if overlay.dragging {
            ReleaseCapture();
        }
        if !overlay.hwnd.is_null() {
            DestroyWindow(overlay.hwnd);
        }
        if !overlay.previous_foreground.is_null() && IsWindow(overlay.previous_foreground) != 0 {
            SetForegroundWindow(overlay.previous_foreground);
        }
    }
    Some(overlay)
}

fn finish(selection: RECT) {
    let Some(overlay) = close() else { return };
    let width = selection.right - selection.left;
    let height = selection.bottom - selection.top;

    let row_bytes = width as usize * 4;
    let stride = overlay.width as usize * 4;
    let source = overlay.original.pixels();
    let mut bgrx = Vec::with_capacity(row_bytes * height as usize);
    for y in selection.top..selection.bottom {
        let start = y as usize * stride + selection.left as usize * 4;
        match source.get(start..start + row_bytes) {
            Some(row) => bgrx.extend_from_slice(row),
            None => {
                log("snip: selection outside the captured bitmap");
                return;
            }
        }
    }

    let anchor = POINT {
        x: overlay.origin.x + selection.left + width / 2,
        y: overlay.origin.y + selection.top + height / 2,
    };
    // Free ~3 screens worth of bitmaps before encoding the PNG.
    drop(overlay);

    if let Some(on_snip) = ON_SNIP.get() {
        on_snip(
            Crop {
                width,
                height,
                bgrx,
            },
            anchor,
        );
    }
}

/// The rectangle spanned by two pixels, including both of them. Inclusive
/// because the cursor cannot move past the last pixel of the screen: with an
/// exclusive edge the final row and column could never be selected.
fn selection_rect(anchor: POINT, current: POINT) -> RECT {
    RECT {
        left: anchor.x.min(current.x),
        top: anchor.y.min(current.y),
        right: anchor.x.max(current.x) + 1,
        bottom: anchor.y.max(current.y) + 1,
    }
}

fn grown(rect: RECT, by: i32) -> RECT {
    RECT {
        left: rect.left - by,
        top: rect.top - by,
        right: rect.right + by,
        bottom: rect.bottom + by,
    }
}

fn union(a: RECT, b: RECT) -> RECT {
    RECT {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }
}

/// Redraw `composed` for a new selection and return the area that changed.
fn draw_selection(overlay: &mut Overlay, selection: Option<RECT>) -> Option<RECT> {
    let mut dirty = None;
    // SAFETY: blits between memory DCs owned by `overlay`; GDI clips
    // rectangles that reach outside the bitmaps.
    unsafe {
        if let Some(old) = overlay.drawn.take() {
            let area = grown(old, 1);
            BitBlt(
                overlay.composed.dc,
                area.left,
                area.top,
                area.right - area.left,
                area.bottom - area.top,
                overlay.dimmed.dc,
                area.left,
                area.top,
                SRCCOPY,
            );
            dirty = Some(area);
        }
        if let Some(new) = selection {
            BitBlt(
                overlay.composed.dc,
                new.left,
                new.top,
                new.right - new.left,
                new.bottom - new.top,
                overlay.original.dc,
                new.left,
                new.top,
                SRCCOPY,
            );
            // The frame sits just outside the selection so it never covers
            // pixels that end up in the snip.
            let frame = grown(new, 1);
            FrameRect(overlay.composed.dc, &frame, GetStockObject(WHITE_BRUSH));
            dirty = Some(dirty.map_or(frame, |d| union(d, frame)));
            overlay.drawn = Some(new);
        }
    }
    dirty
}

/// Mouse position from an LPARAM, clamped to the overlay.
fn mouse_point(lparam: LPARAM, overlay: &Overlay) -> POINT {
    let x = (lparam & 0xFFFF) as i16 as i32;
    let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
    POINT {
        x: x.clamp(0, overlay.width - 1),
        y: y.clamp(0, overlay.height - 1),
    }
}

unsafe extern "system" fn overlay_proc(
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
                let mut paint: PAINTSTRUCT = std::mem::zeroed();
                let dc = BeginPaint(hwnd, &mut paint);
                if let Some(overlay) = OVERLAY.get() {
                    let area = paint.rcPaint;
                    BitBlt(
                        dc,
                        area.left,
                        area.top,
                        area.right - area.left,
                        area.bottom - area.top,
                        overlay.composed.dc,
                        area.left,
                        area.top,
                        SRCCOPY,
                    );
                }
                EndPaint(hwnd, &paint);
            }
            0
        }

        WM_SETCURSOR => {
            // SAFETY: loading a stock cursor.
            unsafe { SetCursor(LoadCursorW(null_mut(), IDC_CROSS)) };
            1
        }

        WM_LBUTTONDOWN => {
            let mut dirty = None;
            if let Some(overlay) = OVERLAY.get() {
                overlay.anchor = mouse_point(lparam, overlay);
                overlay.dragging = true;
                dirty = draw_selection(overlay, None);
            }
            // SAFETY: our own window handle.
            unsafe {
                SetCapture(hwnd);
                if let Some(area) = dirty {
                    InvalidateRect(hwnd, &area, 0);
                }
            }
            0
        }

        WM_MOUSEMOVE => {
            let mut dirty = None;
            if let Some(overlay) = OVERLAY.get()
                && overlay.dragging
            {
                let rect = selection_rect(overlay.anchor, mouse_point(lparam, overlay));
                dirty = draw_selection(overlay, Some(rect));
            }
            if let Some(area) = dirty {
                // SAFETY: our own window handle.
                unsafe { InvalidateRect(hwnd, &area, 0) };
            }
            0
        }

        WM_LBUTTONUP => {
            let mut selection = None;
            let mut dirty = None;
            let mut was_dragging = false;
            if let Some(overlay) = OVERLAY.get()
                && overlay.dragging
            {
                was_dragging = true;
                overlay.dragging = false;
                let rect = selection_rect(overlay.anchor, mouse_point(lparam, overlay));
                if rect.right - rect.left >= MIN_SIZE && rect.bottom - rect.top >= MIN_SIZE {
                    selection = Some(rect);
                } else {
                    // A click without a real drag: stay in snip mode.
                    dirty = draw_selection(overlay, None);
                }
            }
            if was_dragging {
                // SAFETY: releasing the capture we took on button down.
                unsafe { ReleaseCapture() };
            }
            if let Some(rect) = selection {
                finish(rect);
            } else if let Some(area) = dirty {
                // SAFETY: our own window handle.
                unsafe { InvalidateRect(hwnd, &area, 0) };
            }
            0
        }

        WM_CAPTURECHANGED => {
            // Capture taken away mid-drag (e.g. by a system dialog): drop the
            // half-made selection instead of leaving it stuck to the cursor.
            let mut dirty = None;
            if let Some(overlay) = OVERLAY.get()
                && overlay.dragging
            {
                overlay.dragging = false;
                dirty = draw_selection(overlay, None);
            }
            if let Some(area) = dirty {
                // SAFETY: our own window handle.
                unsafe { InvalidateRect(hwnd, &area, 0) };
            }
            0
        }

        WM_RBUTTONDOWN => {
            cancel();
            0
        }

        WM_KEYDOWN | WM_SYSKEYDOWN if wparam == VK_ESCAPE as usize => {
            cancel();
            0
        }

        // Losing activation (Alt+Tab, another window popping up) ends snip
        // mode, so the overlay can never be left covering the desktop.
        WM_ACTIVATE if wparam & 0xFFFF == 0 => {
            cancel();
            0
        }

        // SAFETY: forwarding unhandled messages unchanged.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
