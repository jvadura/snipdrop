# snipdrop

Snip a region of the screen and paste it straight into a terminal coding agent
with **Ctrl+V**. A tiny Windows tray app, written in Rust.

Press the hotkey, drag a rectangle, paste. That is the whole tool.

## Why

The Windows Snipping Tool puts a *bitmap* on the clipboard. A terminal can only
paste *text*, so Ctrl+V in Windows Terminal does nothing with a snip. The usual
workaround is to save the snip, find the file, copy its path and paste that.

Copying an image *file* in Explorer does paste, because the terminal pastes the
file's path, and Claude Code turns a pasted path to an existing image file into
an image attachment (`[Image #1]`).

snipdrop does that in one step. Each snip is saved as a PNG and the clipboard
is filled with:

| Format | Used by |
| --- | --- |
| the file path as text | terminals: Ctrl+V pastes the path, the agent attaches the image |
| the file itself | Explorer, mail and chat apps |
| the bitmap | image editors, and Alt+V in Claude Code |

## Install

Requires [Rust](https://rustup.rs) with the MSVC toolchain.

```
cargo install --git https://github.com/jvadura/snipdrop
snipdrop
```

An icon appears in the notification area. Right-click it and tick
**Start with Windows** to keep it around.

## Use

| Action | Result |
| --- | --- |
| **Win+Shift+X** (or left-click the tray icon) | the screen freezes and dims |
| drag a rectangle | the snip is saved and copied; a confirmation shows for a moment |
| Esc or right-click | cancel |
| Ctrl+V in the terminal | the image is attached |

Focus returns to the window you were in, so you can paste immediately.

## Configure

Right-click the tray icon → **Edit config**, then **Reload config**.
The file lives at `%APPDATA%\snipdrop\config.ini`:

```ini
# Modifiers: Ctrl, Alt, Shift, Win
# Keys: A-Z, 0-9, F1-F24, PrintScreen, Insert, Delete, Home, End,
#       PageUp, PageDown, Pause, ScrollLock, Space, Tab
hotkey = Win+Shift+X

# Empty = %USERPROFILE%\Pictures\snipdrop
save_dir =

# Delete snips older than this many days. 0 = keep forever.
# Only files named snip-*.png inside save_dir are ever deleted.
keep_days = 30

# Any combination of: path, file, image
clipboard = path, file, image
```

Notes:

- If the hotkey is already taken by another program, snipdrop says so in a
  notification and keeps running; pick another one.
- For `PrintScreen` you may first have to stop Windows from using the key
  itself (Settings → Accessibility → Keyboard → *Use the Print screen key to
  open screen capture*).
- Drop `path` from `clipboard` if you mostly paste into apps that would
  otherwise insert the path as text. Windows Terminal still pastes the path
  from the `file` entry; most other terminals need `path`.
- Values are taken literally. No quoting or escaping of backslashes.

## How it works

One thread, one message loop, raw Win32 via `windows-sys`, no GUI framework.

- The hotkey is a plain `RegisterHotKey`; no keyboard hook.
- The desktop is copied once into a bitmap, and selection happens on that
  frozen copy, so menus and hover states can be captured and the saved pixels
  are exactly what was on screen.
- The process is per-monitor DPI aware, so snips are pixel-exact on scaled
  displays.
- The tray icon is re-added when Explorer restarts.
- Errors go to `%LOCALAPPDATA%\snipdrop\snipdrop.log`.

The image path detection is Claude Code's behaviour, checked against version
2.1.330: pasted text that is an absolute path to an existing `.png`, `.jpg`,
`.jpeg`, `.gif` or `.webp` file becomes an attachment. Other agents that accept
image paths work the same way.

## Limits

- Windows 10 and 11 only.
- Developed and tested on a single display; multi-monitor setups are handled
  in the code but have had less testing.
- On HDR displays the captured colours can look washed out.

## Build

```
cargo build --release
cargo test
```

## License

MIT
