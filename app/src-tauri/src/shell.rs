//! The Tauri shell (SPEC §12.1, R-UI-SEC-1): one window, two commands, one scheme.
//!
//! - `gm_call(req)` and `gm_subscribe(scope, onEvent)` are the only commands, and the
//!   capability grants exactly those (`capabilities/main.json`, R-SUP-2: AJ's file).
//! - `gm://data/…` serves binary columns ([`crate::ipc::gm_data`], R-IPC-1).
//! - The CSP is the spec's, with the frame socket's exact port written in at launch
//!   (the port is only known once it is bound); Trusted Types are required.
//! - Navigation away from the bundle origin is refused; new windows are refused.
//! - Files dropped on the window are offered to the UI as import candidates (a path the
//!   person chose by dropping it; the UI asks before importing).

use std::sync::Arc;

use serde_json::Value;
use tauri::Manager;
use tauri::webview::NewWindowResponse;

use crate::error::AppError;
use crate::ipc::{CallRequestWire, Ctx, check_scope, dispatch, gm_data, pump};

/// The webview's origin (exact; R-FRAME-1): the bundle origin in a built app, the Vite
/// dev server without `custom-protocol`. WebView2 (Windows) has no custom schemes: Tauri
/// serves the bundle at `http://tauri.localhost` there.
pub fn webview_origin() -> &'static str {
    if cfg!(feature = "custom-protocol") {
        if cfg!(windows) {
            "http://tauri.localhost"
        } else {
            "tauri://localhost"
        }
    } else {
        "http://localhost:5173"
    }
}

/// A `gm` request's URI in its `gm://` form: on Windows Tauri serves `gm://x` at
/// `http://gm.localhost/x` (WebView2 has no custom schemes), and wry hands the handler
/// that request as `gm://localhost/x` (measured in the built app); both are mapped back
/// to `gm://x` before the strict parser (`gmuri`) sees them. Elsewhere unchanged.
pub fn gm_uri(uri: &str) -> String {
    if cfg!(windows) {
        for p in [
            "gm://localhost/",
            "http://gm.localhost/",
            "https://gm.localhost/",
        ] {
            if let Some(rest) = uri.strip_prefix(p) {
                return format!("gm://{rest}");
            }
        }
    }
    uri.to_owned()
}

/// SR R-UI-SEC-1's CSP with the frame socket's port.
pub fn csp(frame_port: u16) -> String {
    // the `gm` scheme as WebView2 reaches it too (see [`gm_uri`])
    let gm = if cfg!(windows) {
        "gm: http://gm.localhost"
    } else {
        "gm:"
    };
    format!(
        "default-src 'self'; script-src 'self'; style-src 'self'; \
         object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'; \
         connect-src ipc: http://ipc.localhost {gm} ws://127.0.0.1:{frame_port}; \
         require-trusted-types-for 'script'; trusted-types default"
    )
}

/// Native menu items the UI acts on (sent as `app` scope `menu` events; `app_menu` sends
/// the same ones from the page).
pub const MENU_IDS: &[&str] = &[
    "open_video",
    "add_video",
    "save",
    "export",
    "undo",
    "redo",
    "keys",
    "hover_scrub",
    "safe_zones",
];

/// View › Hover Scrub and View › Safe Zones, kept in step with the saved preferences.
static VIEW_CHECKS: std::sync::OnceLock<[tauri::menu::CheckMenuItem<tauri::Wry>; 2]> =
    std::sync::OnceLock::new();

/// Ticks the View menu's check items to match the preferences (no-op without a window).
pub fn sync_view_menu(prefs: &Value) {
    if let Some([hover, safe]) = VIEW_CHECKS.get() {
        let _ = hover.set_checked(prefs["hover_scrub"].as_bool().unwrap_or(true));
        let _ = safe.set_checked(prefs["safe_zones"].as_bool().unwrap_or(false));
    }
}

/// Most live subscriptions one webview may hold.
const MAX_SUBS: usize = 64;

#[tauri::command]
async fn gm_call(state: tauri::State<'_, Ctx>, req: CallRequestWire) -> Result<Value, AppError> {
    let ctx = state.inner().clone();
    let name = req.command.clone();
    // Commands can block (imports, actor calls): off the async runtime.
    let out = tauri::async_runtime::spawn_blocking(move || dispatch(&ctx, req))
        .await
        .map_err(|_| AppError::internal("the call did not finish"))?;
    if std::env::var_os("GMCLIP_DEBUG_IPC").is_some() {
        // Shape only: the command name and whether it succeeded.
        eprintln!("gmclip ipc: {name} ok={}", out["ok"]);
    }
    Ok(out)
}

#[tauri::command]
fn gm_subscribe(
    state: tauri::State<'_, Ctx>,
    scope: String,
    on_event: tauri::ipc::Channel<Value>,
) -> Result<u64, AppError> {
    check_scope(&state.core, &scope)?;
    let mut subs = state
        .webview_subs
        .lock()
        .map_err(|_| AppError::internal("lock"))?;
    if subs.len() >= MAX_SUBS {
        return Err(AppError::invalid("too many subscriptions"));
    }
    let id = pump(state.core.clone(), scope, move |v| on_event.send(v).is_ok());
    subs.push(id);
    Ok(id)
}

fn navigation_allowed(url: &tauri::Url) -> bool {
    let origin = webview_origin();
    let o = url.origin().ascii_serialization();
    // `tauri://localhost` has an opaque origin in the URL crate: compare scheme + host.
    if origin.starts_with("tauri://") {
        url.scheme() == "tauri" && url.host_str() == Some("localhost")
    } else {
        o == origin
    }
}

/// Runs the app until the window closes.
pub fn run(ctx: Ctx) -> Result<(), String> {
    let core = ctx.core.clone();
    let mut context = tauri::generate_context!();
    context.config_mut().app.security.csp =
        Some(tauri::utils::config::Csp::Policy(csp(ctx.frame_port)));
    let data_core = ctx.core.clone();
    let origin = webview_origin().to_owned();
    let drop_core = ctx.core.clone();
    let picker = ctx.picker.clone();
    let picker_many = ctx.picker_many.clone();
    let hidden = ctx.frame_hidden.clone();
    let frame_session = ctx.frame_session.clone();
    let app = tauri::Builder::default()
        .manage(ctx)
        .invoke_handler(tauri::generate_handler![gm_call, gm_subscribe])
        // A reloaded page cannot receive its old channels: drop its subscriptions.
        .on_page_load(|webview, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Started {
                let ctx = webview.state::<Ctx>();
                if let Ok(mut subs) = ctx.webview_subs.lock() {
                    for id in subs.drain(..) {
                        ctx.core.bus.unsubscribe(id);
                    }
                }
            }
        })
        .register_asynchronous_uri_scheme_protocol("gm", move |_ctx, request, responder| {
            let core = Arc::clone(&data_core);
            let origin = origin.clone();
            let uri = gm_uri(&request.uri().to_string());
            std::thread::spawn(move || {
                let body = if uri.starts_with("gm://camera/") {
                    crate::recording::preview_frame(&uri)
                } else {
                    gm_data(&core, &uri)
                };
                let resp = match body {
                    Some(body) => tauri::http::Response::builder()
                        .status(200)
                        .header("Content-Type", "application/octet-stream")
                        .header("Access-Control-Allow-Origin", origin)
                        .header("Cache-Control", "no-store")
                        .body(body),
                    None => tauri::http::Response::builder()
                        .status(404)
                        .header("Access-Control-Allow-Origin", origin)
                        .body(Vec::new()),
                };
                if let Ok(r) = resp {
                    responder.respond(r);
                }
            });
        })
        .setup(move |app| {
            let w = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("gmclip")
            .inner_size(1440.0, 900.0)
            // every view lays out down to 480 × 360 (a phone-sized corner of the screen);
            // the panes fold into drawers and menus below that (AJ 2026-10-16)
            .min_inner_size(480.0, 360.0)
            .on_navigation(navigation_allowed)
            .on_new_window(|_, _| NewWindowResponse::Deny)
            // the page never opens a camera or mic itself (the Record sheet's preview
            // comes from the app's capture, `gm://camera`)
            .on_permission_request(|_, _| tauri::webview::PermissionResponse::Deny)
            .build()?;
            // R-FRAME-1: whether the window is hidden or minimised, checked every second;
            // the frame socket closes after 60 s of it.
            {
                let w = w.clone();
                let hidden = hidden.clone();
                let session = frame_session.clone();
                let _ = std::thread::Builder::new()
                    .name("gmclip-window-watch".into())
                    .spawn(move || {
                        loop {
                            std::thread::sleep(std::time::Duration::from_secs(1));
                            let shown = w.is_visible().unwrap_or(true)
                                && !w.is_minimized().unwrap_or(false);
                            hidden.set(!shown);
                            // shown again after the socket closed: a fresh token, and the
                            // UI reconnects by itself (decision 0010)
                            if shown
                                && hidden.closed.load(std::sync::atomic::Ordering::SeqCst)
                                && session.rearm()
                            {
                                hidden
                                    .closed
                                    .store(false, std::sync::atomic::Ordering::SeqCst);
                                eprintln!("gmclip: frame socket re-armed with a new token");
                            }
                        }
                    });
            }
            // The native menu (U.14): File and Edit route to the UI as `app` scope events
            // (the UI owns the cut in view); text editing items are the system's own.
            {
                use tauri::menu::{
                    CheckMenuItemBuilder, MenuBuilder, MenuItemBuilder, SubmenuBuilder,
                };
                let item = |id: &str, text: &str, acc: &str| {
                    MenuItemBuilder::with_id(id, text)
                        .accelerator(acc)
                        .build(app)
                };
                let app_menu = SubmenuBuilder::new(app, "gmclip")
                    .about(None)
                    .separator()
                    .services()
                    .separator()
                    .hide()
                    .hide_others()
                    .show_all()
                    .separator()
                    .quit()
                    .build()?;
                let file = SubmenuBuilder::new(app, "File")
                    .item(&item("open_video", "Open Video…", "CmdOrCtrl+O")?)
                    .item(&item(
                        "add_video",
                        "Add Video to Project…",
                        "CmdOrCtrl+Shift+O",
                    )?)
                    .separator()
                    .item(&item("save", "Save Restore Point", "CmdOrCtrl+S")?)
                    .item(&item("export", "Export…", "CmdOrCtrl+E")?)
                    .separator()
                    .close_window();
                // Windows has no app menu: Exit ends File, About sits under Help
                let file = if cfg!(target_os = "macos") {
                    file
                } else {
                    file.separator().quit_with_text("Exit")
                }
                .build()?;
                // ⌘Z / ⌘⇧Z are the system's own items, so a focused text field undoes its
                // typing natively; outside a field the page's keydown undoes the project
                // first (and the system item then finds nothing to undo). The project's own
                // items carry no shortcut, for the mouse.
                let plain = |id: &str, text: &str| MenuItemBuilder::with_id(id, text).build(app);
                let edit = SubmenuBuilder::new(app, "Edit")
                    .undo()
                    .redo()
                    .separator()
                    .item(&plain("undo", "Undo Last Edit")?)
                    .item(&plain("redo", "Redo Last Edit")?)
                    .separator()
                    .cut()
                    .copy()
                    .paste()
                    .select_all()
                    .build()?;
                // The check items show the saved state; the UI owns the toggle (a click
                // sends the menu event, the UI flips the preference, the save re-ticks).
                let prefs = crate::uiprefs::load(&drop_core.dirs.app_data);
                let hover = CheckMenuItemBuilder::with_id("hover_scrub", "Hover Scrub")
                    .accelerator("Alt+S")
                    .checked(prefs["hover_scrub"].as_bool().unwrap_or(true))
                    .build(app)?;
                let safe = CheckMenuItemBuilder::with_id("safe_zones", "Safe Zones")
                    .checked(prefs["safe_zones"].as_bool().unwrap_or(false))
                    .build(app)?;
                let view = SubmenuBuilder::new(app, "View")
                    .item(&hover)
                    .item(&safe)
                    .separator()
                    .item(&item("keys", "Keyboard Shortcuts", "Shift+CmdOrCtrl+/")?)
                    .build()?;
                let _ = VIEW_CHECKS.set([hover, safe]);
                let window = SubmenuBuilder::new(app, "Window")
                    .minimize()
                    .fullscreen()
                    .build()?;
                let menu = if cfg!(target_os = "macos") {
                    MenuBuilder::new(app)
                        .items(&[&app_menu, &file, &edit, &view, &window])
                        .build()?
                } else {
                    let help = SubmenuBuilder::new(app, "Help")
                        .about_with_text("About gmclip", None)
                        .build()?;
                    MenuBuilder::new(app)
                        .items(&[&file, &edit, &view, &window, &help])
                        .build()?
                };
                app.set_menu(menu)?;
                let bus_core = drop_core.clone();
                app.on_menu_event(move |_, ev| {
                    let id = ev.id().0.as_str();
                    if MENU_IDS.contains(&id) {
                        bus_core
                            .bus
                            .app(crate::events::AppEvent::Menu { id: id.to_owned() });
                    }
                });
            }
            let sheet_handle = app.handle().clone();
            crate::sheet::install(Box::new(move |title, text, action| {
                let (tx, rx) = std::sync::mpsc::sync_channel(1);
                if sheet_handle
                    .run_on_main_thread(move || {
                        let _ = tx.send(crate::sheet::alert_on_main_thread(&title, &text, &action));
                    })
                    .is_err()
                {
                    return false;
                }
                rx.recv().unwrap_or(false)
            }));
            let share_handle = app.handle().clone();
            crate::share::install(Box::new(move |f| {
                share_handle.run_on_main_thread(f).is_ok()
            }));
            // the native file panels: AppKit's, and on Windows the system file dialog
            // (W.9); elsewhere none is installed and the calls answer Unsupported
            // (ipc::no_panel)
            #[cfg(any(target_os = "macos", windows))]
            let handle = app.handle().clone();
            #[cfg(any(target_os = "macos", windows))]
            let _ = picker.set(Box::new(move || {
                let (tx, rx) = std::sync::mpsc::sync_channel(1);
                handle
                    .run_on_main_thread(move || {
                        let _ = tx.send(crate::video::pick_on_main_thread());
                    })
                    .ok()?;
                rx.recv().ok().flatten()
            }));
            #[cfg(any(target_os = "macos", windows))]
            let handle = app.handle().clone();
            #[cfg(any(target_os = "macos", windows))]
            crate::owner::install_style_picker(Box::new(move || {
                let (tx, rx) = std::sync::mpsc::sync_channel(1);
                handle
                    .run_on_main_thread(move || {
                        let _ = tx.send(crate::owner::pick_style_on_main_thread());
                    })
                    .ok()?;
                rx.recv().ok().flatten()
            }));
            #[cfg(any(target_os = "macos", windows))]
            let handle = app.handle().clone();
            #[cfg(any(target_os = "macos", windows))]
            let _ = picker_many.set(Box::new(move || {
                let (tx, rx) = std::sync::mpsc::sync_channel(1);
                if handle
                    .run_on_main_thread(move || {
                        let _ = tx.send(crate::video::pick_many_on_main_thread());
                    })
                    .is_err()
                {
                    return Vec::new();
                }
                rx.recv().unwrap_or_default()
            }));
            let core = drop_core.clone();
            let win = w.clone();
            w.on_window_event(move |e| {
                if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop {
                    paths,
                    position,
                }) = e
                {
                    let paths: Vec<String> = paths
                        .iter()
                        .take(16)
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect();
                    let k = win.scale_factor().unwrap_or(1.0).max(0.1);
                    let at = Some([position.x / k, position.y / k]);
                    core.bus
                        .home(crate::events::HomeEvent::Dropped { paths, at });
                }
            });
            let _ = app.get_webview_window("main");
            Ok(())
        })
        .build(context)
        .map_err(|e| format!("tauri: {e}"))?;
    app.run(move |_, e| {
        if let tauri::RunEvent::Exit = e {
            core.shutdown();
            // everything is closed: end here, before ONNX Runtime's static teardown can
            // race its own telemetry thread (gmclip_gpu::apple::exit)
            #[cfg(target_os = "macos")]
            gmclip_gpu::apple::exit::exit_skipping_teardown(0);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gm_requests_reach_the_parser_in_their_gm_form() {
        assert_eq!(gm_uri("gm://data/p/parts?cut=c"), "gm://data/p/parts?cut=c");
        let win = gm_uri("http://gm.localhost/data/p/parts?cut=c");
        if cfg!(windows) {
            assert_eq!(win, "gm://data/p/parts?cut=c");
            // what wry hands the handler on WebView2
            assert_eq!(
                gm_uri("gm://localhost/data/p/parts?cut=c"),
                "gm://data/p/parts?cut=c"
            );
            assert!(csp(1).contains(
                "connect-src ipc: http://ipc.localhost gm: http://gm.localhost ws://127.0.0.1:1;"
            ));
        } else {
            // only WebView2 serves custom schemes over http
            assert_eq!(win, "http://gm.localhost/data/p/parts?cut=c");
            assert!(csp(1).contains("connect-src ipc: http://ipc.localhost gm: ws://127.0.0.1:1;"));
        }
        // nothing else is rewritten
        assert_eq!(
            gm_uri("http://gm.localhost.evil/data/p"),
            "http://gm.localhost.evil/data/p"
        );
        assert_eq!(
            gm_uri("http://evil/gm.localhost/data/p"),
            "http://evil/gm.localhost/data/p"
        );
    }
}
