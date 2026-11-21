//! Putting a snip on the clipboard in every form a paste target might want.
//!
//! Terminals only paste text. Claude Code (and similar agents) turn pasted
//! text that is the absolute path of an existing image file into an image
//! attachment, so the path as text is what makes a snip pasteable with Ctrl+V.
//! The file list and the bitmap are there for everything that is not a
//! terminal: Explorer, mail and chat apps, image editors.

use std::path::Path;

use windows_sys::Win32::Foundation::{GlobalFree, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Threading::Sleep;

use crate::capture::Crop;
use crate::util::wide;

const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const DROPEFFECT_COPY: u32 = 1;

/// Size of the Win32 `DROPFILES` header.
const DROPFILES_SIZE: usize = 20;
/// Size of `BITMAPINFOHEADER`.
const DIB_HEADER_SIZE: usize = 40;

pub struct Formats {
    pub path: bool,
    pub file: bool,
    pub image: bool,
}

/// Replace the clipboard contents with the snip.
pub fn set(owner: HWND, path: &Path, crop: &Crop, formats: &Formats) -> Result<(), String> {
    let path_wide = wide(&path.to_string_lossy());

    // Another program may hold the clipboard for a moment (clipboard
    // managers do this constantly), so opening it is retried.
    let mut opened = false;
    for _ in 0..25 {
        // SAFETY: plain Win32 call; `owner` is our own window.
        if unsafe { OpenClipboard(owner) } != 0 {
            opened = true;
            break;
        }
        // SAFETY: Sleep has no preconditions.
        unsafe { Sleep(20) };
    }
    if !opened {
        return Err("The clipboard is locked by another program".to_string());
    }

    let mut failed = Vec::new();
    // SAFETY: the clipboard is open and owned by us until CloseClipboard.
    unsafe {
        EmptyClipboard();
        if formats.path && !put(CF_UNICODETEXT, &text_bytes(&path_wide)) {
            failed.push("path");
        }
        if formats.file {
            let effect = RegisterClipboardFormatW(wide("Preferred DropEffect").as_ptr());
            if !put(CF_HDROP, &drop_files_bytes(&path_wide)) {
                failed.push("file");
            } else if effect != 0 {
                put(effect, &DROPEFFECT_COPY.to_le_bytes());
            }
        }
        if formats.image && !put(CF_DIB, &dib_bytes(crop)) {
            failed.push("image");
        }
        CloseClipboard();
    }

    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Could not put {} on the clipboard",
            failed.join(", ")
        ))
    }
}

/// Hand `data` to the clipboard under `format`. The clipboard must be open.
unsafe fn put(format: u32, data: &[u8]) -> bool {
    // SAFETY: the block is allocated with the exact size, locked, filled and
    // unlocked; ownership passes to the system only if SetClipboardData
    // succeeds, otherwise it is freed here.
    unsafe {
        let handle = GlobalAlloc(GMEM_MOVEABLE, data.len());
        if handle.is_null() {
            return false;
        }
        let target = GlobalLock(handle) as *mut u8;
        if target.is_null() {
            GlobalFree(handle);
            return false;
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), target, data.len());
        GlobalUnlock(handle);
        if SetClipboardData(format, handle).is_null() {
            GlobalFree(handle);
            return false;
        }
        true
    }
}

fn utf16_bytes(units: &[u16]) -> impl Iterator<Item = u8> + '_ {
    units.iter().flat_map(|u| u.to_le_bytes())
}

/// CF_UNICODETEXT: the NUL-terminated UTF-16 string.
fn text_bytes(path_wide: &[u16]) -> Vec<u8> {
    utf16_bytes(path_wide).collect()
}

/// CF_HDROP: a DROPFILES header followed by a double-NUL-terminated list.
fn drop_files_bytes(path_wide: &[u16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(DROPFILES_SIZE + path_wide.len() * 2 + 2);
    bytes.extend_from_slice(&(DROPFILES_SIZE as u32).to_le_bytes()); // pFiles
    bytes.extend_from_slice(&[0; 8]); // pt
    bytes.extend_from_slice(&0u32.to_le_bytes()); // fNC
    bytes.extend_from_slice(&1u32.to_le_bytes()); // fWide
    bytes.extend(utf16_bytes(path_wide)); // includes the string's NUL
    bytes.extend_from_slice(&[0, 0]); // list terminator
    bytes
}

/// CF_DIB: BITMAPINFOHEADER plus bottom-up 24-bit rows padded to 4 bytes.
/// 24-bit on purpose: a 32-bit DIB makes some programs read the unused
/// fourth byte as alpha and paste a fully transparent image.
fn dib_bytes(crop: &Crop) -> Vec<u8> {
    let width = crop.width as usize;
    let height = crop.height as usize;
    let stride = (width * 3 + 3) & !3;
    let image_size = stride * height;

    let mut bytes = Vec::with_capacity(DIB_HEADER_SIZE + image_size);
    bytes.extend_from_slice(&(DIB_HEADER_SIZE as u32).to_le_bytes()); // biSize
    bytes.extend_from_slice(&crop.width.to_le_bytes()); // biWidth
    bytes.extend_from_slice(&crop.height.to_le_bytes()); // biHeight (positive = bottom-up)
    bytes.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    bytes.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
    bytes.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    bytes.extend_from_slice(&(image_size as u32).to_le_bytes()); // biSizeImage
    bytes.extend_from_slice(&[0; 16]); // resolution and palette fields

    let padding = [0u8; 3];
    for row in crop.bgrx.chunks_exact(width * 4).rev() {
        for pixel in row.as_chunks::<4>().0 {
            bytes.extend_from_slice(&pixel[..3]);
        }
        bytes.extend_from_slice(&padding[..stride - width * 3]);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_files_layout() {
        let path = wide(r"C:\a\b.png");
        let bytes = drop_files_bytes(&path);
        assert_eq!(
            u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
            20,
            "pFiles"
        );
        assert_eq!(
            u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
            1,
            "fWide"
        );
        assert_eq!(bytes.len(), 20 + path.len() * 2 + 2);
        assert_eq!(&bytes[bytes.len() - 4..], &[0, 0, 0, 0], "double NUL");
        assert_eq!(bytes[20], b'C');
    }

    #[test]
    fn dib_is_bottom_up_bgr_with_padded_rows() {
        // 3x2 image: each pixel's blue channel is 10*row + column.
        let mut bgrx = Vec::new();
        for y in 0..2u8 {
            for x in 0..3u8 {
                bgrx.extend_from_slice(&[10 * y + x, 100, 200, 0]);
            }
        }
        let bytes = dib_bytes(&Crop {
            width: 3,
            height: 2,
            bgrx,
        });
        let stride = 12; // 9 bytes of pixels padded to a multiple of 4
        assert_eq!(bytes.len(), 40 + stride * 2);
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(bytes[4..8].try_into().unwrap()), 3);
        assert_eq!(i32::from_le_bytes(bytes[8..12].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(bytes[14..16].try_into().unwrap()), 24);
        assert_eq!(u32::from_le_bytes(bytes[20..24].try_into().unwrap()), 24);
        // First stored row is the image's bottom row (y = 1).
        assert_eq!(&bytes[40..43], &[10, 100, 200]);
        assert_eq!(&bytes[46..49], &[12, 100, 200]);
        assert_eq!(&bytes[49..52], &[0, 0, 0], "row padding");
        // Second stored row is the image's top row (y = 0).
        assert_eq!(&bytes[52..55], &[0, 100, 200]);
    }
}
