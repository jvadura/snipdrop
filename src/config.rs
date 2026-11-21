//! Configuration: a plain `key = value` file at `%APPDATA%\snipdrop\config.ini`.
//!
//! Deliberately not TOML: Windows paths are full of backslashes and a config
//! format that needs them escaped is a trap. Values are taken literally.

use std::fs;
use std::path::{Path, PathBuf};

use crate::util::APP_NAME;

pub const MOD_ALT: u32 = 0x0001;
pub const MOD_CONTROL: u32 = 0x0002;
pub const MOD_SHIFT: u32 = 0x0004;
pub const MOD_WIN: u32 = 0x0008;

pub const DEFAULT_HOTKEY: &str = "Win+Shift+X";
const DEFAULT_KEEP_DAYS: u32 = 30;

const DEFAULT_FILE: &str = r"# snipdrop configuration
# After editing: right-click the tray icon -> Reload config.

# Global hotkey that starts a snip.
#   Modifiers: Ctrl, Alt, Shift, Win
#   Keys:      A-Z, 0-9, F1-F24, PrintScreen, Insert, Delete, Home, End,
#              PageUp, PageDown, Pause, ScrollLock, Space, Tab
# Examples: Win+Shift+X    Ctrl+Alt+S    F8    Ctrl+PrintScreen
hotkey = Win+Shift+X

# Folder snips are saved to. Empty = %USERPROFILE%\Pictures\snipdrop
save_dir =

# Delete snips older than this many days. 0 = keep forever.
# Only files named snip-*.png inside save_dir are ever deleted.
keep_days = 30

# What a snip puts on the clipboard. Any combination of:
#   path   the file path as text  -> Ctrl+V in a terminal (Claude Code attaches it)
#   file   the file itself        -> paste into Explorer, mail, chat apps
#   image  the bitmap             -> paste into image editors, Alt+V in Claude Code
clipboard = path, file, image
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub mods: u32,
    pub vk: u32,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub hotkey: Hotkey,
    /// The hotkey as written by the user, for menus and messages.
    pub hotkey_text: String,
    pub save_dir: PathBuf,
    pub keep_days: u32,
    pub clip_path: bool,
    pub clip_file: bool,
    pub clip_image: bool,
}

pub struct Loaded {
    pub config: Config,
    /// Human-readable problems found while loading; the app still starts.
    pub warnings: Vec<String>,
    /// True when the config file did not exist and was just created.
    pub first_run: bool,
}

pub fn config_path() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(crate::util::data_dir)
        .join(APP_NAME)
        .join("config.ini")
}

fn default_save_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(crate::util::data_dir)
        .join("Pictures")
        .join(APP_NAME)
}

/// Load the config, creating the default file on first run. Never fails:
/// anything unreadable falls back to defaults and is reported as a warning.
pub fn load() -> Loaded {
    let path = config_path();
    let mut warnings = Vec::new();
    let mut first_run = false;

    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) if !path.exists() => {
            first_run = true;
            if let Some(dir) = path.parent() {
                let _ = fs::create_dir_all(dir);
            }
            if let Err(e) = fs::write(&path, DEFAULT_FILE) {
                warnings.push(format!("Could not create {}: {e}", path.display()));
            }
            DEFAULT_FILE.to_string()
        }
        Err(e) => {
            warnings.push(format!("Could not read {}: {e}", path.display()));
            DEFAULT_FILE.to_string()
        }
    };

    let config = parse(&text, &mut warnings);
    Loaded {
        config,
        warnings,
        first_run,
    }
}

pub fn parse(text: &str, warnings: &mut Vec<String>) -> Config {
    let mut hotkey_text = DEFAULT_HOTKEY.to_string();
    let mut save_dir = String::new();
    let mut keep_days = DEFAULT_KEEP_DAYS;
    let mut clipboard = "path, file, image".to_string();

    // A UTF-8 BOM is what Notepad may add; it must not break the first key.
    for raw in text.trim_start_matches('\u{feff}').lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            warnings.push(format!("Ignored config line: {line}"));
            continue;
        };
        let value = value.trim().trim_matches('"').trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "hotkey" => hotkey_text = value.to_string(),
            "save_dir" => save_dir = value.to_string(),
            "keep_days" => match value.parse() {
                Ok(n) => keep_days = n,
                Err(_) => warnings.push(format!("keep_days is not a number: {value}")),
            },
            "clipboard" => clipboard = value.to_string(),
            other => warnings.push(format!("Unknown config key: {other}")),
        }
    }

    let hotkey = match parse_hotkey(&hotkey_text) {
        Ok(h) => h,
        Err(e) => {
            warnings.push(format!(
                "Hotkey \"{hotkey_text}\": {e}. Using {DEFAULT_HOTKEY}."
            ));
            hotkey_text = DEFAULT_HOTKEY.to_string();
            Hotkey {
                mods: MOD_WIN | MOD_SHIFT,
                vk: b'X' as u32,
            }
        }
    };

    let save_dir = resolve_save_dir(&save_dir);

    let (mut clip_path, mut clip_file, mut clip_image) = (false, false, false);
    for part in clipboard.split([',', ' ', ';']).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "path" | "text" => clip_path = true,
            "file" => clip_file = true,
            "image" | "bitmap" => clip_image = true,
            other => warnings.push(format!("Unknown clipboard format: {other}")),
        }
    }
    if !(clip_path || clip_file || clip_image) {
        warnings.push("clipboard has no formats; using path, file, image.".to_string());
        (clip_path, clip_file, clip_image) = (true, true, true);
    }

    Config {
        hotkey,
        hotkey_text,
        save_dir,
        keep_days,
        clip_path,
        clip_file,
        clip_image,
    }
}

fn resolve_save_dir(value: &str) -> PathBuf {
    if value.is_empty() {
        return default_save_dir();
    }
    let expanded = expand_env(value);
    let path = Path::new(&expanded);
    // The clipboard path must be absolute or the paste target cannot find it.
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Expand `%NAME%` references; unknown names are left untouched.
fn expand_env(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(v) if !name.is_empty() => out.push_str(&v),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn parse_hotkey(text: &str) -> Result<Hotkey, String> {
    let mut mods = 0u32;
    let mut key: Option<(u32, bool)> = None;

    for part in text.split('+').map(str::trim) {
        if part.is_empty() {
            return Err("empty key name".to_string());
        }
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "win" | "windows" | "super" => mods |= MOD_WIN,
            name => {
                if key.is_some() {
                    return Err("more than one non-modifier key".to_string());
                }
                key = Some(parse_key(name).ok_or_else(|| format!("unknown key \"{part}\""))?);
            }
        }
    }

    let (vk, standalone_ok) = key.ok_or_else(|| "no key, only modifiers".to_string())?;
    // A bare letter as a global hotkey would swallow that letter everywhere.
    if mods & (MOD_CONTROL | MOD_ALT | MOD_WIN) == 0 && !standalone_ok {
        return Err("needs Ctrl, Alt or Win".to_string());
    }
    Ok(Hotkey { mods, vk })
}

/// Returns the virtual-key code and whether the key is safe to bind without
/// a modifier (function-style keys are, typing keys are not).
fn parse_key(name: &str) -> Option<(u32, bool)> {
    let bytes = name.as_bytes();
    if bytes.len() == 1 {
        let c = bytes[0].to_ascii_uppercase();
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some((c as u32, false));
        }
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u32>().ok())
        && (1..=24).contains(&n)
    {
        return Some((0x70 + n - 1, true));
    }
    Some(match name {
        "printscreen" | "prtsc" | "prtscn" | "print" | "snapshot" => (0x2C, true),
        "insert" | "ins" => (0x2D, true),
        "pause" | "break" => (0x13, true),
        "scrolllock" | "scroll" => (0x91, true),
        "delete" | "del" => (0x2E, false),
        "home" => (0x24, false),
        "end" => (0x23, false),
        "pageup" | "pgup" => (0x21, false),
        "pagedown" | "pgdn" => (0x22, false),
        "space" => (0x20, false),
        "tab" => (0x09, false),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_combinations() {
        assert_eq!(
            parse_hotkey("Win+Shift+X"),
            Ok(Hotkey {
                mods: MOD_WIN | MOD_SHIFT,
                vk: 0x58
            })
        );
        assert_eq!(
            parse_hotkey(" ctrl + ALT + s "),
            Ok(Hotkey {
                mods: MOD_CONTROL | MOD_ALT,
                vk: 0x53
            })
        );
        assert_eq!(parse_hotkey("F8"), Ok(Hotkey { mods: 0, vk: 0x77 }));
        assert_eq!(parse_hotkey("F24"), Ok(Hotkey { mods: 0, vk: 0x87 }));
        assert_eq!(
            parse_hotkey("PrintScreen"),
            Ok(Hotkey { mods: 0, vk: 0x2C })
        );
        assert_eq!(
            parse_hotkey("Ctrl+PrtSc"),
            Ok(Hotkey {
                mods: MOD_CONTROL,
                vk: 0x2C
            })
        );
    }

    #[test]
    fn hotkey_rejects_dangerous_or_malformed() {
        assert!(parse_hotkey("X").is_err(), "bare letter");
        assert!(parse_hotkey("Shift+X").is_err(), "shift-only letter");
        assert!(parse_hotkey("Space").is_err(), "bare space");
        assert!(parse_hotkey("Ctrl+Shift").is_err(), "modifiers only");
        assert!(parse_hotkey("Ctrl+A+B").is_err(), "two keys");
        assert!(parse_hotkey("Ctrl+Banana").is_err(), "unknown key");
        assert!(parse_hotkey("Ctrl++A").is_err(), "empty part");
        assert!(parse_hotkey("F25").is_err(), "F-key out of range");
        assert!(parse_hotkey("").is_err(), "empty");
    }

    #[test]
    fn default_file_parses_cleanly() {
        let mut warnings = Vec::new();
        let c = parse(DEFAULT_FILE, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(c.hotkey, parse_hotkey(DEFAULT_HOTKEY).unwrap());
        assert_eq!(c.keep_days, 30);
        assert!(c.clip_path && c.clip_file && c.clip_image);
        assert!(c.save_dir.is_absolute());
        assert!(c.save_dir.ends_with("Pictures/snipdrop"));
    }

    #[test]
    fn values_are_literal_and_tolerant() {
        let mut warnings = Vec::new();
        let text = "\u{feff}hotkey = Ctrl+Alt+S\r\nsave_dir = \"D:\\shots\\new folder\"\r\nkeep_days=0\r\nclipboard = path\r\n";
        let c = parse(text, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(c.hotkey_text, "Ctrl+Alt+S");
        assert_eq!(c.save_dir, PathBuf::from(r"D:\shots\new folder"));
        assert_eq!(c.keep_days, 0);
        assert!(c.clip_path && !c.clip_file && !c.clip_image);
    }

    #[test]
    fn bad_values_fall_back_with_warnings() {
        let mut warnings = Vec::new();
        let c = parse(
            "hotkey = Q\nkeep_days = soon\nclipboard = smoke\nwhat\nnope = 1\n",
            &mut warnings,
        );
        assert_eq!(c.hotkey_text, DEFAULT_HOTKEY);
        assert_eq!(c.keep_days, 30);
        assert!(c.clip_path && c.clip_file && c.clip_image);
        assert_eq!(warnings.len(), 6, "{warnings:?}");
    }

    #[test]
    fn env_expansion() {
        // SAFETY: tests in this module do not read this variable concurrently.
        unsafe { std::env::set_var("SNIPDROP_TEST_DIR", r"C:\base") };
        assert_eq!(expand_env(r"%SNIPDROP_TEST_DIR%\snips"), r"C:\base\snips");
        assert_eq!(expand_env(r"%SNIPDROP_NOPE%\x"), r"%SNIPDROP_NOPE%\x");
        assert_eq!(expand_env("100% sure"), "100% sure");
        assert_eq!(expand_env("plain"), "plain");
    }
}
