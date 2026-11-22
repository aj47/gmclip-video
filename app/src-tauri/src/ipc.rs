//! The UI wire (SPEC §12.1): one `gm_call` carrying every UI command, `gm_subscribe`
//! scopes coalesced per 16 ms, and the `gm://data/…` binary columns (R-IPC-1).
//!
//! `gm_call` routes two kinds of names:
//! - **registry commands** (`gmclip_core::registry`), run on the project's actor as the
//!   local human (`Caller::human()`, base filled automatically, R-CC-1). Owner commands
//!   are not in the registry at all (R-OWN-1) and a gesture nonce is not accepted yet:
//!   the native sheet (R-OWN-1b) is the next M2 step, so `gesture` is refused;
//! - **app-shell commands** ([`APP_COMMANDS`]): opening projects, importing, the viewer,
//!   exports, Home. They are thin: anything that changes a cut goes through a registry
//!   command (frame → flicks conversions happen here so the UI does no model math,
//!   R-CMD-2).
//!
//! The same [`dispatch`] serves Tauri and the dev bridge.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::core::{AppCore, now_ms};
use crate::error::{AppError, AppResult, reply_err};
use crate::jobs::Jobs;
use crate::viewer::{Viewer, ViewerMsg};

/// `gm_call`'s request.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallRequestWire {
    pub command: String,
    #[serde(default)]
    pub args: Value,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    /// An Owner nonce from the native sheet (R-OWN-1b). Not accepted until the sheet
    /// exists.
    #[serde(default)]
    pub gesture: Option<Value>,
}

/// Every app-shell command, once (the UI's `actions.ts` names these and registry ones).
pub const APP_COMMANDS: &[&str] = &[
    "app_info",
    // library search (L.01)
    "library_search",
    "library_play",
    "library_open",
    "library_topic_open",
    "library_moment",
    // ideas 2026-10-16 (library and layout; lane K4: `crate::ideas`)
    "layout_draw_view",
    "layout_draw_frame",
    "layout_draw_save",
    "term_fix_everywhere",
    "terms_view",
    "longform_compare_view",
    "broll_suggest",
    "broll_add",
    // backdrops and video overlays (lane K3, decision 0028: `crate::backdrop`)
    "backdrop_set",
    "video_overlay_add",
    "overlay_track",
    "cover_track",
    "cover_add",
    "reactions_find",
    // ideas 2026-10-16 (edit polish; lane K2: `crate::polish`)
    "polish_view",
    "polish_apply",
    "holds_find",
    // chat logs (L.03), tap marks (L.04)
    "chat_import",
    "chat_view",
    "chat_nudge",
    "chat_ribbon",
    "chat_moment",
    "tap_mark",
    // new cut like this (L.05)
    "cut_like",
    "cut_template_save",
    "cut_templates",
    "cut_template_delete",
    // consolidate (L.06)
    "project_consolidate",
    "project_open_copy",
    "app_frame_socket",
    "app_ready",
    "app_unsubscribe",
    "project_list_open",
    "project_new",
    "project_open_home",
    "project_close",
    "cut_import",
    "cut_view",
    "cut_checks",
    "check_fix",
    "history_list",
    "history_diff",
    "history_count",
    "history_restore",
    "snapshot_named",
    "cut_trim",
    "cut_split",
    // keyboard editing (lane K5): ripple-delete the I/O range
    "cut_range_delete",
    // 9:16 and 16:9 together (lane K5: `crate::bothexport`)
    "export_framings",
    "framing_make",
    "export_both",
    // end card to the long-form (lane K5: `crate::endcard`)
    "end_card_longform_view",
    "end_card_longform_add_app",
    "end_card_longform_remove",
    "layer_place_px",
    "layer_crop_px",
    "viewer_load",
    "stream_view",
    "hold_at",
    "caption_keep",
    "caption_y",
    "term_gate",
    "term_add",
    "viewer_scrub",
    "pair_propose",
    "viewer_unload",
    "viewer_seek",
    "proxy_status",
    "moment_clip",
    "viewer_play_range",
    "viewer_play_src",
    "viewer_play",
    "viewer_pause",
    "viewer_toggle",
    "viewer_step",
    "viewer_full",
    "viewer_stress",
    "viewer_rate",
    "viewer_select",
    "viewer_group",
    "viewer_state",
    "export_start",
    "export_cancel",
    "export_verify",
    "caps_startup",
    "matte_status",
    "matte_build",
    "matte_progress",
    "follow_action",
    "follow_cursor",
    "viewer_matte_view",
    "jobs_list",
    "exports_list",
    "export_reveal",
    "export_share",
    "export_drag",
    "retention_import",
    "retention_clippy",
    "retention_view",
    // three openings, one short (decision 0024)
    "openings_view",
    "opening_preview",
    "opening_export",
    "opening_add_words",
    "home",
    "home_seen",
    "app_s2a_report",
    "moments_list",
    "story_view",
    "part_time_set",
    "part_nudge_ms",
    "part_move",
    "cut_chronological",
    "cut_words",
    "words_strike_ord",
    "words_restore_ord",
    "word_nudge_ord",
    "word_respell",
    "word_edge_reset",
    "words_add_part",
    "words_stretch_last",
    "moment_review",
    "video_open",
    "video_inspect",
    // recording (decision 0045)
    "recording_status",
    "recording_enable",
    "recording_ask",
    "recording_preview",
    "recording_sources",
    "recording_prepare",
    "recording_go",
    "recording_marker",
    "recording_stop",
    "recording_cancel",
    "recording_open",
    "recording_reveal",
    // the Screen Studio look in one click (decision 0046)
    "screen_studio_apply",
    "zoom_auto_app",
    // several videos in one project (AJ 2026-10-15)
    "videos_inspect",
    "videos_open",
    "videos_add",
    "videos_pick",
    "video_remove",
    "video_relink",
    "files_dropped",
    "picture_offset_set",
    "sound_view",
    "loudness_set",
    "cut_from_moment",
    "run_measure_src",
    "gain_span_add",
    "gain_span_delete",
    "music_add",
    "part_gain_set",
    "cam_hide_add",
    "cam_hide_delete",
    "no_captions_add",
    "no_captions_delete",
    "agent_client_allow",
    "video_pick",
    "videos_recent",
    "video_thumb",
    "analysis_status",
    "transcript_view",
    "cut_output_set",
    "layout_preset",
    "layout_apply_all",
    "layer_crop_norm",
    "inset_add",
    "hook_layout_keep",
    "inset_delete",
    "layout_copy_next",
    "layout_copy_same_source",
    "layout_presets",
    "layout_preset_save",
    "layout_preset_apply",
    "layout_preset_delete",
    "layout_preset_rename",
    "short_view",
    "captions_set",
    "caption_fix",
    "caption_replace",
    "cue_break",
    "caption_keywords",
    "hook_set",
    "pill_add",
    "run_measure",
    "clock_add",
    "short_overlay_delete",
    // the look: caption gallery, style library (`studio`)
    "caption_gallery",
    "caption_style_apply",
    "caption_style_save_mine",
    "caption_style_forget_mine",
    "caption_style_match",
    "captions_file",
    "image_overlay_add",
    "image_thumb",
    "callout_add",
    "overlays_view",
    "brand_kit_view",
    "brand_kit_save",
    "brand_kit_use_mine",
    "brand_logo_set",
    "lower_third_add_at",
    "end_card_add_app",
    "overlay_template_save_app",
    "overlay_template_insert_at",
    "style_items_view",
    "style_item_save_mine",
    "style_item_forget_mine",
    "style_item_apply",
    "plate_save",
    "plates_view",
    "timeline_audio",
    "timeline_lanes",
    "timeline_part_energy",
    "timeline_gesture_begin",
    "timeline_gesture_end",
    "viewer_gizmos",
    "viewer_text_drag",
    "viewer_text_drag_end",
    "viewer_trim_peek",
    "viewer_trim_peek_end",
    "overlay_drag",
    "caption_seat",
    "commands_catalog",
    "overlay_time_set",
    "overlay_split_at",
    "gain_segment_edit",
    "sweep_preview",
    "sweep_apply_app",
    "hold_toggle",
    "part_reorder_to",
    "parts_move_to",
    "project_save_state",
    "project_restore_point",
    "app_menu",
    "agent_status",
    "agent_run",
    "batch_start",
    "batch_list",
    "owner_request",
    "owner_settings",
    "style_pick",
    "style_review",
    // lane S: backups, storage, first run
    "backups_overview",
    "backup_now",
    "backup_restore",
    "storage_usage",
    "storage_free",
    "setup_preview",
    "setup_state",
    "setup_apply",
    "setup_no_transcribe",
    "checklist_view",
    "checklist_dismiss",
    "agent_client_set",
    "agent_clients",
    "agent_default_set",
    "agent_continue",
    "models_status",
    "models_download",
    "models_pause",
    "board_view",
    "review_status",
    "review_serve",
    "review_link_new",
    "review_link_revoke",
    "export_verdict",
    "feedback_preview",
    "feedback_append",
    "clippy_pair",
    "clippy_send",
    "activity",
    "first_seen_ack",
    "agents_pause",
    "approval_resolve",
    "approval_export",
    "agent_run_stop",
    // decision 0035: agent sessions over ACP
    "session_agents",
    "session_list",
    "session_get",
    "session_start",
    "session_prompt",
    "session_cancel",
    "session_stop",
    "session_answer",
    "session_mode",
    "session_model",
    "session_resume",
    "session_forget",
    "session_undo",
    "ui_prefs_get",
    "ui_prefs_set",
    "stream_overlay",
    "project_map_view",
    "topic_label_ord",
    "topic_edge_ord",
    "topic_split_ord",
    "framing_pick",
    // lane V (decision 0015): comments, review-link asks, golden cuts, thumbnails
    "comments_view",
    "comment_add_at",
    "comments_fix",
    "review_ask",
    "review_link_role",
    "golden_view",
    "golden_mark",
    "golden_compare",
    "thumbs_view",
    "thumb_add_at",
    "thumb_cover",
    "thumb_render",
    "thumb_agent",
];

/// What every call can reach.
#[derive(Clone)]
pub struct Ctx {
    pub core: Arc<AppCore>,
    pub jobs: Arc<Jobs>,
    pub viewer: Viewer,
    pub frame_port: u16,
    /// The frame session's credential (token + generation, re-armed on show).
    pub frame_session: Arc<crate::frame::FrameSession>,
    /// R-FRAME-1: the window's hidden/minimised state, which closes the frame socket.
    pub frame_hidden: Arc<crate::frame::HiddenGate>,
    pub dev_bridge: bool,
    /// `--s2a`: the UI runs the S2a measurement on launch and reports back.
    pub autorun: Option<String>,
    /// `--perf-cold-start`: `app_ready` prints the cold-start time and quits.
    pub perf_cold_start: bool,
    /// The webview's live subscriptions (dropped when the page reloads; capped).
    pub webview_subs: std::sync::Arc<std::sync::Mutex<Vec<u64>>>,
    /// Runs the native "Open video…" panel on the main thread (set once the window
    /// exists; absent with --no-window).
    pub picker: Arc<std::sync::OnceLock<Picker>>,
    /// The same panel with several files allowed ("Add video…").
    pub picker_many: Arc<std::sync::OnceLock<PickerMany>>,
}

/// Shows the native file panel and waits for the person's choice.
pub type Picker = Box<dyn Fn() -> Option<PathBuf> + Send + Sync>;
/// Shows the native file panel for several files (empty when cancelled).
pub type PickerMany = Box<dyn Fn() -> Vec<PathBuf> + Send + Sync>;

impl Ctx {
    /// The app's commands with no window (no viewer, frame socket or file panels): what
    /// an agent's `command_run` reaches in tests.
    pub fn headless(core: Arc<AppCore>, jobs: Arc<Jobs>) -> Ctx {
        Ctx {
            core,
            jobs,
            viewer: Viewer::detached(),
            frame_port: 0,
            frame_session: Arc::new(crate::frame::FrameSession::new([0; 32])),
            frame_hidden: Arc::new(crate::frame::HiddenGate::new(crate::frame::HIDDEN_LIMIT)),
            dev_bridge: false,
            autorun: None,
            perf_cold_start: false,
            webview_subs: Arc::default(),
            picker: Arc::default(),
            picker_many: Arc::default(),
        }
    }
}

impl std::fmt::Debug for Ctx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx")
            .field("frame_port", &self.frame_port)
            .finish_non_exhaustive()
    }
}

fn s<'a>(args: &'a Value, k: &str) -> AppResult<&'a str> {
    args.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::invalid(format!("`{k}` is required")))
}

fn n(args: &Value, k: &str) -> AppResult<f64> {
    args.get(k)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .ok_or_else(|| AppError::invalid(format!("`{k}` is a number")))
}

fn strings(args: &Value, k: &str) -> AppResult<Vec<String>> {
    args.get(k)
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty() && v.len() <= crate::video::MAX_FILES)
        .ok_or_else(|| AppError::invalid(format!("`{k}`: 1 to 16 paths")))
}

/// The files of a multi-file open or add, and the same-recording pairs among them.
type FilesAndPairs = (Vec<crate::video::AddFile>, Vec<(usize, usize)>);

/// `{files: [{path, voice?}], same: [[i, j]]}` of a multi-file open or add.
fn add_files(a: &Value) -> AppResult<FilesAndPairs> {
    let files: Vec<crate::video::AddFile> = a
        .get("files")
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .filter_map(|f| {
                    Some(crate::video::AddFile {
                        path: PathBuf::from(f.get("path")?.as_str()?),
                        voice: f
                            .get("voice")
                            .and_then(Value::as_u64)
                            .and_then(|x| u32::try_from(x).ok()),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if files.is_empty() || files.len() > crate::video::MAX_FILES {
        return Err(AppError::invalid("`files`: 1 to 16 videos"));
    }
    let same = a
        .get("same")
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .filter_map(|p| {
                    let p = p.as_array()?;
                    Some((
                        usize::try_from(p.first()?.as_u64()?).ok()?,
                        usize::try_from(p.get(1)?.as_u64()?).ok()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok((files, same))
}

fn project_of(req: &CallRequestWire) -> AppResult<&str> {
    req.project
        .as_deref()
        .or_else(|| req.args.get("project").and_then(Value::as_str))
        .ok_or_else(|| AppError::invalid("`project` is required"))
}

/// Runs one call; the reply is `{ok: true, output, as_of?}` or `{ok: false, error}`.
pub fn dispatch(ctx: &Ctx, req: CallRequestWire) -> Value {
    match dispatch_inner(ctx, &req) {
        Ok(v) => {
            let mut out = json!({"ok": true});
            if let Some(o) = v.get("output").filter(|_| v.get("as_of").is_some()) {
                out["output"] = o.clone();
                out["as_of"] = v["as_of"].clone();
            } else {
                out["output"] = v;
            }
            out
        }
        Err(e) => {
            crate::applog::failed(&req.command, &e.code, &e.message);
            reply_err(&e)
        }
    }
}

pub(crate) fn dispatch_inner(ctx: &Ctx, req: &CallRequestWire) -> AppResult<Value> {
    if req.gesture.is_some() {
        return Err(AppError::new(
            "Unauthorized",
            "Owner actions need the native confirmation sheet, which is not built yet (R-OWN-1b)",
        ));
    }
    if req.command.len() > 64
        || !req
            .command
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(AppError::invalid("unknown command"));
    }
    let a = &req.args;
    let core = &ctx.core;
    // a window action on a library project that is not open opens it first (a
    // session's Open after a restart, a link from Home); one that is not in the library
    // fails as before
    if req.command != "project_close"
        && let Some(id) = req
            .project
            .as_deref()
            .or_else(|| a.get("project").and_then(Value::as_str))
        && !core.is_open(id)
    {
        let _ = core.ensure_open(id);
    }
    match req.command.as_str() {
        "app_info" => Ok(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "platform": std::env::consts::OS,
            "library": core.dirs.library,
            "exports": core.dirs.exports,
            "dev_bridge": ctx.dev_bridge,
            "autorun": ctx.autorun,
            "registry": gmclip_core::REGISTRY.iter().map(|c| c.meta().name).collect::<Vec<_>>(),
            "app_commands": APP_COMMANDS,
        })),
        // R-FRAME-1: the token travels over IPC only.
        // Dev bridge builds only: the hide → close → show cycle of decision 0010, so the
        // e2e can watch the UI reconnect (the window watch does this for real).
        #[cfg(feature = "dev-bridge")]
        "dev_frame_cycle" => {
            use std::sync::atomic::Ordering;
            ctx.frame_hidden.force.store(true, Ordering::SeqCst);
            let t = std::time::Instant::now();
            while !ctx.frame_hidden.closed.load(Ordering::SeqCst)
                && t.elapsed() < std::time::Duration::from_secs(5)
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            ctx.frame_hidden.force.store(false, Ordering::SeqCst);
            ctx.frame_hidden.closed.store(false, Ordering::SeqCst);
            let ok = ctx.frame_session.rearm();
            Ok(json!({"rearmed": ok, "generation": ctx.frame_session.current().1}))
        }
        // Dev bridge builds only: the zero-spend stub model's turns for the next runs.
        #[cfg(feature = "dev-bridge")]
        "dev_stub_turns" => {
            crate::agent::dev_stub_turns(a.get("turns").cloned().unwrap_or(Value::Null))?;
            Ok(json!({}))
        }
        // Dev bridge builds only: a batch without the sheet (the e2e), through the launcher.
        #[cfg(feature = "dev-bridge")]
        "dev_batch_start" => {
            let cuts: Vec<String> = a
                .get("cuts")
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            crate::batch::dev_start(
                core,
                project_of(req)?,
                &cuts,
                s(a, "pass")?,
                &batch_options(a),
            )
        }
        // Dev bridge builds only: an approval as an agent's run would raise it (R-PROTO-6
        // / R-UI-SEC-2 fixtures: effects the server computed, prose the agent wrote).
        #[cfg(feature = "dev-bridge")]
        "dev_approval_add" => {
            let h = core
                .agents
                .holder()
                .ok_or_else(|| AppError::new("Unsupported", "no agent server in this app"))?;
            let identity = s(a, "identity")?;
            let asked = h.approval_add(
                identity,
                a.get("run").and_then(Value::as_str).unwrap_or(identity),
                s(a, "tool")?,
                a.get("effects").cloned().unwrap_or(Value::Null),
                a.get("prose").and_then(Value::as_str).map(str::to_owned),
            );
            Ok(match asked {
                gmclip_mcp::holder::Asked::New(id) => json!({"queued": true, "approval": id}),
                gmclip_mcp::holder::Asked::Again(id) => {
                    json!({"queued": true, "again": true, "approval": id})
                }
                gmclip_mcp::holder::Asked::Paused => json!({"queued": false}),
            })
        }
        // Dev bridge builds only: one MCP tool call as an outside agent's session would make
        // it (decision 0050 e2e: an agent's app commands, its refused export and its asks).
        #[cfg(feature = "dev-bridge")]
        "dev_agent_tool" => {
            let h = core
                .agents
                .holder()
                .ok_or_else(|| AppError::new("Unsupported", "no agent server in this app"))?;
            let session = gmclip_mcp::wire::session_label(s(a, "session")?)
                .ok_or_else(|| AppError::invalid("a session label"))?
                .to_owned();
            let conn = gmclip_mcp::holder::Conn {
                id: h.next_conn_id(),
                pid: None,
                exe: None,
                client_exe: None,
                identity: std::sync::Mutex::new(Some(format!("local:session-{session}"))),
                launched: None,
                granted: None,
                session: Some(session),
                touched: std::sync::Mutex::new(Default::default()),
                logged: std::sync::Mutex::new(true),
            };
            let r = gmclip_mcp::tools::call(
                &h,
                &conn,
                s(a, "tool")?,
                a.get("args").cloned().unwrap_or_else(|| json!({})),
            );
            Ok(match r {
                Ok(v) => json!({"ok": v}),
                Err(e) => json!({"error": e}),
            })
        }
        // Dev bridge builds only: a registry command as an agent connection would send it
        // (decision 0011 e2e: topic and fluff proposals arrive the way a pass makes them).
        #[cfg(feature = "dev-bridge")]
        "dev_agent_call" => {
            let p = core.project(project_of(req)?)?;
            let as_of = p.handle.snapshot().doc.rev();
            let ok = p
                .handle
                .call(gmclip_core::CallRequest {
                    command: s(a, "command")?.to_owned(),
                    args: a.get("args").cloned().unwrap_or(Value::Null),
                    base: Some(gmclip_proto::Base::AsOf { as_of }),
                    request_id: None,
                    caller: gmclip_core::Caller::agent("local:e2e", "e2e"),
                })
                .map_err(AppError::from)?;
            Ok(ok.output)
        }
        // Dev bridge builds only: R-EVT-1's UI side (A2.15/U.18) — `n` agent edits from
        // another thread (as `gmclip-app perf` floods the actor), so the e2e can measure
        // the page's input latency and the bus coalescing while they land. Returns at
        // once; `{"status": true}` says how far it got.
        #[cfg(feature = "dev-bridge")]
        "dev_agent_burst" => crate::perf::dev_burst(
            core,
            project_of(req)?,
            a.get("status").and_then(Value::as_bool).unwrap_or(false),
            a.get("cut").and_then(Value::as_str).unwrap_or_default(),
            a.get("part").and_then(Value::as_str).unwrap_or_default(),
            a.get("n")
                .and_then(Value::as_u64)
                .unwrap_or(1000)
                .min(10_000) as usize,
        ),
        // Dev bridge builds only: the project's store behaves as on a full disk (`on`) or
        // gets its space back (R-ERR-2 e2e: "Not saved: disk full", retried, never lost).
        #[cfg(feature = "dev-bridge")]
        "dev_disk_full" => {
            let p = core.project(project_of(req)?)?;
            p.handle
                .simulate_disk_full(a.get("on").and_then(Value::as_bool).unwrap_or(true))
                .map_err(|e| AppError::internal(e.to_string()))?;
            Ok(json!({}))
        }
        // Dev bridge builds only: exports read this free space instead of the volume's
        // (R-ERR-2 e2e: a new export below 2 GB / twice its estimate is refused with the
        // numbers); `bytes: null` puts the real statfs back.
        #[cfg(feature = "dev-bridge")]
        "dev_free_space" => {
            crate::jobs::fake_free_space(a.get("bytes").and_then(Value::as_u64));
            Ok(json!({}))
        }
        // Dev bridge builds only: serving and a link without the sheets (WebKit e2e).
        #[cfg(feature = "dev-bridge")]
        "dev_review_link" => {
            crate::reviewlink::dev_link_as(core, a.get("role").and_then(Value::as_str))
        }
        // Dev bridge builds only: a review note as a link writes it (R-UI-SEC-2 fixtures);
        // `comment: true` writes a comment instead; `export` is the export's id on the link
        // (the board's `thread_key`).
        #[cfg(feature = "dev-bridge")]
        "dev_review_add" if a.get("comment").and_then(Value::as_bool) == Some(true) => {
            crate::reviewlink::add_comment_as(
                core,
                "e2e",
                s(a, "cut")?,
                s(a, "text")?,
                a.get("export").and_then(Value::as_str),
            )
        }
        #[cfg(feature = "dev-bridge")]
        "dev_review_add" => crate::reviewlink::add_review_as(
            core,
            "e2e",
            s(a, "cut")?,
            s(a, "text")?,
            a.get("verdict").and_then(Value::as_str).unwrap_or("change"),
            a.get("export").and_then(Value::as_str),
        ),
        // The UI's first render (U.18 cold start).
        "app_ready" => {
            if ctx.perf_cold_start {
                let ms = crate::START
                    .get()
                    .map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0);
                println!("GMCLIP_PERF cold_start_ms={ms:.0}");
                use std::io::Write;
                let _ = std::io::stdout().flush();
                std::process::exit(0);
            }
            // R-PROF-3: style-file changes since the last accept, batched on one sheet
            let core = ctx.core.clone();
            let _ = std::thread::Builder::new()
                .name("gmclip-style-sheet".into())
                .spawn(move || {
                    let _ = crate::owner::style_changes_sheet(&core, false);
                });
            // the agent clients are found once (a few seconds: each is probed) and kept:
            // found now, Settings and Agents open with them (Settings said "Loading…")
            let core = ctx.core.clone();
            let _ = std::thread::Builder::new()
                .name("gmclip-agent-detect".into())
                .spawn(move || {
                    let _ = crate::agent::detected(&core, false);
                });
            Ok(json!({}))
        }
        // Re-armed after a long hide (decision 0010): a new token and generation.
        "app_frame_socket" => {
            let (token, generation) = ctx.frame_session.current();
            Ok(json!({"port": ctx.frame_port, "token": token, "generation": generation}))
        }
        // Only this webview's own subscriptions.
        "app_unsubscribe" => {
            let id = a.get("sub").and_then(Value::as_u64).unwrap_or(0);
            let mine = ctx.webview_subs.lock().is_ok_and(|mut g| {
                let n = g.len();
                g.retain(|s| *s != id);
                g.len() != n
            });
            Ok(json!({"removed": mine && core.bus.unsubscribe(id)}))
        }
        "project_list_open" => Ok(Value::Array(
            core.projects()
                .iter()
                .map(|p| core.project_json(p))
                .collect(),
        )),
        "project_new" => core.create_project(s(a, "name")?),
        "project_open_home" => {
            let v = core.open_home(&PathBuf::from(s(a, "path")?))?;
            if let Some(id) = v["id"].as_str() {
                core.analysis.ensure(core, id);
                core.proxies.ensure(core, id);
                // cut-outs without accurate tracks build in the background (R.01)
                crate::roto::mark(core, id);
            }
            // exports a takeover from `gmclip serve` stopped restart from frame 0 (§3.9)
            let stopped: Vec<(String, String)> = core
                .taken
                .lock()
                .map(|mut g| std::mem::take(&mut *g))
                .unwrap_or_default();
            for (pid, cut) in stopped {
                let _ = ctx.jobs.start_export(core, &pid, &cut);
            }
            Ok(v)
        }
        "project_close" => {
            ctx.viewer.send(ViewerMsg::Unload);
            core.close_project(project_of(req)?)?;
            Ok(json!({}))
        }
        "cut_import" => {
            let path = PathBuf::from(s(a, "path")?);
            core.import(project_of(req)?, &path)
        }
        "cut_view" => core.cut_view(project_of(req)?, s(a, "cut")?),
        "cut_checks" => crate::checks::checks(core, project_of(req)?, s(a, "cut")?),
        // A2.17: a check's one-click fix (worked out again from the cut, never the page)
        "check_fix" => crate::checks::fix(
            core,
            project_of(req)?,
            s(a, "cut")?,
            a.get("n")
                .and_then(Value::as_u64)
                .ok_or_else(|| AppError::invalid("n"))? as usize,
            s(a, "rule")?,
        ),
        // History (U.14 / A2.13): the versions, a cut restored to one, a named snapshot.
        "history_list" => crate::history::list(
            core,
            project_of(req)?,
            a.get("cut").and_then(Value::as_str),
            &core.agents.launched_clients(),
        ),
        "history_count" => crate::history::count(core, project_of(req)?),
        "history_diff" => crate::history::diff(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "snapshot")?,
            a.get("against").and_then(Value::as_str),
            &core.agents.launched_clients(),
        ),
        "history_restore" => crate::history::restore(
            core,
            project_of(req)?,
            a.get("cut").and_then(Value::as_str),
            s(a, "snapshot")?,
        ),
        "snapshot_named" => {
            let name = s(a, "name")?.trim();
            if name.is_empty() || name.chars().count() > 80 {
                return Err(AppError::invalid("a name, at most 80 characters"));
            }
            core.call(
                project_of(req)?,
                "restore_point_create",
                json!({"name": name}),
            )
            .map(|v| v["output"].clone())
        }
        "cut_trim" => core.trim_by(
            project_of(req)?,
            s(a, "cut")?,
            s(a, "part")?,
            s(a, "edge")?,
            n(a, "delta")? as i64,
        ),
        "cut_split" => core.split_at_frame(project_of(req)?, s(a, "cut")?, n(a, "frame")? as u64),
        "cut_range_delete" => core.delete_range(
            project_of(req)?,
            s(a, "cut")?,
            n(a, "from")?.max(0.0) as u64,
            n(a, "to")?.max(0.0) as u64,
        ),
        "layer_place_px" => core.place_px_group(
            project_of(req)?,
            s(a, "cut")?,
            s(a, "part")?,
            s(a, "layer")?,
            n(a, "x")?,
            n(a, "y")?,
            n(a, "w")?,
            a.get("rev").and_then(Value::as_u64),
            &crate::framing::group_arg(a),
        ),
        "layer_crop_px" => core.crop_px_group(
            project_of(req)?,
            s(a, "cut")?,
            s(a, "part")?,
            s(a, "layer")?,
            [n(a, "x")?, n(a, "y")?, n(a, "w")?, n(a, "h")?],
            a.get("rev").and_then(Value::as_u64),
            &crate::framing::group_arg(a),
        ),
        "viewer_load" => {
            let (p, c) = (project_of(req)?, s(a, "cut")?);
            core.project(p)?;
            // `source: true` shows the cut's 16:9 framing (the crop editor), keeping
            // the playhead; `keep` alone keeps it too.
            let source = a.get("source").and_then(Value::as_bool).unwrap_or(false);
            // the crop editor on one part: that part's own source picture, whole, or the
            // picture of its `layer` (a recording's camera file beside its screen)
            let framing = if let (true, Some(part)) =
                (source, a.get("part").and_then(Value::as_str))
            {
                Some(match a.get("layer").and_then(Value::as_str) {
                    Some(l) => format!("source:{part}:{l}"),
                    None => format!("source:{part}"),
                })
            } else if source {
                Some(
                    crate::framing::source_view_framing(core, p, c).ok_or_else(|| {
                        AppError::new("Unsupported", "this cut has no 16:9 framing to crop from")
                    })?,
                )
            } else {
                None
            };
            ctx.viewer.send(ViewerMsg::Load {
                project: p.into(),
                cut: c.into(),
                framing,
                keep: a.get("keep").and_then(Value::as_bool).unwrap_or(false),
            });
            Ok(json!({}))
        }
        "viewer_unload" => {
            ctx.viewer.send(ViewerMsg::Unload);
            Ok(json!({}))
        }
        "viewer_play_range" => {
            let f = |k: &str| n(a, k).map(|x| x.max(0.0) as u64);
            ctx.viewer.send(ViewerMsg::PlayRange {
                from: f("from_sample")?,
                to: f("to_sample")?,
                looping: a.get("looping").and_then(Value::as_bool).unwrap_or(false),
            });
            Ok(json!({}))
        }
        "viewer_play_src" => {
            ctx.viewer.send(ViewerMsg::PlaySrc {
                from_ms: n(a, "from_ms")? as i64,
                to_ms: n(a, "to_ms")? as i64,
            });
            Ok(json!({}))
        }
        "viewer_seek" => {
            let f = n(a, "frame")?.max(0.0) as u64;
            // a scrub step (H.05) shows the scrub proxy until the scrub rests
            ctx.viewer
                .send(if a.get("scrub").and_then(Value::as_bool) == Some(true) {
                    ViewerMsg::ScrubTo(f)
                } else {
                    ViewerMsg::Seek(f)
                });
            Ok(json!({}))
        }
        "proxy_status" => Ok(core
            .proxies
            .status(a.get("project").and_then(Value::as_str))),
        "viewer_play" => {
            ctx.viewer.send(ViewerMsg::Play);
            Ok(json!({}))
        }
        "viewer_pause" => {
            ctx.viewer.send(ViewerMsg::Pause);
            Ok(json!({}))
        }
        "viewer_toggle" => {
            ctx.viewer.send(ViewerMsg::Toggle);
            Ok(json!({}))
        }
        "viewer_step" => {
            ctx.viewer
                .send(ViewerMsg::Step(n(a, "n")?.clamp(-10_000.0, 10_000.0) as i64));
            Ok(json!({}))
        }
        "viewer_full" => {
            ctx.viewer.send(ViewerMsg::Full(
                a.get("on").and_then(Value::as_bool).unwrap_or(false),
            ));
            Ok(json!({}))
        }
        "viewer_scrub" => {
            ctx.viewer.send(ViewerMsg::Scrub(
                a.get("on").and_then(Value::as_bool).unwrap_or(true),
            ));
            Ok(json!({}))
        }
        "viewer_rate" => {
            ctx.viewer.send(ViewerMsg::Rate(n(a, "rate")?));
            Ok(json!({}))
        }
        "viewer_stress" => {
            ctx.viewer.send(ViewerMsg::Stress(
                a.get("on").and_then(Value::as_bool).unwrap_or(false),
            ));
            Ok(json!({}))
        }
        // the layers grouped on the timeline: a drag of one on the preview places them all
        "viewer_group" => {
            ctx.viewer
                .send(ViewerMsg::Group(crate::framing::group_arg(a)));
            Ok(json!({}))
        }
        "viewer_select" => {
            let sel = match (
                a.get("part").and_then(Value::as_str),
                a.get("layer").and_then(Value::as_str),
            ) {
                (Some(p), Some(l)) => Some((p.to_owned(), l.to_owned())),
                _ => None,
            };
            ctx.viewer.send(ViewerMsg::Select(sel));
            Ok(json!({}))
        }
        "viewer_state" => Ok(serde_json::to_value(ctx.viewer.state()).unwrap_or(Value::Null)),
        // U.12: a text overlay (`role` + its anchor `at`, canvas px) or the caption seat
        // (`role: caption`, no `at`) dragged `(dx, dy)` canvas px: the frame previews it
        // through the shared compositor. Never logged; the release commits one command.
        "viewer_text_drag" => {
            let role: gmclip_plan::types::TextRole = serde_json::from_value(
                a.get("role").cloned().unwrap_or(Value::Null),
            )
            .map_err(|_| {
                AppError::invalid("role: caption, text_hook, section_pill, run_clock or note")
            })?;
            let at = match a.get("at") {
                None | Some(Value::Null) => None,
                Some(v) => Some(
                    serde_json::from_value::<(f64, f64)>(v.clone())
                        .ok()
                        .filter(|(x, y)| x.is_finite() && y.is_finite())
                        .ok_or_else(|| AppError::invalid("at: [x, y] canvas px"))?,
                ),
            };
            let (dx, dy) = (n(a, "dx")?, n(a, "dy")?);
            if !(dx.abs() <= 20_000.0 && dy.abs() <= 20_000.0) {
                return Err(AppError::invalid("dx, dy: canvas px"));
            }
            ctx.viewer.send(ViewerMsg::TextDrag(
                gmclip_render::export::preview::TextShift { role, at, dx, dy },
            ));
            Ok(json!({}))
        }
        // decision 0053: a part edge dragged on the timeline shows, paused, the frame on the
        // edge as the trim would leave it (planned on a copy, never committed or logged);
        // the end goes back to the cut and the frame shown before
        "viewer_trim_peek" => {
            let (plan, frame) = core.trim_peek(
                project_of(req)?,
                s(a, "cut")?,
                s(a, "part")?,
                s(a, "edge")?,
                n(a, "delta")? as i64,
            )?;
            ctx.viewer.send(ViewerMsg::TrimPeek { plan, frame });
            Ok(json!({ "frame": frame }))
        }
        "viewer_trim_peek_end" => {
            ctx.viewer.send(ViewerMsg::TrimPeekEnd);
            Ok(json!({}))
        }
        "viewer_text_drag_end" => {
            ctx.viewer.send(ViewerMsg::TextDragEnd {
                committed: a.get("committed").and_then(Value::as_bool).unwrap_or(false),
            });
            Ok(json!({}))
        }
        "openings_view" => crate::openings::view(core, project_of(req)?, s(a, "cut")?),
        // a version in the viewer: the cut with that opening (none: the cut itself)
        "opening_preview" => {
            let pid = project_of(req)?.to_owned();
            let cut = s(a, "cut")?.to_owned();
            let opening = a.get("opening").and_then(Value::as_str);
            let pl = match opening {
                Some(o) => crate::openings::planned_version(core, &pid, &cut, o)?,
                None => core.planned(&pid, &cut)?,
            };
            let (frames, fps) = (pl.plan.output.frames, pl.plan.output.fps.as_f64());
            ctx.viewer.send(ViewerMsg::LoadPlan {
                key: (pid, format!("opening:{cut}:{}", opening.unwrap_or("main"))),
                plan: pl.plan.clone(),
                looping: false,
                at: Some(0),
            });
            Ok(json!({"frames": frames, "fps": fps}))
        }
        "opening_add_words" => crate::openings::add_words(
            core,
            project_of(req)?,
            s(a, "cut")?,
            a.get("from")
                .and_then(Value::as_u64)
                .ok_or_else(|| AppError::invalid("from: a word"))?,
            a.get("to")
                .and_then(Value::as_u64)
                .ok_or_else(|| AppError::invalid("to: a word"))?,
            a.get("label").and_then(Value::as_str),
            a.get("hook_text").and_then(Value::as_str),
        ),
        "opening_export" => {
            ctx.jobs
                .start_export_version(core, project_of(req)?, s(a, "cut")?, s(a, "opening")?)
        }
        "end_card_longform_view" => crate::endcard::view(core, project_of(req)?, s(a, "cut")?),
        "end_card_longform_add_app" => crate::endcard::add(core, project_of(req)?, s(a, "cut")?),
        "end_card_longform_remove" => crate::endcard::remove(core, project_of(req)?, s(a, "cut")?),
        "export_framings" => crate::bothexport::view(core, project_of(req)?, s(a, "cut")?),
        "framing_make" => {
            let p = project_of(req)?;
            let r = crate::bothexport::make(core, p, s(a, "cut")?, s(a, "aspect")?)?;
            ctx.viewer.send(ViewerMsg::Replan { project: p.into() });
            Ok(r)
        }
        "export_both" => crate::bothexport::start(
            core,
            &ctx.jobs,
            project_of(req)?,
            s(a, "cut")?,
            a.get("make_missing")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
        "export_start" => ctx.jobs.start_export_preset(
            core,
            project_of(req)?,
            s(a, "cut")?,
            a.get("draft_mattes")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            a.get("preset").and_then(Value::as_str),
        ),
        #[cfg(any(target_os = "macos", windows))]
        "matte_status" => crate::roto::status(core, project_of(req)?, s(a, "cut")?),
        #[cfg(any(target_os = "macos", windows))]
        "matte_build" => crate::roto::build(core, project_of(req)?, s(a, "cut")?),
        #[cfg(any(target_os = "macos", windows))]
        "matte_progress" => crate::roto::progress(core, project_of(req)?, s(a, "cut")?),
        "follow_action" => crate::roto::follow_action(
            core,
            project_of(req)?,
            s(a, "cut")?,
            a.get("part").and_then(Value::as_str),
            a.get("dry_run").and_then(Value::as_bool).unwrap_or(false),
        ),
        "follow_cursor" => crate::cursorfollow::follow_cursor(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "part")?,
            a.get("on").and_then(Value::as_bool).unwrap_or(false),
            a.get("max_zoom").and_then(Value::as_f64),
        ),
        "viewer_matte_view" => {
            ctx.viewer.send(ViewerMsg::MatteView(
                a.get("on").and_then(Value::as_bool).unwrap_or(false),
            ));
            Ok(json!({}))
        }
        "export_cancel" => Ok(json!({"cancelled": ctx.jobs.cancel(s(a, "job")?)})),
        // H.06 (R-EXP-6): re-verify an export file and record the result on its entry
        "export_verify" => crate::exportverify::reverify(core, project_of(req)?, s(a, "export")?),
        // H.09 (R-PLAT-1): the startup capability probe's lines for this Mac
        "caps_startup" => Ok(crate::caps::startup()),
        "jobs_list" => Ok(serde_json::to_value(ctx.jobs.list()).unwrap_or(Value::Null)),
        "exports_list" => {
            Ok(serde_json::to_value(core.exports(project_of(req)?)?).unwrap_or(Value::Null))
        }
        // Share… and drag-out (SPEC §5.14): a file the export list names, never a path.
        "export_share" => crate::share::share(
            core,
            project_of(req)?,
            s(a, "export")?,
            a.get("x").and_then(Value::as_f64).unwrap_or(0.0),
            a.get("y").and_then(Value::as_f64).unwrap_or(0.0),
        ),
        "export_drag" => crate::share::drag(core, project_of(req)?, s(a, "export")?),
        // Retention on the timeline: a picked file's text (never a path), or Clippy's own
        // measurement read only; stored on the export, drawn under the timeline.
        "retention_import" => crate::retention::import(
            core,
            project_of(req)?,
            s(a, "export")?,
            a.get("name").and_then(Value::as_str).unwrap_or("retention"),
            s(a, "text")?,
        ),
        "retention_clippy" => {
            crate::retention::from_clippy(core, project_of(req)?, s(a, "export")?)
        }
        "retention_view" => crate::retention::view(core, project_of(req)?, s(a, "cut")?),
        "export_reveal" => {
            // Only a file the export list names; never a path from the caller.
            let id = s(a, "export")?;
            let e = core
                .exports(project_of(req)?)?
                .into_iter()
                .find(|e| e.id == id)
                .ok_or_else(|| AppError::not_found("no such export"))?;
            if !e.file.exists() {
                return Err(AppError::not_found(
                    "the exported file has moved or been deleted",
                ));
            }
            reveal(&e.file)?;
            Ok(json!({}))
        }
        // S2a's measurements, written to `<app data>/diagnostics/` (shape only: numbers).
        "app_s2a_report" => {
            let report = a.get("report").cloned().unwrap_or(Value::Null);
            let bytes =
                serde_json::to_vec_pretty(&report).map_err(|_| AppError::invalid("report"))?;
            if bytes.len() > 256 * 1024 {
                return Err(AppError::invalid("report too large"));
            }
            let dir = core.dirs.app_data.join("diagnostics");
            std::fs::create_dir_all(&dir)
                .map_err(|e| AppError::internal(format!("diagnostics: {}", e.kind())))?;
            let path = dir.join(format!("s2a-{}.json", now_ms()));
            std::fs::write(&path, &bytes)
                .map_err(|e| AppError::internal(format!("diagnostics: {}", e.kind())))?;
            eprintln!(
                "GMCLIP_S2A {}",
                serde_json::to_string(&report).unwrap_or_default()
            );
            if ctx.autorun.as_deref() == Some("s2a") {
                let core = core.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(300));
                    core.shutdown();
                    std::process::exit(0);
                });
            }
            Ok(json!({"file": path}))
        }
        "cut_words" => crate::words::cut_words(core, project_of(req)?, s(a, "cut")?),
        "words_strike_ord" | "words_restore_ord" => crate::words::strike_of(
            core,
            project_of(req)?,
            s(a, "cut")?,
            n(a, "from")?.max(0.0) as u64,
            n(a, "to")?.max(0.0) as u64,
            req.command == "words_restore_ord",
            a.get("owner").and_then(Value::as_str),
        ),
        "word_nudge_ord" => crate::words::nudge(
            core,
            project_of(req)?,
            s(a, "cut")?,
            n(a, "word")?.max(0.0) as u64,
            s(a, "edge")?,
            n(a, "delta_ms")? as i64,
            a.get("owner").and_then(Value::as_str),
        ),
        "word_respell" => crate::words::respell(
            core,
            project_of(req)?,
            s(a, "cut")?,
            n(a, "word")?.max(0.0) as u64,
            a.get("text").and_then(Value::as_str),
            a.get("owner").and_then(Value::as_str),
        ),
        "word_edge_reset" => crate::words::edge_reset(
            core,
            project_of(req)?,
            s(a, "cut")?,
            n(a, "word")?.max(0.0) as u64,
            a.get("owner").and_then(Value::as_str),
        ),
        "words_add_part" => crate::words::add_as_part_of(
            core,
            project_of(req)?,
            s(a, "cut")?,
            n(a, "from")?.max(0.0) as u64,
            n(a, "to")?.max(0.0) as u64,
            a.get("after").and_then(Value::as_str),
            a.get("owner").and_then(Value::as_str),
        ),
        "words_stretch_last" => crate::words::stretch_last(
            core,
            project_of(req)?,
            s(a, "cut")?,
            n(a, "to")?.max(0.0) as u64,
            a.get("owner").and_then(Value::as_str),
        ),
        "story_view" => crate::story::story_view(core, project_of(req)?, s(a, "cut")?),
        "part_time_set" => crate::story::set_time(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "part")?,
            s(a, "edge")?,
            s(a, "text")?,
        ),
        "part_nudge_ms" => crate::story::nudge_ms(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "part")?,
            s(a, "edge")?,
            n(a, "delta_ms")? as i64,
        ),
        "cut_chronological" => crate::story::chronological(core, project_of(req)?, s(a, "cut")?),
        "part_move" => crate::story::move_part(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "part")?,
            s(a, "dir")?,
        ),
        // The stream around a time, in the viewer, paused there: browse it and mark a run
        // the cut does not show (`viewer_load` goes back to the cut).
        // The pair dialog's measured offset (sound, then pictures), with a confidence.
        "pair_propose" => crate::autosync::propose_json(
            s(a, "voice")?,
            s(a, "hires")?,
            a.get("voice_track")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok()),
        ),
        "term_gate" => crate::short::term_gate(core, project_of(req)?, s(a, "cut")?),
        // "exe.dev" heard as "XD.dev, x e dev" (comma list)
        "term_add" => core.call(
            project_of(req)?,
            "term_set",
            json!({"written": s(a, "written")?,
                   "heard_as": a.get("heard_as").and_then(Value::as_str).unwrap_or("")
                       .split(',').map(str::trim).filter(|x| !x.is_empty()).collect::<Vec<_>>(),
                   "remove": a.get("remove").and_then(Value::as_bool).unwrap_or(false)}),
        ),
        "caption_y" => {
            crate::short::caption_y_set(core, project_of(req)?, s(a, "cut")?, n(a, "y")?)
        }
        "hold_at" => crate::tighten::hold_part_at(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "part")?,
            n(a, "at_s")?,
        ),
        // "web app, link your account": each comma group kept together in caption cues
        "caption_keep" => {
            let p = project_of(req)?;
            let on = a.get("keep").and_then(Value::as_bool).unwrap_or(true);
            let mut n_set = 0;
            for g in s(a, "words")?.split(',') {
                let words: Vec<&str> = g.split_whitespace().collect();
                if words.is_empty() {
                    continue;
                }
                core.call(p, "caption_keep_set", json!({"words": words, "keep": on}))?;
                n_set += 1;
            }
            Ok(json!({"groups": n_set}))
        }
        "stream_view" => {
            let pid = project_of(req)?.to_owned();
            let cut = s(a, "cut")?.to_owned();
            let (plan, info) = crate::short::source_view(core, &pid, &cut, n(a, "at_s")?)?;
            ctx.viewer.send(ViewerMsg::LoadPlan {
                key: (pid, format!("stream:{cut}")),
                plan: std::sync::Arc::new(plan),
                looping: false,
                at: info["frame"].as_u64(),
            });
            Ok(info)
        }
        "moments_list" => {
            let p = core.project(project_of(req)?)?;
            Ok(crate::review::moments_list(&p, &core.transcripts))
        }
        // Review mode: the excerpt's first word → last word + 1 s, looping, video first.
        "moment_review" => {
            let pid = project_of(req)?.to_owned();
            let p = core.project(&pid)?;
            let moment = s(a, "moment")?.to_owned();
            let (plan, (a0, a1)) = crate::review::review_for(&p, &core.transcripts, &moment)?;
            ctx.viewer.send(ViewerMsg::LoadPlan {
                key: (pid.clone(), format!("moment:{moment}")),
                plan: std::sync::Arc::new(plan),
                looping: true,
                at: None,
            });
            // H.10: this moment's clip and the next ones' render in the background (the
            // review link serves them; the viewer here plays the master, exactly)
            let mut want = vec![moment.clone()];
            if let Some(next) = a.get("next").and_then(Value::as_array) {
                want.extend(
                    next.iter()
                        .filter_map(Value::as_str)
                        .take(3)
                        .map(str::to_owned),
                );
            }
            core.clips.queue(core, &pid, &want);
            let clip = crate::clips::cached(core, &pid, &moment).is_some();
            Ok(json!({"start_s": a0, "end_s": a1, "clip": clip}))
        }
        // H.10: a moment's clip (≤ 720p, cached by moment, transcript revision and span),
        // rendered now when there is no current one.
        "moment_clip" => {
            let pid = project_of(req)?.to_owned();
            let t = std::time::Instant::now();
            let (rec, path, rendered) = crate::clips::ensure(core, &pid, s(a, "moment")?)?;
            Ok(json!({"clip": rec, "path": path, "rendered": rendered,
                      "ms": t.elapsed().as_millis() as u64}))
        }
        "video_open" => crate::video::open_stream(
            core,
            &PathBuf::from(s(a, "path")?),
            a.get("voice")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok()),
            a.get("hires").and_then(|h| {
                Some(crate::video::HiRes {
                    path: PathBuf::from(h.get("path")?.as_str()?),
                    offset_s: h.get("offset_s").and_then(Value::as_f64).unwrap_or(0.0),
                    segments: h
                        .get("segments")
                        .and_then(Value::as_array)
                        .map(|v| {
                            v.iter()
                                .filter_map(|x| {
                                    Some((x.get("from_s")?.as_f64()?, x.get("offset_s")?.as_f64()?))
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
            }),
        ),
        "cut_from_moment" => crate::cutnew::from_moment(core, project_of(req)?, s(a, "moment")?),
        "loudness_set" => crate::framing::set_loudness(
            core,
            project_of(req)?,
            s(a, "cut")?,
            a.get("lufs").and_then(Value::as_f64),
        ),
        "sound_view" => crate::sound::view(core, project_of(req)?, s(a, "cut")?),
        "gain_span_add" => crate::sound::gain_span(
            core,
            project_of(req)?,
            s(a, "cut")?,
            n(a, "from")? as i64,
            n(a, "to")? as i64,
            n(a, "gain_db")?,
        ),
        "gain_span_delete" => crate::sound::gain_delete(core, project_of(req)?, s(a, "segment")?),
        "music_add" => {
            crate::sound::music_add(core, project_of(req)?, s(a, "cut")?, s(a, "path")?)
        }
        "part_gain_set" => {
            crate::sound::part_gain(core, project_of(req)?, s(a, "part")?, n(a, "gain_db")?)
        }
        "cam_hide_add" => crate::sound::hide_cam(
            core,
            project_of(req)?,
            s(a, "cut")?,
            n(a, "from")? as i64,
            n(a, "to")? as i64,
        ),
        "no_captions_add" => crate::sound::no_captions(
            core,
            project_of(req)?,
            s(a, "cut")?,
            n(a, "from")? as i64,
            n(a, "to")? as i64,
        ),
        "no_captions_delete" => {
            crate::sound::no_captions_delete(core, project_of(req)?, s(a, "cut")?, s(a, "id")?)
        }
        "cam_hide_delete" => {
            crate::sound::hide_delete(core, project_of(req)?, s(a, "cut")?, s(a, "hide")?)
        }
        "picture_offset_set" => crate::video::set_picture_offset(
            core,
            project_of(req)?,
            s(a, "cut")?,
            n(a, "offset_s")?,
            a.get("frame").and_then(Value::as_i64),
            a.get("split").and_then(Value::as_bool).unwrap_or(false),
        ),
        "agent_client_allow" => crate::clientcheck::allow(core, s(a, "sha256")?),
        "video_inspect" => crate::importsheet::inspect(core, s(a, "path")?),
        "recording_status" => crate::recording::status(core),
        "recording_reveal" => {
            let dir = crate::recording::reveal_folder(core)?;
            reveal(&dir)?;
            Ok(json!({"folder": dir}))
        }
        "recording_enable" => {
            crate::recording::enable(core, a.get("on").and_then(Value::as_bool).unwrap_or(true))
        }
        "recording_ask" => crate::recording::ask(
            core,
            s(a, "kind")?,
            a.get("settings").and_then(Value::as_bool).unwrap_or(false),
        ),
        "recording_preview" => crate::recording::preview(
            a.get("camera")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty()),
            a.get("on").and_then(Value::as_bool).unwrap_or(false),
        ),
        "recording_sources" => Ok(crate::recording::sources()),
        "recording_prepare" => crate::recording::prepare(core, a),
        "recording_go" => crate::recording::go(),
        "recording_marker" => crate::recording::marker(),
        "recording_stop" => crate::recording::stop(core),
        "recording_cancel" => crate::recording::cancel(),
        "recording_open" => crate::recording::open(core, &PathBuf::from(s(a, "dir")?)),
        "screen_studio_apply" => crate::screenstudio::apply(
            core,
            project_of(req)?,
            s(a, "cut")?,
            a.get("plate").and_then(Value::as_str),
        ),
        "zoom_auto_app" => crate::screenstudio::zoom_auto(
            core,
            project_of(req)?,
            s(a, "cut")?,
            a.get("max_zoom").and_then(Value::as_f64),
        ),
        "videos_inspect" => crate::video::inspect_many(core, &strings(a, "paths")?),
        "videos_open" => {
            let (files, same) = add_files(a)?;
            let one = a
                .get("one_project")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let name = a.get("name").and_then(Value::as_str);
            crate::video::open_many(core, &files, &same, one, name)
        }
        "videos_add" => {
            let (files, same) = add_files(a)?;
            crate::video::add_many(core, project_of(req)?, &files, &same)
        }
        "videos_pick" => {
            let pick = ctx.picker_many.get().ok_or_else(no_panel)?;
            let picked: Vec<String> = pick()
                .into_iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            Ok(json!({"picked": picked}))
        }
        "video_remove" => crate::video::remove_source(core, project_of(req)?, s(a, "source")?),
        "video_relink" => {
            let path = match a.get("path").and_then(Value::as_str) {
                Some(p) => PathBuf::from(p),
                None => {
                    let pick = ctx.picker.get().ok_or_else(no_panel)?;
                    match pick() {
                        Some(p) => p,
                        None => return Ok(json!({"cancelled": true})),
                    }
                }
            };
            crate::video::relink_source(core, project_of(req)?, s(a, "source")?, &path)
        }
        // what a native drop sends (the dev bridge replays it: a page under Playwright
        // has no native drop)
        "files_dropped" => {
            let paths: Vec<String> = strings(a, "paths")?.into_iter().take(16).collect();
            // `at`: where the drop landed, CSS px of the window (a drop on the timeline)
            let at = a
                .get("at")
                .and_then(Value::as_array)
                .and_then(|v| Some([v.first()?.as_f64()?, v.get(1)?.as_f64()?]));
            core.bus
                .home(crate::events::HomeEvent::Dropped { paths, at });
            Ok(json!({}))
        }
        "video_pick" => {
            let pick = ctx.picker.get().ok_or_else(no_panel)?;
            match pick() {
                // the UI inspects it (the import sheet) and then opens it
                Some(path) => Ok(json!({"picked": path})),
                None => Ok(json!({"cancelled": true})),
            }
        }
        "videos_recent" => Ok(crate::video::recent_json(core)),
        "video_thumb" => crate::recentthumbs::video_thumb(core, s(a, "path")?),
        "cut_output_set" => {
            let (p, c) = (project_of(req)?, s(a, "cut")?);
            let r = crate::framing::set_output(core, p, c, s(a, "aspect")?)?;
            ctx.viewer.send(ViewerMsg::Replan { project: p.into() });
            Ok(r)
        }
        "layout_preset" => {
            let parts: Vec<String> = a
                .get("parts")
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .take(10_000)
                        .collect()
                })
                .unwrap_or_default();
            crate::framing::apply_preset(
                core,
                project_of(req)?,
                s(a, "cut")?,
                &parts,
                s(a, "preset")?,
            )
        }
        "layout_apply_all" => {
            crate::framing::apply_to_all(core, project_of(req)?, s(a, "cut")?, s(a, "part")?)
        }
        "hook_layout_keep" => {
            crate::takelayout::keep_opening_until_hook(core, project_of(req)?, s(a, "cut")?)
        }
        "inset_add" => {
            let nums = |k: &str| -> Option<Vec<f64>> {
                a.get(k)?.as_array()?.iter().map(Value::as_f64).collect()
            };
            crate::takelayout::detail_add(
                core,
                project_of(req)?,
                s(a, "cut")?,
                s(a, "part")?,
                nums("crop").and_then(|v| <[f64; 4]>::try_from(v).ok()),
                nums("place").and_then(|v| <[f64; 3]>::try_from(v).ok()),
            )
        }
        "inset_delete" => {
            crate::takelayout::layer_delete(core, project_of(req)?, s(a, "cut")?, s(a, "layer")?)
        }
        "layout_copy_same_source" => {
            crate::takelayout::copy_same_source(core, project_of(req)?, s(a, "cut")?, s(a, "part")?)
        }
        "layout_copy_next" => crate::takelayout::copy_layout(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "part")?,
            Some(
                a.get("count")
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
                    .min(10_000) as usize,
            ),
        ),
        "layout_presets" => crate::takelayout::presets(core, project_of(req)?, s(a, "cut")?),
        "layout_preset_save" => crate::takelayout::preset_save(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "part")?,
            s(a, "name")?,
        ),
        "layout_preset_apply" => {
            let parts: Vec<String> = a
                .get("parts")
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            crate::takelayout::preset_apply(
                core,
                project_of(req)?,
                s(a, "cut")?,
                s(a, "name")?,
                &parts,
            )
        }
        "layout_preset_delete" => crate::takelayout::preset_delete(core, s(a, "name")?),
        "layout_preset_rename" => {
            crate::takelayout::preset_rename(core, s(a, "name")?, s(a, "to")?)
        }
        "layer_crop_norm" => {
            let r = (|| {
                crate::framing::crop_norm_group(
                    core,
                    project_of(req)?,
                    s(a, "cut")?,
                    s(a, "part")?,
                    s(a, "layer")?,
                    [n(a, "x")?, n(a, "y")?, n(a, "w")?, n(a, "h")?],
                    &crate::framing::group_arg(a),
                )
            })();
            // a crop-pane drag (U.12) ends its gesture with its commit
            if let (Some(g), Ok(p)) = (a.get("gesture").and_then(Value::as_u64), project_of(req)) {
                crate::lanes::gesture_end(core, p, g);
            }
            r
        }
        "short_view" => crate::short::short_view(core, project_of(req)?, s(a, "cut")?),
        "captions_set" => crate::short::captions_set(
            core,
            project_of(req)?,
            s(a, "cut")?,
            a.get("on").and_then(Value::as_bool).unwrap_or(true),
        ),
        "caption_replace" => {
            let hits: Vec<Vec<u64>> = a
                .get("hits")
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .map(|h| {
                            h.as_array()
                                .map(|x| x.iter().filter_map(Value::as_u64).collect())
                                .unwrap_or_default()
                        })
                        .collect()
                })
                .unwrap_or_default();
            crate::short::caption_replace(
                core,
                project_of(req)?,
                s(a, "cut")?,
                s(a, "rev")?,
                &hits,
                a.get("to").and_then(Value::as_str).unwrap_or(""),
            )
        }
        "caption_fix" => crate::short::caption_fix(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "from")?,
            s(a, "to")?,
        ),
        "caption_keywords" => crate::short::keywords_set(
            core,
            project_of(req)?,
            s(a, "cut")?,
            a.get("terms").and_then(Value::as_str).unwrap_or(""),
        ),
        "cue_break" => crate::short::cue_break(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "rev")?,
            a.get("ordinal")
                .and_then(Value::as_u64)
                .ok_or_else(|| AppError::invalid("ordinal"))?,
            a.get("break").and_then(Value::as_str),
        ),
        "hook_set" => crate::short::hook_set_with(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "text")?,
            a.get("until").and_then(Value::as_u64),
            a.get("dur_ms")
                .and_then(Value::as_u64)
                .map(|d| d.min(60_000) as u32),
            a.get("provenance").cloned(),
        ),
        "pill_add" => {
            let (p, c) = (project_of(req)?, s(a, "cut")?);
            let part = match a.get("part").and_then(Value::as_str) {
                Some(x) => x.to_owned(),
                None => crate::short::part_at(core, p, c, n(a, "frame")?.max(0.0) as u64)?,
            };
            crate::short::pill_add_span(
                core,
                p,
                c,
                &part,
                a.get("to_part").and_then(Value::as_str),
                s(a, "text")?,
            )
        }
        "run_measure_src" => crate::short::run_measure_src(
            core,
            project_of(req)?,
            s(a, "cut")?,
            n(a, "from_s")?,
            n(a, "to_s")?,
        ),
        "run_measure" => crate::short::run_measure(
            core,
            project_of(req)?,
            s(a, "cut")?,
            n(a, "from")?.max(0.0) as u64,
            n(a, "to")?.max(0.0) as u64,
        ),
        "clock_add" => crate::short::clock_add_span(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "measurement")?,
            s(a, "running")?,
            s(a, "done")?,
            a.get("from_part").and_then(Value::as_str),
            a.get("until_part").and_then(Value::as_str),
        ),
        "short_overlay_delete" => {
            crate::short::overlay_delete(core, project_of(req)?, s(a, "cut")?, s(a, "overlay")?)
        }
        // decision 0053: a part's level on its own source past its edges, for a trim drag
        "timeline_part_energy" => {
            let (pid, cut, part) = (project_of(req)?, s(a, "cut")?, s(a, "part")?);
            let n = |k: &str| a.get(k).and_then(Value::as_u64).unwrap_or(0);
            Ok(crate::columns::part_energy(core, pid, cut, part, n("before"), n("after"))
                .unwrap_or(Value::Null))
        }
        "timeline_audio" => crate::tighten::timeline_audio_with(
            core,
            project_of(req)?,
            s(a, "cut")?,
            a.get("onsets_only")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
        "timeline_lanes" => crate::lanes::view(core, project_of(req)?, s(a, "cut")?),
        "timeline_gesture_begin" => crate::lanes::gesture_begin_with(
            core,
            project_of(req)?,
            s(a, "kind")?,
            s(a, "id")?,
            a.get("part").and_then(Value::as_str),
        ),
        "commands_catalog" => Ok(crate::catalog::catalog()),
        "viewer_gizmos" => crate::gizmos::view(
            core,
            project_of(req)?,
            s(a, "cut")?,
            n(a, "frame")?.max(0.0) as u64,
        ),
        "overlay_drag" => crate::gizmos::overlay_drag(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "overlay")?,
            (n(a, "x")?, n(a, "y")?),
            a.get("gesture").and_then(Value::as_u64),
        ),
        "caption_seat" => crate::gizmos::caption_seat(
            core,
            project_of(req)?,
            s(a, "cut")?,
            n(a, "y")?,
            a.get("part").and_then(Value::as_str),
            a.get("gesture").and_then(Value::as_u64),
        ),
        "timeline_gesture_end" => {
            crate::lanes::gesture_end(core, project_of(req)?, n(a, "gesture")? as u64);
            Ok(json!({}))
        }
        "overlay_split_at" => crate::lanes::overlay_split_at(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "overlay")?,
            n(a, "frame")? as i64,
        ),
        "overlay_time_set" => crate::lanes::overlay_time(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "overlay")?,
            a.get("from").and_then(Value::as_f64).map(|x| x as i64),
            a.get("to").and_then(Value::as_f64).map(|x| x as i64),
            a.get("gesture").and_then(Value::as_u64),
        ),
        "gain_segment_edit" => crate::lanes::gain_set(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "segment")?,
            a.get("from").and_then(Value::as_f64).map(|x| x as i64),
            a.get("to").and_then(Value::as_f64).map(|x| x as i64),
            a.get("gain_db")
                .and_then(Value::as_f64)
                .filter(|g| g.is_finite()),
            a.get("gesture").and_then(Value::as_u64),
        ),
        "sweep_preview" => crate::tighten::sweep_preview(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "kind")?,
            a.get("min_s").and_then(Value::as_f64).unwrap_or(0.32),
            a.get("keep_s").and_then(Value::as_f64).unwrap_or(0.10),
        ),
        "sweep_apply_app" => {
            let take: Vec<usize> = a
                .get("take")
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .filter_map(Value::as_u64)
                        .map(|x| x as usize)
                        .take(10_000)
                        .collect()
                })
                .unwrap_or_default();
            crate::tighten::sweep_apply(
                core,
                project_of(req)?,
                s(a, "cut")?,
                s(a, "preview_id")?,
                &take,
            )
        }
        "hold_toggle" => crate::tighten::hold_toggle(
            core,
            project_of(req)?,
            s(a, "cut")?,
            n(a, "frame")?.max(0.0) as u64,
        ),
        "parts_move_to" => crate::tighten::parts_move_to(
            core,
            project_of(req)?,
            s(a, "cut")?,
            &strings(a, "parts")?,
            n(a, "index")?.max(0.0) as usize,
        ),
        "part_reorder_to" => crate::tighten::reorder_to(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "part")?,
            n(a, "index")?.max(0.0) as usize,
        ),
        // The same event a native menu item sends (the menu's tests; a UI that wants to
        // replay a menu item).
        "app_menu" => {
            let id = s(a, "id")?;
            if !crate::shell::MENU_IDS.contains(&id) {
                return Err(AppError::invalid("no such menu item"));
            }
            core.bus
                .app(crate::events::AppEvent::Menu { id: id.to_owned() });
            Ok(json!({}))
        }
        // R-OWN-1b: the webview asks; the native sheet decides (never over the dev bridge).
        "review_status" => Ok(crate::reviewlink::status(core)),
        "review_serve" => {
            crate::reviewlink::serve(core, a.get("on").and_then(Value::as_bool).unwrap_or(true))
        }
        "review_link_new" => {
            crate::reviewlink::link_new(core, a.get("login").and_then(Value::as_str))
        }
        // decision 0015: a reviewer's or an editor's link
        "review_link_role" => crate::reviewlink::link_new_as(
            core,
            a.get("login").and_then(Value::as_str),
            a.get("role").and_then(Value::as_str),
        ),
        c if crate::vroutes::COMMANDS.contains(&c) => {
            crate::vroutes::dispatch(core, c, req.project.as_deref(), a)
        }
        "review_link_revoke" => {
            crate::reviewlink::revoke(core, a.get("id").and_then(Value::as_str))
        }
        "approval_resolve" => crate::board::approval_resolve(
            core,
            a.get("id").and_then(Value::as_str),
            a.get("run")
                .or_else(|| a.get("identity"))
                .and_then(Value::as_str),
        ),
        // "Approve and export" (decision 0050): the human exports what the agent asked to
        "approval_export" => crate::board::approval_export(core, &ctx.jobs, s(a, "id")?),
        "owner_request" => crate::owner::request(
            core,
            s(a, "command")?,
            a.get("args").unwrap_or(&Value::Null),
        ),
        "setup_preview" => Ok(crate::firstrun::preview(core)),
        "setup_state" => Ok(crate::firstrun::state(core)),
        "setup_apply" => {
            let ans: crate::firstrun::Answers = serde_json::from_value(
                a.get("answers").cloned().unwrap_or(Value::Null),
            )
            .map_err(|_| {
                AppError::invalid("answers: {kit, platforms, finder, harness?, recordings?}")
            })?;
            crate::firstrun::apply(
                core,
                &ans,
                a.get("dry_run").and_then(Value::as_bool).unwrap_or(false),
            )
        }
        "setup_no_transcribe" => crate::firstrun::set_no_transcribe(
            core,
            a.get("on").and_then(Value::as_bool).unwrap_or(true),
        ),
        "checklist_view" => Ok(crate::firstrun::checklist(core)),
        "checklist_dismiss" => crate::firstrun::dismiss_checklist(
            core,
            a.get("dismissed").and_then(Value::as_bool).unwrap_or(true),
        ),
        "backups_overview" => Ok(crate::backups::overview(core)),
        "backup_now" => {
            let pid = project_of(req)?;
            let b = crate::backups::maybe_backup_open(core, pid, "manual")?;
            Ok(json!({"backup": b}))
        }
        "backup_restore" => crate::backups::restore(core, s(a, "project")?, s(a, "backup")?),
        "storage_usage" => Ok(crate::storage::usage(core)),
        "storage_free" => {
            let kinds: Vec<String> = a
                .get("kinds")
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            crate::storage::free(core, &kinds)
        }
        "board_view" => crate::board::board(core, s(a, "project")?),
        "export_verdict" => crate::board::verdict(
            core,
            s(a, "project")?,
            s(a, "export")?,
            a.get("verdict").and_then(Value::as_str),
        ),
        "clippy_pair" => crate::board::pair(core),
        "clippy_send" => crate::board::send(core, s(a, "project")?, s(a, "export")?),
        "activity" => Ok(crate::board::activity(core)),
        "feedback_preview" => crate::board::feedback_preview(core, s(a, "text")?),
        "feedback_append" => crate::board::feedback_append(
            core,
            s(a, "text")?,
            a.get("export").and_then(Value::as_str),
            a.get("link").and_then(Value::as_str),
        ),
        "first_seen_ack" => crate::board::first_seen_ack(
            core,
            a.get("ts")
                .and_then(Value::as_i64)
                .ok_or_else(|| AppError::invalid("ts"))?,
        ),
        "agents_pause" => {
            crate::board::pause(core, a.get("on").and_then(Value::as_bool).unwrap_or(true))
        }
        "agent_run_stop" => crate::board::stop_run(core, s(a, "run")?),
        // decision 0035: agent sessions. Starting one and every message is a human
        // click in the app (R-AGT-3); never an agent surface.
        "session_agents" => Ok(crate::sessions::agents(core)),
        "session_list" => Ok(crate::sessions::list(core)),
        "session_get" => crate::sessions::get(core, s(a, "session")?),
        "session_start" => crate::sessions::start(
            core,
            a.get("target").unwrap_or(&Value::Null),
            crate::sessions::StartArgs {
                harness: harness_arg(a)?.unwrap_or_else(|| crate::agent::default_harness(core)),
                access: a
                    .get("access")
                    .and_then(Value::as_str)
                    .unwrap_or("gmclip")
                    .to_owned(),
                message: s(a, "message")?.to_owned(),
                media: a.get("media").and_then(Value::as_bool).unwrap_or(false),
                folder: a
                    .get("folder")
                    .and_then(Value::as_str)
                    .filter(|f| !f.is_empty())
                    .map(std::path::PathBuf::from),
                model: a
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|m| !m.is_empty() && m.len() <= 256)
                    .map(str::to_owned),
            },
        ),
        "session_prompt" => crate::sessions::prompt(core, s(a, "session")?, s(a, "message")?),
        "session_cancel" => crate::sessions::cancel(core, s(a, "session")?),
        "session_stop" => crate::sessions::stop(core, s(a, "session")?),
        "session_answer" => {
            crate::sessions::answer(core, s(a, "session")?, s(a, "permission")?, s(a, "option")?)
        }
        "session_mode" => crate::sessions::set_mode(core, s(a, "session")?, s(a, "mode")?),
        "session_model" => crate::sessions::set_model(core, s(a, "session")?, s(a, "model")?),
        "session_resume" => crate::sessions::resume(
            core,
            s(a, "session")?,
            a.get("message").and_then(Value::as_str),
        ),
        "session_forget" => crate::sessions::forget(core, s(a, "session")?),
        "session_undo" => crate::sessions::undo(core, s(a, "session")?),
        "models_status" => Ok(crate::models::status()),
        // A click (never on its own): fetch the missing pinned model files.
        "models_download" => crate::models::download(core),
        "models_pause" => Ok(crate::models::pause()),
        "owner_settings" => Ok(crate::owner::settings(core)),
        // Owner (R-PROF-5): the native panel, then the native sheet
        "style_pick" => crate::owner::style_pick(core),
        // Owner (R-PROF-3): the batched diff sheet, on request
        "style_review" => crate::owner::style_changes_sheet(core, true),
        // The person's view choices (app settings, never the profile).
        "ui_prefs_get" => Ok(crate::uiprefs::get(core)),
        "ui_prefs_set" => crate::uiprefs::put(core, a),
        "agent_status" => {
            let mut v = core.agents.status(
                req.project
                    .as_deref()
                    .or_else(|| a.get("project").and_then(Value::as_str)),
            );
            v["client"] = crate::agent::client_status(core);
            let c = crate::agent::clients_json(core, false);
            v["clients"] = c["clients"].clone();
            v["default"] = c["default"].clone();
            Ok(v)
        }
        "agent_client_set" => crate::agent::set_client(
            core,
            s(a, "path")?,
            harness_arg(a)?.unwrap_or(gmclip_ai::launcher::Harness::ClaudeCode),
        ),
        // G.02: the user's agents (detected by path, never a shell) and the default
        "agent_clients" => Ok(crate::agent::clients_json(
            core,
            a.get("refresh").and_then(Value::as_bool).unwrap_or(false),
        )),
        "agent_default_set" => crate::agent::set_default(core, s(a, "harness")?),
        // G.03: a human click on a finished run (R-AGT-3), never an agent surface
        "agent_continue" => crate::agent::continue_run(core, s(a, "run")?, s(a, "note")?),
        // A human click in the app (R-AGT-3): the only way the UI starts a harness.
        "batch_start" => {
            let cuts: Vec<String> = a
                .get("cuts")
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            crate::batch::start(
                core,
                project_of(req)?,
                &cuts,
                s(a, "pass")?,
                &batch_options(a),
            )
        }
        "batch_list" => Ok(crate::batch::list(core, project_of(req)?)),
        "agent_run" => crate::agent::run_pass_as(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "pass")?,
            a.get("brief").and_then(Value::as_str).unwrap_or(""),
            a.get("media").and_then(Value::as_bool).unwrap_or(false),
            harness_arg(a)?,
        ),
        "project_save_state" => {
            let p = core.project(project_of(req)?)?;
            let rev = p.handle.snapshot().doc.rev();
            let (saved, error) = p.handle.saved();
            Ok(json!({"rev": rev, "saved": saved, "error": error,
                      "state": if error.is_some() { "failed" } else if saved >= rev { "saved" } else { "saving" }}))
        }
        "project_restore_point" => {
            // ⌘S: everything committed made durable now, plus a restore point to go back to.
            let pid = project_of(req)?;
            let p = core.project(pid)?;
            let flushed = p.handle.flush();
            let r = core.call(pid, "restore_point_create", json!({}))?;
            let (saved, error) = p.handle.saved();
            Ok(
                json!({"restore_point": r["output"], "flushed": flushed, "saved": saved, "error": error}),
            )
        }
        "transcript_view" => crate::transcript::transcript_view_of(
            core,
            project_of(req)?,
            s(a, "cut")?,
            a.get("owner").and_then(Value::as_str),
        ),
        "analysis_status" => Ok(crate::analysis::status(core, project_of(req)?)),
        // The stream edit (decision 0011): topics by word ordinal, the map, framings.
        "stream_overlay" => crate::streamedit::stream_overlay(core, project_of(req)?, s(a, "cut")?),
        "project_map_view" => crate::streamedit::project_map_view(core, project_of(req)?),
        "topic_label_ord" => crate::streamedit::label(
            core,
            project_of(req)?,
            s(a, "cut")?,
            n(a, "from")? as u64,
            n(a, "to")? as u64,
            s(a, "title")?,
            a.get("colour").and_then(Value::as_str),
            a.get("owner").and_then(Value::as_str),
        ),
        "topic_edge_ord" => crate::streamedit::edge(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "topic")?,
            s(a, "edge")?,
            n(a, "word")? as u64,
        ),
        "topic_split_ord" => crate::streamedit::split(
            core,
            project_of(req)?,
            s(a, "cut")?,
            s(a, "topic")?,
            n(a, "at")? as u64,
            a.get("title").and_then(Value::as_str),
        ),
        "framing_pick" => {
            let (p, c) = (project_of(req)?, s(a, "cut")?);
            let r = crate::streamedit::framing_pick(core, p, c, s(a, "framing")?)?;
            ctx.viewer.send(ViewerMsg::Load {
                project: p.into(),
                cut: c.into(),
                framing: None,
                keep: true,
            });
            Ok(r)
        }
        "home" => Ok(home(ctx)),
        "library_search" | "library_play" | "library_open" | "library_topic_open"
        | "library_moment" => crate::libsearch::ipc(ctx, &req.command, a),
        c if crate::ideas::COMMANDS.contains(&c) => crate::ideas::ipc(ctx, c, project_of(req)?, a),
        c if crate::polish::COMMANDS.contains(&c) => {
            crate::polish::ipc(&ctx.core, c, project_of(req)?, a)
        }
        "chat_import" | "chat_view" | "chat_nudge" | "chat_ribbon" | "chat_moment" => {
            crate::chat::ipc(ctx, &req.command, req.project.as_deref(), a)
        }
        "tap_mark" => crate::tapmark::mark(core, project_of(req)?, a),
        "cut_like" | "cut_template_save" | "cut_templates" | "cut_template_delete" => {
            crate::cutlike::ipc(core, &req.command, req.project.as_deref(), a)
        }
        "project_consolidate" | "project_open_copy" => {
            crate::consolidate::ipc(core, &req.command, req.project.as_deref(), a)
        }
        "home_seen" => {
            if let Ok(mut g) = core.seen_ms.lock() {
                *g = now_ms();
            }
            Ok(json!({}))
        }
        name if crate::backdrop::COMMANDS.contains(&name) => {
            crate::backdrop::dispatch(core, project_of(req)?, name, a)
        }
        name if crate::studio::COMMANDS.contains(&name) => {
            crate::studio::dispatch(core, project_of(req)?, name, a)
        }
        name => {
            if gmclip_core::registry::find(name).is_none() {
                return Err(AppError::invalid("unknown command"));
            }
            let _ = &req.request_id;
            core.call(project_of(req)?, name, a.clone())
        }
    }
}

#[cfg(target_os = "macos")]
fn reveal(path: &std::path::Path) -> AppResult<()> {
    std::process::Command::new("/usr/bin/open")
        .arg("-R")
        .arg(path)
        .status()
        .map_err(|_| AppError::internal("Finder did not open"))
        .and_then(|s| {
            if s.success() {
                Ok(())
            } else {
                Err(AppError::internal("Finder did not open"))
            }
        })
}

/// No file panel to show: no window yet (macOS, Windows), or no panel on this OS yet
/// (a dropped file or a typed path still opens).
fn no_panel() -> AppError {
    AppError::new(
        "Unsupported",
        if cfg!(any(target_os = "macos", windows)) {
            "no window to show the file panel in"
        } else {
            "no file panel on this system yet: drop the video on the window instead"
        },
    )
}

/// Shows the file selected in File Explorer. Explorer exits 1 even when it opened, so
/// only a failure to start counts; `/select,"…"` goes through as written (a Windows path
/// cannot hold a `"`).
#[cfg(windows)]
fn reveal(path: &std::path::Path) -> AppResult<()> {
    use std::os::windows::process::CommandExt;
    let p = path.to_string_lossy();
    if p.contains('"') || !path.is_absolute() {
        return Err(AppError::invalid("an absolute path"));
    }
    let explorer = std::env::var_os("SystemRoot")
        .map(|r| std::path::PathBuf::from(r).join("explorer.exe"))
        .unwrap_or_else(|| "C:\\Windows\\explorer.exe".into());
    std::process::Command::new(explorer)
        .raw_arg(format!("/select,\"{p}\""))
        .spawn()
        .map(|_| ())
        .map_err(|_| AppError::internal("File Explorer did not open"))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn reveal(_path: &std::path::Path) -> AppResult<()> {
    Err(AppError::new(
        "Unsupported",
        "Reveal runs on macOS and Windows",
    ))
}

/// Home (SPEC §12.6) across the library: running, needs you, done since you last
/// looked, recent exports ([`crate::library::home_view`]).
pub fn home(ctx: &Ctx) -> Value {
    crate::library::home_view(&ctx.core, &ctx.jobs)
}

/// Allowed subscription scopes.
pub fn check_scope(core: &AppCore, scope: &str) -> AppResult<()> {
    match scope {
        "home" | "jobs" | "viewer" | "app" | "agents" => Ok(()),
        s if s
            .strip_prefix("project:")
            .is_some_and(|id| core.is_open(id)) =>
        {
            Ok(())
        }
        _ => Err(AppError::invalid("unknown or closed scope")),
    }
}

/// Feeds one bus subscription to `send` in batches, at most one per 16 ms, until `send`
/// fails or the subscription is removed.
pub fn pump(
    core: Arc<AppCore>,
    scope: String,
    send: impl Fn(Value) -> bool + Send + 'static,
) -> u64 {
    let sub = core.bus.subscribe(&scope);
    let id = sub.id;
    let _ = std::thread::Builder::new()
        .name("gmclip-subscription".into())
        .spawn(move || {
            loop {
                let Some(mut evs) = sub.wait(Duration::from_secs(1)) else {
                    return;
                };
                if evs.is_empty() {
                    continue;
                }
                let t0 = Instant::now();
                std::thread::sleep(Duration::from_millis(16).saturating_sub(t0.elapsed()));
                evs.extend(sub.drain());
                if !send(json!({"sub": id, "scope": scope, "events": evs})) {
                    core.bus.unsubscribe(id);
                    return;
                }
            }
        });
    id
}

/// `gm://data/<project-ulid>/<kind>?cut=<ulid>` (R-IPC-1): only an open project's ULID
/// and an enumerated kind; no path is ever accepted. Body: a 16-byte header
/// `{version u16, kind u16, count u32, rev u64}` (LE) then the columns.
pub fn gm_data(core: &AppCore, uri: &str) -> Option<Vec<u8>> {
    let req = crate::gmuri::parse(uri)?;
    let (pid, kind, cut) = (req.project, req.kind, req.cut);
    if gmclip_model::Ulid::parse(pid).is_err() || !core.is_open(pid) {
        return None;
    }
    let header = |kind: u16, count: u32, rev: u64| {
        let mut b = Vec::with_capacity(16);
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&kind.to_le_bytes());
        b.extend_from_slice(&count.to_le_bytes());
        b.extend_from_slice(&rev.to_le_bytes());
        b
    };
    match kind {
        // Per part: out start, out end, source first frame, source end frame (u32 each).
        "parts" => {
            let pl = core.planned(pid, cut?).ok()?;
            let mut b = header(1, pl.plan.parts.len() as u32, pl.rev);
            for p in &pl.plan.parts {
                for v in [
                    p.out.start(),
                    p.out.end(),
                    p.picture.first_frame,
                    p.picture.first_frame + p.out.len(),
                ] {
                    b.extend_from_slice(&(v.min(u64::from(u32::MAX)) as u32).to_le_bytes());
                }
            }
            Some(b)
        }
        // The timeline's columns on the output clock (D.02, [`crate::columns`]).
        "words" | "peaks" | "energy" | "quiet" => crate::columns::column(core, pid, cut?, kind),
        // Timeline thumbnails from the scrub proxy (H.05).
        "filmstrip" => crate::proxy::filmstrip(core, pid, cut?, req.every, req.width),
        _ => None,
    }
}

/// An optional `harness` argument (`claude-code`, `codex`, `pi`, `opencode`).
/// The batch sheet's numbers (SPEC §10.7.1): `budget_usd`, `wall_clock_h`,
/// `per_cut_min`, `harness`; checked by [`crate::batch::BatchOptions::check`].
fn batch_options(a: &Value) -> crate::batch::BatchOptions {
    let num = |k: &str| a.get(k).and_then(Value::as_f64);
    let mut opts = crate::batch::BatchOptions::default();
    if let Some(h) = num("wall_clock_h") {
        opts.wall_clock = Duration::from_secs_f64((h * 3600.0).clamp(0.0, 1e7));
    }
    if let Some(m) = num("per_cut_min") {
        opts.per_cut = Duration::from_secs_f64((m * 60.0).clamp(0.0, 1e7));
    }
    if let Some(b) = num("budget_usd") {
        opts.budget_usd = b;
    }
    opts.harness = a.get("harness").and_then(Value::as_str).map(str::to_owned);
    opts
}

fn harness_arg(a: &Value) -> AppResult<Option<gmclip_ai::launcher::Harness>> {
    match a.get("harness").and_then(Value::as_str) {
        None => Ok(None),
        Some(h) => gmclip_ai::launcher::Harness::parse(h)
            .map(Some)
            .ok_or_else(|| AppError::invalid("harness is claude-code, codex, pi or opencode")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Dirs;

    fn core() -> Arc<AppCore> {
        AppCore::new(Dirs {
            app_data: std::env::temp_dir().join("gmclip-ipc-test"),
            library: std::env::temp_dir().join("gmclip-ipc-test/lib"),
            exports: std::env::temp_dir().join("gmclip-ipc-test/exp"),
        })
    }

    /// R-IPC-1: path-shaped or closed-project selectors are refused.
    #[test]
    fn gm_data_refuses_paths_and_closed_projects() {
        let c = core();
        for uri in [
            "gm://data/../../etc/passwd/parts",
            "gm://data/01ARZ3NDEKTSV4RRFFQ69G5FAV/parts",
            "gm://data/01ARZ3NDEKTSV4RRFFQ69G5FAV/../../x",
            "gm://data/01ARZ3NDEKTSV4RRFFQ69G5FAV/words?path=/etc",
            "gm://other/01ARZ3NDEKTSV4RRFFQ69G5FAV/words",
            "gm://data/%2e%2e/words",
        ] {
            assert!(gm_data(&c, uri).is_none(), "{uri}");
        }
    }

    #[test]
    fn scopes_are_enumerated() {
        let c = core();
        assert!(check_scope(&c, "jobs").is_ok());
        assert!(check_scope(&c, "project:01ARZ3NDEKTSV4RRFFQ69G5FAV").is_err());
        assert!(check_scope(&c, "anything").is_err());
    }
}
