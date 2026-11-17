//! gmclip for Mac (SPEC §12, M2): the Tauri shell around gmclip-core.
//!
//! One app process hosts every open project's actor in-process (R-ARCH-2). The UI
//! reaches the core through `gm_call` / `gm_subscribe` / `gm://data` ([`ipc`], [`shell`]);
//! preview frames come from the render thread ([`viewer`]) over the frame socket
//! ([`frame`], SPEC D5). [`devbridge`] (feature `dev-bridge`, never in release) carries
//! the same IPC over a loopback WebSocket for Playwright.
#![forbid(unsafe_code)]
// Off macOS the media paths (VideoToolbox, AppKit panels, CoreAudio out, the Metal
// preview) are stubs until M5, so code only they reach is dead there; the Windows check
// (tools/check_windows.sh, decisions/0019) builds it anyway. macOS is unaffected.
#![cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, unused_imports, unused_mut, unused_variables)
)]

// R-DEV-1: the bridge is a debug-build tool; an optimised build cannot carry it.
#[cfg(all(feature = "dev-bridge", not(debug_assertions)))]
compile_error!("the `dev-bridge` feature is for debug builds only (R-DEV-1)");

pub mod agent;
pub mod agentapp;
pub mod analysis;
#[cfg(feature = "dev-bridge")]
pub mod analysiscache;
pub mod applog;
pub mod autosync;
pub mod backdrop;
pub mod backups;
pub mod batch;
pub mod board;
pub mod bothexport;
pub mod broll;
pub mod bus;
pub mod caps;
pub mod catalog;
pub mod chat;
pub mod checks;
pub mod clientcheck;
pub mod clips;
pub mod collab;
pub mod columns;
pub mod consolidate;
pub mod core;
pub mod cursorfollow;
pub mod cutlike;
pub mod cutnew;
#[cfg(feature = "dev-bridge")]
pub mod devbridge;
pub mod drawlayout;
pub mod endcard;
pub mod error;
pub mod events;
pub mod exportverify;
pub mod firstrun;
pub mod fonts;
pub mod frame;
pub mod framerec;
pub mod framing;
pub mod gizmos;
pub mod gmuri;
pub mod golden;
pub mod history;
pub mod ideas;
pub mod importsheet;
pub mod inputs;
pub mod ipc;
pub mod jobs;
pub mod labfixture;
pub mod lanes;
pub mod libmeaning;
pub mod library;
pub mod libsearch;
pub mod longcompare;
pub mod models;
pub mod openings;
pub mod owner;
pub mod perf;
pub mod planner;
pub mod playback;
pub mod polish;
pub mod proxy;
pub mod qos;
pub mod reactions;
pub mod recording;
pub mod recentthumbs;
pub mod retention;
pub mod review;
pub mod reviewlink;
pub mod roto;
pub mod screenstudio;
pub mod sessions;
pub mod share;
pub mod sheet;
pub mod shell;
pub mod short;
pub mod sound;
pub mod storage;
pub mod story;
pub mod streamedit;
pub mod studio;
pub mod takelayout;
pub mod takeover;
pub mod tapmark;
pub mod termlist;
pub mod thumbs;
pub mod tighten;
pub mod transcript;
pub mod uiprefs;
pub mod video;
pub mod viewer;
pub mod vroutes;
pub mod words;

use std::sync::Arc;

/// Command-line options (a second launch forwarding its arguments is R-ARCH-5's, later).
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// `--dev-bridge`: serve IPC on a loopback WebSocket (dev builds only).
    pub dev_bridge: bool,
    /// `--dev-bridge-origin <origin>`: the harness page's exact origin.
    pub dev_bridge_origin: Option<String>,
    /// `--no-window`: core + bridge only (Playwright drives the UI in a browser).
    pub no_window: bool,
    /// `--open <project home>`.
    pub open: Option<String>,
    /// `--s2a`: measure the preview on launch (S2a), write the report, quit.
    pub s2a: bool,
    /// `--perf-cold-start` (U.18): print the time from process start to the UI's first
    /// render (`GMCLIP_PERF cold_start_ms=…`) and quit.
    pub perf_cold_start: bool,
}

/// The file part of a source's stored path (after the last `/`; on Windows after a `\` too,
/// where stored paths use either separator).
pub(crate) fn file_part(path: &str) -> &str {
    let seps: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };
    path.rsplit(seps).next().unwrap_or(path)
}

/// When the process started (set first thing in `main`; U.18's cold start).
pub static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut o = Options::default();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dev-bridge" => o.dev_bridge = true,
            "--dev-bridge-origin" => {
                o.dev_bridge_origin = Some(it.next().ok_or("--dev-bridge-origin needs a value")?)
            }
            "--no-window" => o.no_window = true,
            "--s2a" => o.s2a = true,
            "--perf-cold-start" => o.perf_cold_start = true,
            "--open" => o.open = Some(it.next().ok_or("--open needs a path")?),
            // macOS adds this when launched from Finder.
            s if s.starts_with("-psn_") => {}
            s => {
                return Err(format!(
                    "unknown argument `{}`",
                    s.chars().take(40).collect::<String>()
                ));
            }
        }
    }
    if o.dev_bridge && !cfg!(feature = "dev-bridge") {
        return Err(
            "this build has no dev bridge (R-DEV-1): build with `--features dev-bridge`".into(),
        );
    }
    if o.no_window && !o.dev_bridge {
        return Err("--no-window only makes sense with --dev-bridge".into());
    }
    Ok(o)
}

/// Starts everything and runs until quit.
pub fn run(opts: Options) -> Result<(), String> {
    let dirs = core::Dirs::from_env().map_err(|e| e.message)?;
    // Windows: one app per user's app data (macOS's LaunchServices keeps one); a second
    // launch brings the running window to the front and leaves. Not for the dev bridge or
    // --no-window (tests run several cores side by side).
    #[cfg(windows)]
    let _instance = if opts.dev_bridge || opts.no_window || opts.s2a || opts.perf_cold_start {
        None
    } else {
        match windows_instance(&dirs.app_data) {
            Some(i) => i,
            None => return Ok(()),
        }
    };
    let app = core::AppCore::new(dirs);
    let bridge_origin = opts
        .dev_bridge_origin
        .clone()
        .unwrap_or_else(|| "http://127.0.0.1:4173".into());
    // The frame socket accepts the webview's exact origin; with --no-window the browser
    // page under test is the viewer.
    let frame_origin = if opts.no_window {
        bridge_origin.clone()
    } else {
        shell::webview_origin().to_owned()
    };
    let (fs, listener) =
        frame::FrameServer::bind(&frame_origin).map_err(|e| format!("frame socket: {e}"))?;
    let slot = Arc::new(frame::FrameSlot::default());
    let (rtx, rrx) = std::sync::mpsc::sync_channel(64);
    fs.serve(listener, slot.clone(), rtx)
        .map_err(|e| format!("frame socket: {e}"))?;
    let viewer = viewer::spawn(&app, slot, rrx, fs.connected.clone());
    // rolling project backups every 30 min of editing (lane S; at quit in `shutdown`)
    backups::start(&app);
    // R-PLAT-1 at startup (H.09): what VideoToolbox offers on this Mac, asked once
    caps::probe_at_startup();
    // The agent server (the holder) in this process, on the gmclip socket.
    if let Ok(env) = gmclip_mcp::paths::Env::from_env() {
        app.agents.start(&app, env);
    }
    let ctx = ipc::Ctx {
        core: app.clone(),
        jobs: Arc::new(jobs::Jobs::default()),
        viewer,
        frame_port: fs.port,
        frame_session: fs.session.clone(),
        frame_hidden: fs.hidden.clone(),
        dev_bridge: opts.dev_bridge,
        autorun: opts.s2a.then(|| "s2a".to_owned()),
        perf_cold_start: opts.perf_cold_start,
        webview_subs: Arc::default(),
        picker: Arc::default(),
        picker_many: Arc::default(),
    };
    // agents run the window's commands too (agentapp's list; paths inside their roots)
    if let Some(h) = app.agents.holder() {
        h.set_app(Arc::new(agentapp::Host(ctx.clone())));
    }
    if let Some(home) = &opts.open {
        app.open_home(std::path::Path::new(home))
            .map_err(|e| e.message)?;
    }
    #[cfg(feature = "dev-bridge")]
    if opts.dev_bridge {
        agent::start_stub_from_env()?;
        sessions::fake_from_env(&ctx.core);
        if signed_for_release() {
            return Err("a Developer ID-signed build never starts the dev bridge (R-DEV-1)".into());
        }
        let (port, token) =
            devbridge::start(ctx.clone(), bridge_origin).map_err(|e| format!("dev bridge: {e}"))?;
        println!(
            "GMCLIP_DEV_BRIDGE {}",
            serde_json::json!({"port": port, "token": token, "frame_port": fs.port})
        );
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
    }
    if opts.no_window {
        // Park until killed; the bridge and the render thread do the work.
        loop {
            std::thread::park();
        }
    }
    shell::run(ctx)
}

/// Whether this executable carries a Developer ID signature (R-DEV-1: such a build refuses
/// the bridge even if it was compiled in).
#[cfg(feature = "dev-bridge")]
fn signed_for_release() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return true;
    };
    if !cfg!(target_os = "macos") {
        return false;
    }
    match std::process::Command::new("/usr/bin/codesign")
        .arg("-dvv")
        .arg(&exe)
        .output()
    {
        Ok(o) => String::from_utf8_lossy(&o.stderr).contains("Authority=Developer ID Application"),
        Err(_) => true,
    }
}

/// Claims the app's single instance for `app_data` (named from its path, so isolated
/// homes and tag-separated runs are separate apps); `None` when another process has it,
/// after bringing that process's window to the front. A failure to make the mutex lets
/// this launch run.
#[cfg(windows)]
fn windows_instance(
    app_data: &std::path::Path,
) -> Option<Option<gmclip_platform::windows::Instance>> {
    use gmclip_platform::windows::{Instance, focus_process_window};
    // FNV-1a over the folder's lower-cased path: the same folder, the same name
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in app_data.to_string_lossy().to_lowercase().bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    let pid_file = app_data.join("instance.pid");
    match Instance::claim(&format!("gmclip-app-{h:016x}")) {
        Ok(Some(i)) => {
            let _ = std::fs::create_dir_all(app_data);
            let _ = std::fs::write(&pid_file, std::process::id().to_string());
            Some(Some(i))
        }
        Ok(None) => {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
            {
                focus_process_window(pid);
            }
            None
        }
        Err(_) => Some(None),
    }
}
