//! The process's one ONNX Runtime (SPEC §9: `ort =2.0.0-rc.14`, `load-dynamic`).
//!
//! R-LOAD-1: the dylib loads by **absolute path**, and only after its SHA-256 equals
//! the register pin ([`platform_entry`]: macOS arm64, Windows x64). In a release the path is
//! inside the signed bundle; in the lab it is the gmclip cache. ort's own relative-path
//! fallback (next to the executable) is never reached because a relative path is refused
//! here first. Telemetry is off.

use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ort::environment::Environment;

use crate::MlError;
use crate::models::{ModelCache, ModelEntry};

/// The loaded runtime.
#[derive(Clone, Debug)]
pub struct OrtRuntime {
    env: Environment,
    dylib: PathBuf,
    entry: &'static ModelEntry,
}

static RUNTIME: OnceLock<OrtRuntime> = OnceLock::new();
static INIT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The register row for this platform's ORT build, if one is pinned.
pub fn platform_entry() -> Option<&'static ModelEntry> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(&crate::models::ORT_1_30_0_MACOS_ARM64)
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        Some(&crate::models::ORT_1_30_0_WINDOWS_X64)
    } else {
        // x86_64 macOS, Windows arm64 and Linux builds are pinned when their CI builds
        // land (SPEC §9: built from source where no official binary exists).
        None
    }
}

impl OrtRuntime {
    /// Loads (once) the pinned ORT from `cache`.
    pub fn init(cache: &ModelCache) -> Result<&'static OrtRuntime, MlError> {
        let entry =
            platform_entry().ok_or(MlError::Unsupported("no ORT pinned for this platform"))?;
        let verified = cache.verify(entry)?;
        let file = entry
            .files
            .first()
            .ok_or(MlError::Load("register row has no file"))?;
        Self::init_from(&verified.file(file.name), entry)
    }

    /// Loads (once) the pinned ORT from inside the signed app bundle (R-LOAD-1: macOS
    /// `Contents/Frameworks/<file>`, the Windows install dir). The release path; the
    /// cache path ([`OrtRuntime::init`]) is for the lab and dev builds. The file must be
    /// absolute, a regular file, and its SHA-256 the pin.
    pub fn init_bundled(dylib: &Path) -> Result<&'static OrtRuntime, MlError> {
        let entry =
            platform_entry().ok_or(MlError::Unsupported("no ORT pinned for this platform"))?;
        let pin = entry
            .files
            .first()
            .ok_or(MlError::Load("register row has no file"))?;
        if !dylib.is_absolute() {
            return Err(MlError::Load("ONNX Runtime path is not absolute"));
        }
        let meta =
            std::fs::symlink_metadata(dylib).map_err(|_| MlError::ModelMissing(pin.name.into()))?;
        if !meta.file_type().is_file() || meta.len() != pin.size {
            return Err(MlError::Refused(crate::models::NOT_OUR_MODEL));
        }
        let (h, _) =
            crate::sha256::file_hex(dylib).map_err(|_| MlError::Load("cannot read ORT"))?;
        if h != pin.sha256 {
            return Err(MlError::Refused(crate::models::NOT_OUR_MODEL));
        }
        Self::init_from(dylib, entry)
    }

    /// Loads (once) the dylib at `path`, which must be absolute; the caller has checked
    /// its hash (`init` and `init_bundled` do).
    fn init_from(path: &Path, entry: &'static ModelEntry) -> Result<&'static OrtRuntime, MlError> {
        if let Some(rt) = RUNTIME.get() {
            return if rt.dylib == path {
                Ok(rt)
            } else {
                Err(MlError::Load("a different ONNX Runtime is already loaded"))
            };
        }
        if !path.is_absolute() {
            return Err(MlError::Load("ONNX Runtime path is not absolute"));
        }
        let _g = INIT.lock().map_err(|_| MlError::Load("poisoned"))?;
        if let Some(rt) = RUNTIME.get() {
            return Ok(rt);
        }
        let env = ort::init_from(path)
            .map_err(|_| MlError::Load("ONNX Runtime dylib"))?
            .with_name("gmclip")
            .with_telemetry(false)
            .build()
            .map_err(|_| MlError::Load("ONNX Runtime environment"))?;
        let rt = OrtRuntime {
            env,
            dylib: path.to_path_buf(),
            entry,
        };
        let rt = RUNTIME.get_or_init(|| rt);
        // its static destructors are registered now: the exit hook goes after them
        arm_exit();
        Ok(rt)
    }

    pub fn get() -> Option<&'static OrtRuntime> {
        RUNTIME.get()
    }

    pub fn environment(&self) -> &Environment {
        &self.env
    }

    pub fn dylib(&self) -> &Path {
        &self.dylib
    }

    /// e.g. `onnxruntime-1.30.0-osx-arm64`.
    pub fn id(&self) -> &'static str {
        self.entry.id
    }
}

// ---- Exit: no thread inside ONNX Runtime while its statics are torn down ----
//
// A process that ends through `exit` (a test binary, the CLI) runs ONNX Runtime's C++
// static destructors. A thread still inside a session load or a run then reads torn-down
// statics: SIGSEGV in `DataTypeImpl::GetDataType` under `CreateSession` (the `proxy` test
// binary, 2026-10-16: its analysis thread was still loading Parakeet, past the 10 s quit
// wait on a loaded Mac), or SIGABRT "recursive_mutex lock failed" from the 1DS telemetry
// worker torn down beside it. The app ends with `_exit` after its quit
// (`gmclip_gpu::apple::exit`); everything else registers [`quiesce`] to run at `exit`
// ahead of those destructors (the host's hook, [`set_exit_hook`]): it waits for the calls
// in flight to return and parks any thread that tries to enter after that.
//
// That covers the calls only. The 1DS worker races the teardown with no call in flight
// (the importsheet test binary, 2026-10-17), so the app's hook goes on, after `quiesce`,
// to end the process before the destructors when the exit is `main` returning
// (`gmclip_gpu::apple::exit::end_now_if_main_returned`).

/// Calls inside ONNX Runtime, and whether the process is exiting.
struct Gate {
    state: Mutex<(usize, bool)>,
    cv: Condvar,
}

impl Gate {
    const fn new() -> Self {
        Self {
            state: Mutex::new((0, false)),
            cv: Condvar::new(),
        }
    }

    /// One more call inside; once closing, parks the caller for good.
    fn enter(&self) {
        let mut g = self.state.lock().unwrap_or_else(|p| p.into_inner());
        while g.1 {
            g = self.cv.wait(g).unwrap_or_else(|p| p.into_inner());
        }
        g.0 += 1;
    }

    fn leave(&self) {
        let mut g = self.state.lock().unwrap_or_else(|p| p.into_inner());
        g.0 = g.0.saturating_sub(1);
        drop(g);
        self.cv.notify_all();
    }

    /// Closes the gate and waits up to `max` for the calls inside; true when none is.
    fn close(&self, max: Duration) -> bool {
        let mut g = self.state.lock().unwrap_or_else(|p| p.into_inner());
        g.1 = true;
        let t0 = Instant::now();
        while g.0 > 0 {
            let left = max.saturating_sub(t0.elapsed());
            if left.is_zero() {
                eprintln!("gmclip: {} ONNX Runtime calls still running at exit", g.0);
                return false;
            }
            g = self
                .cv
                .wait_timeout(g, left)
                .map(|(g, _)| g)
                .unwrap_or_else(|p| p.into_inner().0);
        }
        true
    }
}

static GATE: Gate = Gate::new();
/// How the host registers [`quiesce`] to run at exit (`gmclip_gpu::apple::exit::at_exit`).
static EXIT_HOOK: OnceLock<fn()> = OnceLock::new();
/// How long [`quiesce`] waits for the calls in flight: a session load on a loaded Mac
/// takes tens of seconds; a call that never returns must not hold the exit for good.
const QUIESCE_MAX: Duration = Duration::from_secs(120);

/// A call inside ONNX Runtime: a session load, a run and the outputs it returned. Held
/// for the call's whole scope; the exit waits for every one to drop.
#[must_use]
pub(crate) struct Inside {
    load: bool,
}

/// Enters ONNX Runtime (a `load` is a session creation, which registers more static
/// destructors: the exit hook is registered again after it). Once the process is
/// exiting, never returns: the thread parks until the process ends.
pub(crate) fn inside(load: bool) -> Inside {
    GATE.enter();
    if load {
        arm_exit();
    }
    Inside { load }
}

impl Drop for Inside {
    fn drop(&mut self) {
        GATE.leave();
        if self.load {
            arm_exit();
        }
    }
}

/// Sets how [`quiesce`] is registered to run at exit (once per process; later calls are
/// ignored), and registers it now when ONNX Runtime is already loaded. The app core calls
/// it with `gmclip_gpu::apple::exit::at_exit` on macOS.
pub fn set_exit_hook(register: fn()) {
    if EXIT_HOOK.set(register).is_ok() && RUNTIME.get().is_some() {
        register();
    }
}

fn arm_exit() {
    if let Some(register) = EXIT_HOOK.get() {
        register();
    }
}

/// Calls inside ONNX Runtime now (a session load, a run): what [`quiesce`] waits for.
pub fn calls_inside() -> usize {
    GATE.state.lock().map_or(0, |g| g.0)
}

/// At exit: no new call enters ONNX Runtime, and this waits (up to two minutes) for the
/// calls in flight to return. Idempotent: it is registered again after each model load.
pub extern "C" fn quiesce() {
    GATE.close(QUIESCE_MAX);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn closing_waits_for_the_call_inside_and_parks_new_ones() {
        let gate: &'static Gate = Box::leak(Box::new(Gate::new()));
        gate.enter();
        let closed = Arc::new(AtomicBool::new(false));
        let c = closed.clone();
        let t = std::thread::spawn(move || {
            let ok = gate.close(Duration::from_secs(60));
            c.store(true, Ordering::SeqCst);
            ok
        });
        std::thread::sleep(Duration::from_millis(200));
        assert!(!closed.load(Ordering::SeqCst), "closed with a call inside");
        gate.leave();
        assert!(t.join().unwrap());
        // a thread that tries to enter now never gets in
        let entered = Arc::new(AtomicBool::new(false));
        let e = entered.clone();
        std::thread::spawn(move || {
            gate.enter();
            e.store(true, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(200));
        assert!(!entered.load(Ordering::SeqCst));
        // and a call that never returns holds the exit only as long as allowed
        let stuck: &'static Gate = Box::leak(Box::new(Gate::new()));
        stuck.enter();
        assert!(!stuck.close(Duration::from_millis(50)));
    }
}
