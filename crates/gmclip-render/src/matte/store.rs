//! The mask-track cache (SPEC §5.8: "cached `(fingerprint, span, crop, model)` … size
//! cap + LRU"): `<dir>/<key>.gmask` files, least recently used evicted first once the
//! folder passes its cap, and the last few tracks held decoded-ready in memory. A store
//! without a folder keeps tracks in memory only (tests, one-off renders).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use super::Track;

/// Default cap on the folder: 2 GiB.
pub const DEFAULT_CAP: u64 = 2 << 30;
/// Tracks kept in memory.
const MEM_TRACKS: usize = 24;

/// The process-wide store: `$GMCLIP_MATTE_CACHE`, else `~/Library/Caches/gmclip/mattes`
/// (else memory only), capped at [`DEFAULT_CAP`]. The app's preview and every export
/// share it, so a track built once serves both.
pub fn shared() -> std::sync::Arc<MatteStore> {
    static S: std::sync::OnceLock<std::sync::Arc<MatteStore>> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        let dir = std::env::var_os("GMCLIP_MATTE_CACHE")
            .map(PathBuf::from)
            .or_else(|| {
                if cfg!(windows) {
                    // the app's cache folder there (%LOCALAPPDATA%\gmclip)
                    std::env::var_os("LOCALAPPDATA")
                        .map(|d| PathBuf::from(d).join("gmclip").join("mattes"))
                } else {
                    std::env::var_os("HOME")
                        .map(|h| PathBuf::from(h).join("Library/Caches/gmclip/mattes"))
                }
            });
        std::sync::Arc::new(match dir {
            Some(d) => MatteStore::new(d, DEFAULT_CAP),
            None => MatteStore::in_memory(),
        })
    })
    .clone()
}

/// The track cache. Cheap to share (`Arc<MatteStore>`); every method takes `&self`.
#[derive(Debug)]
pub struct MatteStore {
    dir: Option<PathBuf>,
    cap: u64,
    /// Most recently used last.
    mem: Mutex<Vec<(String, Arc<Track>)>>,
}

impl MatteStore {
    /// A store in `dir` (created on first write) capped at `cap` bytes.
    pub fn new(dir: impl Into<PathBuf>, cap: u64) -> Self {
        Self {
            dir: Some(dir.into()),
            cap,
            mem: Mutex::new(Vec::new()),
        }
    }

    /// A store that only remembers tracks while it lives.
    pub fn in_memory() -> Self {
        Self {
            dir: None,
            cap: DEFAULT_CAP,
            mem: Mutex::new(Vec::new()),
        }
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    fn file(&self, key: &str) -> Option<PathBuf> {
        // keys are hex; anything else never touches the disk
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        self.dir.as_ref().map(|d| d.join(format!("{key}.gmask")))
    }

    fn remember(&self, key: &str, t: Arc<Track>) {
        let mut mem = self.mem.lock().unwrap_or_else(|e| e.into_inner());
        mem.retain(|(k, _)| k != key);
        mem.push((key.to_owned(), t));
        if mem.len() > MEM_TRACKS {
            let drop = mem.len() - MEM_TRACKS;
            mem.drain(..drop);
        }
    }

    /// The track under `key`, from memory or the folder (whose file is touched, so the
    /// LRU sees the use).
    pub fn get(&self, key: &str) -> Option<Arc<Track>> {
        {
            let mut mem = self.mem.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(i) = mem.iter().position(|(k, _)| k == key) {
                let e = mem.remove(i);
                let t = e.1.clone();
                mem.push(e);
                return Some(t);
            }
        }
        let path = self.file(key)?;
        let bytes = std::fs::read(&path).ok()?;
        let Some(t) = Track::from_bytes(&bytes) else {
            let _ = std::fs::remove_file(&path); // corrupt: rebuilt on demand
            return None;
        };
        if let Ok(f) = std::fs::File::options().write(true).open(&path) {
            let _ = f.set_modified(SystemTime::now());
        }
        let t = Arc::new(t);
        self.remember(key, t.clone());
        Some(t)
    }

    /// Whether `key` is cached (without loading it).
    pub fn contains(&self, key: &str) -> bool {
        let mem = self.mem.lock().unwrap_or_else(|e| e.into_inner());
        mem.iter().any(|(k, _)| k == key) || self.file(key).is_some_and(|p| p.is_file())
    }

    /// Stores `t` under `key` (atomically: a temp file renamed into place), then evicts
    /// least recently used files until the folder is under its cap.
    pub fn put(&self, key: &str, t: Track) -> std::io::Result<Arc<Track>> {
        let t = Arc::new(t);
        if let Some(path) = self.file(key) {
            if let Some(d) = path.parent() {
                std::fs::create_dir_all(d)?;
            }
            let tmp = path.with_extension("gmask.tmp");
            std::fs::write(&tmp, t.to_bytes())?;
            std::fs::rename(&tmp, &path)?;
            self.evict(key)?;
        }
        self.remember(key, t.clone());
        Ok(t)
    }

    /// Removes the oldest `.gmask` files (by modification time) until the folder holds
    /// at most `cap` bytes; `keep` is never removed.
    fn evict(&self, keep: &str) -> std::io::Result<()> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        for e in std::fs::read_dir(dir)?.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "gmask")
                && let Ok(m) = e.metadata()
            {
                files.push((m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len(), p));
            }
        }
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        files.sort();
        for (_, len, p) in files {
            if total <= self.cap {
                break;
            }
            if p.file_stem().is_some_and(|s| s == keep) {
                continue;
            }
            if std::fs::remove_file(&p).is_ok() {
                total -= len;
                let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned());
                let mut mem = self.mem.lock().unwrap_or_else(|e| e.into_inner());
                mem.retain(|(k, _)| Some(k) != stem.as_ref());
            }
        }
        Ok(())
    }

    /// Bytes on disk.
    pub fn disk_bytes(&self) -> u64 {
        let Some(dir) = &self.dir else { return 0 };
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().extension().is_some_and(|x| x == "gmask"))
                    .filter_map(|e| e.metadata().ok())
                    .map(|m| m.len())
                    .sum()
            })
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Mask, Track};
    use super::*;

    fn track(seed: u8, n: usize) -> Track {
        let masks: Vec<Mask> = (0..n)
            .map(|i| {
                let mut m = Mask::filled(64, 64, 0);
                for (k, v) in m.data.iter_mut().enumerate() {
                    *v = ((k * 31 + i * 7 + usize::from(seed)) % 251) as u8; // incompressible-ish
                }
                m
            })
            .collect();
        Track::from_masks("test", &masks).unwrap()
    }

    #[test]
    fn put_get_and_lru_eviction() {
        let dir = std::env::temp_dir().join(format!("gmclip-matte-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let one = track(1, 4).to_bytes().len() as u64;
        let s = MatteStore::new(&dir, one * 2 + one / 2); // room for two
        s.put("aa", track(1, 4)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        s.put("bb", track(2, 4)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        // a fresh store (no memory) reading "aa" touches it, so "bb" is the oldest
        let s2 = MatteStore::new(&dir, one * 2 + one / 2);
        assert_eq!(s2.get("aa").unwrap().frame(1), track(1, 4).frame(1));
        std::thread::sleep(std::time::Duration::from_millis(20));
        s2.put("cc", track(3, 4)).unwrap();
        assert!(s2.contains("aa") && s2.contains("cc"));
        assert!(
            !dir.join("bb.gmask").exists(),
            "least recently used evicted"
        );
        assert!(s2.disk_bytes() <= one * 2 + one / 2);
        // a corrupt file reads as missing and is removed
        std::fs::write(dir.join("dd.gmask"), b"nope").unwrap();
        assert!(s2.get("dd").is_none());
        assert!(!dir.join("dd.gmask").exists());
        // non-hex keys never reach the disk
        assert!(s2.get("../x").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn memory_store() {
        let s = MatteStore::in_memory();
        assert!(s.get("ab").is_none());
        s.put("ab", track(1, 2)).unwrap();
        assert!(s.contains("ab"));
        assert_eq!(s.get("ab").unwrap().len(), 2);
    }
}
