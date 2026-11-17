//! Ending the process without C++ static teardown, once the app has shut down cleanly.
//!
//! ONNX Runtime 1.30 (macOS) starts Microsoft's 1DS telemetry system inside its `PosixEnv`
//! singleton even with telemetry events disabled (none are recorded: its offline store
//! stays empty). At `exit` the singleton's destructor flushes and tears that system down
//! while one of its own worker threads may still be dispatching, and the process aborts
//! with "recursive_mutex lock failed" (seen in the app's test binaries, 2026-10-16). The
//! app quits through here instead, after closing every project and stopping every ONNX
//! job, so a quit never ends in a crash report.
//!
//! A process that ends through `exit` (a test binary once its tests are done) ends the
//! same way from an atexit handler registered after ONNX Runtime loaded
//! ([`end_now_if_main_returned`]). Waiting for the calls inside ONNX Runtime is not enough
//! there: the 1DS worker is not a call anyone made. The importsheet test binary's abort
//! (2026-10-17) had no call in flight: the main thread was in `~PosixEnv` →
//! `PosixTelemetry::Shutdown` → `HttpClientManager::cancelAllRequests` while the 1DS
//! worker, answering the cancelled upload in `HttpClientManager::onHttpResponse`, locked a
//! `DebugEventSource` mutex already destroyed by the same teardown.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::io::Write;

/// dlfcn.h's `Dl_info`.
#[repr(C)]
struct DlInfo {
    fname: *const c_char,
    fbase: *mut c_void,
    sname: *const c_char,
    saddr: *mut c_void,
}

unsafe extern "C" {
    // unistd.h (libSystem): ends the process at once, without atexit handlers.
    fn _exit(status: i32) -> !;
    // stdlib.h (libSystem): runs `f` at `exit`, before every handler and C++ static
    // destructor registered earlier (one list, last in first out).
    fn atexit(f: extern "C" fn()) -> i32;
    // execinfo.h (libSystem): the calling thread's return addresses, innermost first.
    fn backtrace(frames: *mut *mut c_void, size: c_int) -> c_int;
    // dlfcn.h (libSystem): the image and nearest symbol at or before an address.
    fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int;
    // mach-o/dyld.h (libSystem): image 0 is the main executable.
    fn _dyld_get_image_header(index: u32) -> *const c_void;
    // pthread.h (libSystem): nonzero on the process's main thread.
    fn pthread_main_np() -> c_int;
}

/// Flushes stdout and stderr, then ends the process with `code`, skipping atexit
/// handlers and C++ static destructors. Call only after everything that writes has stopped.
pub fn exit_skipping_teardown(code: i32) -> ! {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: `_exit` takes any status and never returns; nothing Rust-side is left
    // half-done because the caller has already shut the app down and flushed the
    // standard streams.
    unsafe { _exit(code) }
}

/// Runs `f` when the process exits through `exit` (a test binary's end, a CLI's), before
/// the C++ static destructors registered so far: ONNX Runtime's are registered as it
/// loads, so registering after it loaded (and again after each model load, which can
/// register more) puts `f` ahead of them. `gmclip_ml::runtime::quiesce` is the `f`: it
/// waits for the threads inside ONNX Runtime and stops new ones from entering. Returns
/// false when the C library refused (out of memory).
pub fn at_exit(f: extern "C" fn()) -> bool {
    // SAFETY: `atexit` stores the function pointer and calls it once at exit, from the
    // exiting thread; `f` is a plain `extern "C" fn()` with no arguments to outlive.
    unsafe { atexit(f) == 0 }
}

/// Whether the exit now running was reached by the executable's `main` returning, rather
/// than by a call to `exit` from inside the program (`std::process::exit`, which is how a
/// test harness reports a failed test: 101). True only on the main thread with no frame of
/// the executable's own `main` left on its stack. Meant for an atexit handler.
pub fn main_has_returned() -> bool {
    // SAFETY: `pthread_main_np` only reads the calling thread's identity.
    if unsafe { pthread_main_np() } == 0 {
        // `exit` called on another thread: by the program, with a status we cannot see
        return false;
    }
    const DEPTH: usize = 256;
    let mut frames = [std::ptr::null_mut::<c_void>(); DEPTH];
    // SAFETY: `backtrace` writes at most `DEPTH` pointers into the array it is given.
    let n = unsafe { backtrace(frames.as_mut_ptr(), DEPTH as c_int) };
    if n <= 0 || n as usize >= DEPTH {
        // no stack, or one too deep to see its bottom: not known to have returned
        return false;
    }
    // SAFETY: `_dyld_get_image_header(0)` is the main executable's header, always loaded.
    let exe = unsafe { _dyld_get_image_header(0) };
    !frames.iter().take(n as usize).any(|&pc| {
        let mut info = DlInfo {
            fname: std::ptr::null(),
            fbase: std::ptr::null_mut(),
            sname: std::ptr::null(),
            saddr: std::ptr::null_mut(),
        };
        // SAFETY: `dladdr` only fills `info`; a return address is a valid query (one
        // outside every image just returns 0). Its strings belong to dyld and stay
        // valid while the image is loaded, which the main executable always is.
        unsafe {
            dladdr(pc, &mut info) != 0
                && info.fbase.cast_const() == exe
                && !info.sname.is_null()
                && CStr::from_ptr(info.sname).to_bytes() == b"main"
        }
    })
}

/// For an atexit handler in a process whose `main` returns `()` (a test harness, the
/// app): when this exit is `main` returning, the status is 0, so it ends the process here
/// with 0, skipping the handlers and C++ static destructors registered before the handler
/// (ONNX Runtime's `PosixEnv`, whose telemetry teardown races its own worker thread; see
/// the module docs). An explicit `exit(code)` carries a status this cannot read, so it
/// returns and that exit goes on as usual.
pub fn end_now_if_main_returned() {
    if main_has_returned() {
        exit_skipping_teardown(0);
    }
}
