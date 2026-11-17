//! Home's recent-video thumbnails (one frame per video, cached).
//!
//! `video_thumb{path}` answers for a video in Recent videos only: one RGB frame about a
//! second in, [`THUMB_W`] wide, read from the project's scrub proxy when one is on disk
//! (one small decode) and from the file itself otherwise. Each answer is kept in
//! `<OS cache>/gmclip/thumbs/<key>.rgb`, keyed by the path, size and modification time,
//! so Home asks the decoder once per video version.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};

/// Thumbnail width (px): two device pixels per CSS pixel of a Home card.
pub const THUMB_W: u32 = 320;
/// The frame read (≈ 1 s in at 30 fps: past a black first frame or a fade-in).
const AT_FRAME: u64 = 30;

/// The cache file of a video version.
fn cache_file(root: &Path, path: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let m = std::fs::metadata(path).ok()?;
    let mtime = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (path, m.len(), mtime, THUMB_W, AT_FRAME).hash(&mut h);
    Some(root.join(format!("{:016x}.rgb", h.finish())))
}

/// `w u32 · h u32 · RGB` (little-endian).
fn encode(w: u32, h: u32, rgb: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + rgb.len());
    v.extend_from_slice(&w.to_le_bytes());
    v.extend_from_slice(&h.to_le_bytes());
    v.extend_from_slice(rgb);
    v
}

fn decode(b: &[u8]) -> Option<(u32, u32, &[u8])> {
    let w = u32::from_le_bytes(b.get(0..4)?.try_into().ok()?);
    let h = u32::from_le_bytes(b.get(4..8)?.try_into().ok()?);
    let rgb = b.get(8..)?;
    (w > 0 && h > 0 && rgb.len() == (w * h * 3) as usize).then_some((w, h, rgb))
}

/// Standard base64 with padding (the thumbnail's bytes in a JSON reply).
pub fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for k in 0..4 {
            if k <= c.len() {
                s.push(T[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// One frame of `path` as `(w, h, rgb)`: the project's proxy if on disk, else the file.
#[cfg(any(target_os = "macos", windows))]
fn render(core: &AppCore, project: &str, path: &Path) -> Option<(u32, u32, Vec<u8>, &'static str)> {
    use gmclip_render::export::proxy::{self, ProxyReader};
    let root = crate::proxy::cache_root(&core.dirs.app_data)
        .join(project)
        .join("proxies");
    if let Ok(key) = proxy::content_key(path)
        && let Some(mut r) = ProxyReader::open(&root.join(key))
    {
        for n in [AT_FRAME, 0] {
            if r.covers(n)
                && let Ok((w, h, rgb)) = r.thumbnail(n, THUMB_W)
            {
                return Some((w, h, rgb, "proxy"));
            }
        }
    }
    proxy::file_thumbnail(path, AT_FRAME, THUMB_W)
        .ok()
        .map(|(w, h, rgb)| (w, h, rgb, "file"))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn render(_: &AppCore, _: &str, _: &Path) -> Option<(u32, u32, Vec<u8>, &'static str)> {
    None
}

/// `video_thumb{path}`: `{w, h, rgb (base64), from: cache|proxy|file}`, or null when the
/// file cannot be read. Only videos in Recent videos are answered.
pub fn video_thumb(core: &AppCore, path: &str) -> AppResult<Value> {
    let r = crate::video::load_recent(core)
        .into_iter()
        .find(|r| r.path == Path::new(path))
        .ok_or_else(|| AppError::invalid("thumbnails are for videos in Recent videos"))?;
    let root = crate::proxy::cache_root(&core.dirs.app_data).join("thumbs");
    let file = cache_file(&root, &r.path);
    if let Some(b) = file.as_ref().and_then(|f| std::fs::read(f).ok())
        && let Some((w, h, rgb)) = decode(&b)
    {
        return Ok(json!({"w": w, "h": h, "rgb": base64(rgb), "from": "cache"}));
    }
    let Some((w, h, rgb, from)) = render(core, &r.project, &r.path) else {
        return Ok(Value::Null);
    };
    if let Some(f) = file
        && gmclip_mcp::paths::mkdir_private(&root).is_ok()
    {
        let tmp = f.with_extension("rgb.tmp");
        if std::fs::write(&tmp, encode(w, h, &rgb)).is_ok() {
            let _ = std::fs::rename(tmp, f);
        }
    }
    Ok(json!({"w": w, "h": h, "rgb": base64(&rgb), "from": from}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0xfe, 0x00]), "//4A");
    }

    #[test]
    fn the_cache_record_round_trips_and_rejects_short_bodies() {
        let rgb = vec![7u8; 4 * 2 * 3];
        let b = encode(4, 2, &rgb);
        assert_eq!(decode(&b), Some((4, 2, &rgb[..])));
        assert_eq!(decode(&b[..b.len() - 1]), None);
        assert_eq!(decode(&[0; 8]), None);
    }

    #[test]
    fn a_new_version_of_the_file_gets_a_new_cache_key() {
        let d = std::env::temp_dir().join(format!("gmclip-thumbkey-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("v.mp4");
        std::fs::write(&f, b"one").unwrap();
        let a = cache_file(&d, &f).unwrap();
        assert_eq!(cache_file(&d, &f), Some(a.clone()));
        std::fs::write(&f, b"two!").unwrap();
        assert_ne!(cache_file(&d, &f).unwrap(), a);
        let _ = std::fs::remove_dir_all(&d);
    }
}
