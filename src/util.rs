//! Small helpers shared by every module: wide strings, the UI-thread global
//! cell, local time and the error log.

use std::cell::UnsafeCell;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

pub const APP_NAME: &str = "snipdrop";

/// UTF-16, NUL-terminated copy of `s` for Win32 calls.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// State owned by the UI thread.
///
/// The whole program runs on one thread (the message loop), so there is never
/// concurrent access. Window procedures do re-enter each other, which is why
/// this hands out raw access instead of a `RefCell`: a `RefCell` would turn a
/// harmless re-entrant read into a panic, and a panic in a tray tool means the
/// tool silently disappears.
pub struct UiCell<T>(UnsafeCell<Option<T>>);

// SAFETY: only ever touched from the single UI thread.
unsafe impl<T> Sync for UiCell<T> {}

impl<T> UiCell<T> {
    pub const fn new() -> Self {
        Self(UnsafeCell::new(None))
    }

    /// Borrow the value. Callers keep the borrow short and do not hold it
    /// across Win32 calls that dispatch messages back into the same state.
    #[allow(clippy::mut_from_ref)]
    pub fn get(&self) -> Option<&mut T> {
        // SAFETY: single-threaded, see the type-level comment.
        unsafe { (*self.0.get()).as_mut() }
    }

    pub fn set(&self, value: T) {
        // SAFETY: single-threaded, see the type-level comment.
        unsafe { *self.0.get() = Some(value) }
    }

    pub fn take(&self) -> Option<T> {
        // SAFETY: single-threaded, see the type-level comment.
        unsafe { (*self.0.get()).take() }
    }

    pub fn is_some(&self) -> bool {
        // SAFETY: single-threaded, see the type-level comment.
        unsafe { (*self.0.get()).is_some() }
    }
}

pub fn local_time() -> SYSTEMTIME {
    // SAFETY: SYSTEMTIME is plain data and GetLocalTime fills all of it.
    unsafe {
        let mut st: SYSTEMTIME = std::mem::zeroed();
        GetLocalTime(&mut st);
        st
    }
}

/// `%LOCALAPPDATA%\snipdrop`, falling back to the temp dir.
pub fn data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(APP_NAME)
}

pub fn log_path() -> PathBuf {
    data_dir().join("snipdrop.log")
}

/// Append a line to the log. Never fails loudly: logging must not be able to
/// take the app down.
pub fn log(msg: &str) {
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    // Keep the log from growing forever.
    if fs::metadata(&path)
        .map(|m| m.len() > 256 * 1024)
        .unwrap_or(false)
    {
        let _ = fs::remove_file(&path);
    }
    let st = local_time();
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, msg
        );
    }
}
