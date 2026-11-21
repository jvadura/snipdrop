//! Writing snips to disk and pruning old ones.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::capture::Crop;
use crate::util::{local_time, log};

const PREFIX: &str = "snip-";
const EXTENSION: &str = "png";

/// Encode `crop` as a PNG in `dir` and return the file's path.
pub fn save_png(dir: &Path, crop: &Crop) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    let path = free_path(dir);

    let mut rgb = Vec::with_capacity(crop.width as usize * crop.height as usize * 3);
    for pixel in crop.bgrx.as_chunks::<4>().0 {
        rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
    }

    let result = write_png(&path, crop.width as u32, crop.height as u32, &rgb);
    if let Err(e) = result {
        // Do not leave a truncated file behind for something to paste.
        let _ = fs::remove_file(&path);
        return Err(format!("Cannot write {}: {e}", path.display()));
    }
    Ok(path)
}

fn write_png(path: &Path, width: u32, height: u32, rgb: &[u8]) -> Result<(), String> {
    let file = File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    // Snips are pasted seconds later and pruned after a while: favour speed.
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgb).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())
}

/// `snip-YYYYMMDD-HHMMSS.png`, with a numeric suffix if that second is taken.
/// No spaces or special characters, so the path pastes cleanly anywhere.
fn free_path(dir: &Path) -> PathBuf {
    let t = local_time();
    let stem = format!(
        "{PREFIX}{:04}{:02}{:02}-{:02}{:02}{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    );
    let first = dir.join(format!("{stem}.{EXTENSION}"));
    if !first.exists() {
        return first;
    }
    (2..1000)
        .map(|n| dir.join(format!("{stem}-{n}.{EXTENSION}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

fn is_snip(path: &Path) -> bool {
    let name_ok = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(PREFIX));
    let ext_ok = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION));
    name_ok && ext_ok
}

/// Delete snips older than `keep_days`. Only touches files this tool names
/// (`snip-*.png`), so pointing `save_dir` at a shared folder is safe.
pub fn prune(dir: &Path, keep_days: u32) {
    if keep_days == 0 {
        return;
    }
    let max_age = Duration::from_secs(u64::from(keep_days) * 24 * 60 * 60);
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_snip(&path) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let expired = now.duration_since(modified).is_ok_and(|age| age > max_age);
        if expired && let Err(e) = fs::remove_file(&path) {
            log(&format!("prune: could not delete {}: {e}", path.display()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("snipdrop-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn checker(width: i32, height: i32) -> Crop {
        let mut bgrx = Vec::new();
        for y in 0..height {
            for x in 0..width {
                // Blue, green, red, padding: distinct per channel on purpose.
                bgrx.extend_from_slice(&[(x * 7) as u8, (y * 5) as u8, ((x + y) * 3) as u8, 0]);
            }
        }
        Crop {
            width,
            height,
            bgrx,
        }
    }

    #[test]
    fn png_round_trips_with_channels_in_the_right_order() {
        let dir = temp_dir("roundtrip");
        let crop = checker(37, 21);
        let path = save_png(&dir, &crop).unwrap();
        assert!(is_snip(&path));

        let decoder = png::Decoder::new(std::io::BufReader::new(File::open(&path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buffer).unwrap();
        assert_eq!((info.width, info.height), (37, 21));
        assert_eq!(info.color_type, png::ColorType::Rgb);

        let decoded = buffer[..info.buffer_size()].as_chunks::<3>().0;
        let original = crop.bgrx.as_chunks::<4>().0;
        assert_eq!(decoded.len(), original.len());
        for (rgb, bgrx) in decoded.iter().zip(original) {
            assert_eq!(rgb, &[bgrx[2], bgrx[1], bgrx[0]]);
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn names_do_not_collide_within_a_second() {
        let dir = temp_dir("collide");
        let crop = checker(4, 4);
        let paths: Vec<_> = (0..5).map(|_| save_png(&dir, &crop).unwrap()).collect();
        for (i, a) in paths.iter().enumerate() {
            assert!(a.exists());
            assert!(!a.to_str().unwrap().contains(' '));
            for b in &paths[i + 1..] {
                assert_ne!(a, b);
            }
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prune_only_deletes_old_snips() {
        let dir = temp_dir("prune");
        let old = SystemTime::now() - Duration::from_secs(40 * 24 * 60 * 60);
        let make = |name: &str, aged: bool| {
            let path = dir.join(name);
            fs::write(&path, b"x").unwrap();
            if aged {
                File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(old)
                    .unwrap();
            }
            path
        };
        let old_snip = make("snip-20260101-000000.png", true);
        let new_snip = make("snip-20261121-000000.png", false);
        let old_other = make("holiday.png", true);
        let old_text = make("snip-notes.txt", true);

        prune(&dir, 0);
        assert!(old_snip.exists(), "keep_days = 0 keeps everything");

        prune(&dir, 30);
        assert!(!old_snip.exists());
        assert!(new_snip.exists());
        assert!(old_other.exists(), "foreign files are never touched");
        assert!(old_text.exists(), "only .png snips are pruned");
        fs::remove_dir_all(&dir).unwrap();
    }
}
