//! The speech models on first run (SPEC §9 R-ML-3; coordinator 2026-10-15): they live in
//! `~/Library/Caches/gmclip` (never in the app bundle). When they are missing, the
//! transcript pane offers "Download speech model (N MB)" — **a click, never a silent
//! download** — which fetches every missing pinned file of the register
//! (`gmclip_ml::models::REGISTER`; of the ONNX Runtime builds only this platform's), over
//! HTTPS only, with the OS's curl by absolute path (`/usr/bin/curl`; Windows
//! `System32\curl.exe`, no shell), and
//! installs each through `ModelCache::install_local_copy`, which refuses any file whose
//! size or SHA-256 is not the pin. The ONNX Runtime archive is hash-checked before its one
//! dylib is extracted. Until R-ML-3's CDN exists the URLs are the register's upstreams.
//!
//! Lane S (SPEC §12.5 "size, licence, metered warning, resumable"): the setup sheet starts
//! the download when it opens (a human opened it). Each file is fetched into
//! `<cache>/.partial/` and kept there on a failure or a pause, so the next start resumes
//! it (`curl -C -`) instead of starting over; **Pause** stops the transfer.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gmclip_ml::models::{
    ModelCache, ModelEntry, ORT_1_30_0_MACOS_ARM64, ORT_1_30_0_WINDOWS_X64, REGISTER,
};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};

/// An ONNX Runtime row comes as Microsoft's archive: its file name in `.partial/`, its
/// sha256 (the register's `source` line names it) and the one member extracted.
struct OrtArchive {
    partial: &'static str,
    sha256: &'static str,
    member: &'static str,
}

fn ort_archive(e: &ModelEntry) -> Option<OrtArchive> {
    if std::ptr::eq(e, &ORT_1_30_0_MACOS_ARM64) {
        Some(OrtArchive {
            partial: "ort.tgz",
            sha256: "6ebb5062a934537c352937821f9fe9718e7de1a2db1122a93dd363ffd53a7012",
            member: "onnxruntime-osx-arm64-1.30.0/lib/libonnxruntime.1.30.0.dylib",
        })
    } else if std::ptr::eq(e, &ORT_1_30_0_WINDOWS_X64) {
        Some(OrtArchive {
            partial: "ort-win-x64.zip",
            sha256: "c6ba983baf5681af108599675d2a89c2d145512d02de28aed0bff177cd0ba949",
            member: "onnxruntime-win-x64-1.30.0/lib/onnxruntime.dll",
        })
    } else {
        None
    }
}

/// An OS tool by absolute path (no PATH lookup): `/usr/bin/<name>`; Windows 10+ ships
/// curl and bsdtar (which reads zip too) in `%SystemRoot%\System32`.
fn os_tool(name: &str) -> PathBuf {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot")
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| r"C:\Windows".into());
        PathBuf::from(root)
            .join("System32")
            .join(format!("{name}.exe"))
    } else {
        PathBuf::from("/usr/bin").join(name)
    }
}

static RUNNING: AtomicBool = AtomicBool::new(false);
/// Set by [`pause`]: the running transfer is stopped and the download ends "paused".
static PAUSED: AtomicBool = AtomicBool::new(false);
/// The transfer in flight, so Pause can stop it.
static CHILD: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);

/// Where a file being downloaded waits until it is whole and checked: `<cache>/.partial/
/// <entry dir with / as _>__<name>` (outside the model folders, which the loader reads).
pub fn partial_path(root: &Path, entry_dir: &str, name: &str) -> PathBuf {
    root.join(".partial")
        .join(format!("{}__{name}", entry_dir.replace('/', "_")))
}

/// Bytes already downloaded towards the missing files (what a resume skips).
fn partial_bytes(c: &ModelCache, todo: &[(&'static ModelEntry, &'static str, u64)]) -> u64 {
    todo.iter()
        .map(|(e, name, size)| {
            let p = if let Some(a) = ort_archive(e) {
                c.root().join(".partial").join(a.partial)
            } else {
                partial_path(c.root(), e.dir, name)
            };
            std::fs::metadata(p).map_or(0, |m| m.len().min(*size))
        })
        .sum()
}

fn cache() -> AppResult<ModelCache> {
    ModelCache::default_cache().map_err(|e| AppError::internal(format!("model cache: {e:?}")))
}

/// The files missing (or wrong) in the cache: `(entry, file name, size)`.
fn missing(c: &ModelCache) -> Vec<(&'static ModelEntry, &'static str, u64)> {
    let mut out = Vec::new();
    for e in REGISTER {
        let e: &'static ModelEntry = e;
        // another platform's ONNX Runtime is never fetched
        if !gmclip_ml::models::needed_here(e) || c.verify(e).is_ok() {
            continue;
        }
        for f in e.files {
            let p = c.dir(e).join(f.name);
            let ok = std::fs::metadata(&p).is_ok_and(|m| m.len() == f.size)
                && gmclip_ml::sha256::file_hex(&p).is_ok_and(|(h, _)| h == f.sha256);
            if !ok {
                out.push((e, f.name, f.size));
            }
        }
    }
    out
}

/// What the "Download speech model" button needs to say.
pub fn status() -> Value {
    match cache() {
        Ok(c) => {
            let m = missing(&c);
            let bytes: u64 = m.iter().map(|x| x.2).sum();
            let mut ids: Vec<&str> = m.iter().map(|x| x.0.id).collect();
            ids.dedup();
            // what is downloaded, under which licences (SPEC §15.3 model register)
            let licences: Vec<Value> = REGISTER
                .iter()
                .filter(|e| ids.contains(&e.id))
                .map(|e| json!({"id": e.id, "weights": e.weights_licence, "code": e.code_licence}))
                .collect();
            json!({"installed": m.is_empty(), "missing_files": m.len(), "bytes": bytes,
                   "mb": (bytes as f64 / 1e6).round(), "root": c.root(),
                   "running": RUNNING.load(Ordering::Relaxed),
                   "partial_bytes": partial_bytes(&c, &m), "licences": licences})
        }
        Err(e) => json!({"installed": false, "error": e.message}),
    }
}

/// Downloads `url` into `to`, continuing a partial file already there (`-C -`); a file
/// already of `size` bytes is left as it is. Pause stops it.
fn curl(url: &str, to: &Path, size: Option<u64>, tick: &dyn Fn(u64)) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("only HTTPS".into());
    }
    if size.is_some_and(|n| std::fs::metadata(to).is_ok_and(|m| m.len() == n)) {
        return Ok(());
    }
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let child = std::process::Command::new(os_tool("curl"))
        .args([
            "-fsSL", "--proto", "=https", "--retry", "2", "-C", "-", "-o",
        ])
        .arg(to)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Ok(mut g) = CHILD.lock() {
        *g = Some(child);
    }
    // wait without holding the lock, so Pause can stop it; progress about once a second
    let mut n = 0u32;
    let st = loop {
        n += 1;
        if n.is_multiple_of(10) {
            tick(std::fs::metadata(to).map_or(0, |m| m.len()));
        }
        if PAUSED.load(Ordering::SeqCst) {
            if let Some(mut ch) = CHILD.lock().ok().and_then(|mut g| g.take()) {
                let _ = ch.kill();
                let _ = ch.wait();
            }
            return Err("paused".into());
        }
        let done = CHILD
            .lock()
            .ok()
            .and_then(|mut g| g.as_mut().and_then(|ch| ch.try_wait().ok().flatten()));
        if let Some(st) = done {
            if let Ok(mut g) = CHILD.lock() {
                *g = None;
            }
            break st;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    if st.success() {
        Ok(())
    } else {
        Err(format!(
            "download failed ({st}); press Download again to resume"
        ))
    }
}

/// Pauses the download: the transfer stops and its partial file is kept for the resume.
pub fn pause() -> Value {
    let running = RUNNING.load(Ordering::SeqCst);
    if running {
        PAUSED.store(true, Ordering::SeqCst);
    }
    json!({"paused": running})
}

/// Starts the download (once at a time); progress goes to the `app` scope.
pub fn download(core: &Arc<AppCore>) -> AppResult<Value> {
    let c = cache()?;
    let todo = missing(&c);
    if todo.is_empty() {
        return Ok(json!({"installed": true}));
    }
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err(AppError::new(
            "Busy",
            "the speech model is already downloading",
        ));
    }
    let total: u64 = todo.iter().map(|x| x.2).sum();
    PAUSED.store(false, Ordering::SeqCst);
    let weak = Arc::downgrade(core);
    let _ = std::thread::Builder::new()
        .name("gmclip-models".into())
        .spawn(move || {
            use crate::events::{AppEvent, ModelsState};
            let publish = |state: ModelsState,
                           done: Option<u64>,
                           file: Option<&str>,
                           error: Option<String>| {
                if let Some(core) = weak.upgrade() {
                    core.bus.app(AppEvent::Models {
                        state,
                        done,
                        total: done.map(|_| total),
                        file: file.map(str::to_owned),
                        error,
                    });
                }
            };
            // partial files live in the cache, so a later start resumes them
            let tmp: PathBuf = c.root().join(".partial");
            let r = (|| -> Result<(), String> {
                std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
                let mut done = 0u64;
                for (e, name, size) in &todo {
                    publish(ModelsState::Running, Some(done), Some(name), None);
                    let pin = e.files.iter().find(|f| f.name == *name).ok_or("pin")?;
                    let src = if let Some(a) = ort_archive(e) {
                        let arc = tmp.join(a.partial);
                        curl(pin.upstream, &arc, None, &|_| {})?;
                        let (h, _) = gmclip_ml::sha256::file_hex(&arc).map_err(|_| "read")?;
                        if h != a.sha256 {
                            return Err("the ONNX Runtime archive is not the pinned one".into());
                        }
                        let out = tmp.join("ort");
                        let _ = std::fs::remove_dir_all(&out);
                        std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
                        // bsdtar detects the format (tgz on macOS, zip on Windows)
                        let st = std::process::Command::new(os_tool("tar"))
                            .arg("-xf")
                            .arg(&arc)
                            .arg("-C")
                            .arg(&out)
                            .arg(a.member)
                            .status()
                            .map_err(|e| e.to_string())?;
                        if !st.success() {
                            return Err("extracting ONNX Runtime failed".into());
                        }
                        let _ = std::fs::remove_file(&arc);
                        out.join(a.member)
                    } else {
                        let f = partial_path(c.root(), e.dir, name);
                        let at = done;
                        curl(pin.upstream, &f, Some(*size), &|got| {
                            publish(
                                ModelsState::Running,
                                Some(at + got.min(*size)),
                                Some(name),
                                None,
                            );
                        })?;
                        f
                    };
                    let installed = c.install_local_copy(e, name, &src);
                    // a whole file that is not the pin is fetched again from the start
                    let _ = std::fs::remove_file(&src);
                    installed.map_err(|err| format!("{name}: {err:?}"))?;
                    done += size;
                }
                Ok(())
            })();
            let _ = std::fs::remove_dir_all(tmp.join("ort"));
            RUNNING.store(false, Ordering::SeqCst);
            let paused = PAUSED.swap(false, Ordering::SeqCst);
            match r {
                Err(_) if paused => publish(ModelsState::Paused, None, None, None),
                Ok(()) => {
                    publish(ModelsState::Done, Some(total), None, None);
                    // transcribe what waited for the model
                    if let Some(core) = weak.upgrade() {
                        core.analysis.retry_after_models(&core);
                    }
                }
                Err(e) => publish(
                    ModelsState::Failed,
                    None,
                    None,
                    Some(e.chars().take(300).collect::<String>()),
                ),
            }
        });
    Ok(json!({"started": true, "bytes": total}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_files_wait_outside_the_model_folders_and_count_towards_a_resume() {
        let root = std::env::temp_dir().join(format!("gmclip-partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let p = partial_path(
            &root,
            "models/parakeet-tdt-0.6b-v3-int8",
            "encoder-model.int8.onnx",
        );
        assert_eq!(
            p,
            root.join(".partial/models_parakeet-tdt-0.6b-v3-int8__encoder-model.int8.onnx")
        );
        assert!(!p.starts_with(root.join("models")));
        let c = ModelCache::new(root.clone()).unwrap();
        let e: &'static ModelEntry = REGISTER[0];
        let f = e.files[0];
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(partial_path(&root, e.dir, f.name), vec![0u8; 1234]).unwrap();
        assert_eq!(partial_bytes(&c, &[(e, f.name, f.size)]), 1234);
        // nothing running: pause says so and changes nothing
        assert_eq!(pause()["paused"], false);
        let _ = std::fs::remove_dir_all(&root);
    }
}
