//! The app's commands agents may run (MCP parity with the window, AJ 2026-10-18:
//! "everything we can do with gui we should be able to do with agent"). The holder routes
//! `command_run{command}` here for a name that is not a registry command; it has already
//! refused runs bound to one cut, checked the `edit` scope and checked every path argument
//! against the agent read roots (R-FS-1).
//!
//! Left out on purpose: Owner actions and their sheets (R-OWN-1), the human's decisions
//! (verdicts, golden marks, thumbnail and comment choices, review links, feedback, Clippy),
//! agent and session management, settings and first run, and the window's own plumbing
//! (the viewer, gestures, native file panels, menus, drag and reveal).

use std::time::Duration;

use gmclip_core::Caller;
use gmclip_mcp::holder::AppJob;
use gmclip_proto::CmdError;
use serde_json::{Value, json};

use crate::ipc::{self, CallRequestWire, Ctx};

/// `(name, doc)` of each app command agents may run; `project` defaults to the open one.
pub const COMMANDS: &[(&str, &str)] = &[
    // projects and the library
    (
        "project_new",
        "Creates an empty project and opens it. Args: {name}.",
    ),
    (
        "project_list_open",
        "The projects open in the app (id, name, home).",
    ),
    (
        "project_open_home",
        "Opens a project folder (a .gmclip home). Args: {path}.",
    ),
    ("project_close", "Closes a project. Args: {project}."),
    (
        "project_map_view",
        "The project map: its videos, the main cut, shorts and long-forms.",
    ),
    ("project_save_state", "Saves the project now."),
    (
        "project_restore_point",
        "Makes a restore point of the project.",
    ),
    (
        "project_consolidate",
        "Copies the project and every file it uses into one folder. Args: {dest?, name?}.",
    ),
    (
        "project_open_copy",
        "Opens a consolidated copy. Args: {home}.",
    ),
    (
        "snapshot_named",
        "Names a snapshot of the project's history. Args: {name}.",
    ),
    ("history_count", "How many history entries the project has."),
    (
        "history_restore",
        "Restores a cut to a snapshot (undoable). Args: {cut, snapshot}.",
    ),
    ("backup_now", "Backs up the open project now."),
    ("backups_overview", "The project backups on this Mac."),
    (
        "home",
        "What Home shows: recent projects, running work, the checklist.",
    ),
    ("videos_recent", "Recently opened videos."),
    (
        "library_moment",
        "One library hit as a moment. Args: {home, ...the hit}.",
    ),
    ("storage_usage", "Disk used by caches, proxies and exports."),
    (
        "storage_free",
        "Frees rebuildable caches. Args: {kinds: [..]}.",
    ),
    (
        "models_status",
        "The speech models: present, downloading or missing.",
    ),
    ("models_download", "Downloads the missing speech models."),
    (
        "jobs_list",
        "Running and recent jobs (exports, transcripts, analysis).",
    ),
    (
        "analysis_status",
        "Transcription and analysis state of the project's videos.",
    ),
    // videos
    (
        "video_inspect",
        "Probes a video file before opening it. Args: {path}.",
    ),
    (
        "videos_inspect",
        "Probes several video files. Args: {paths: [..]}.",
    ),
    (
        "video_open",
        "Opens a video as a new project (transcribed in the background). Args: {path, voice?, hires?}.",
    ),
    (
        "videos_open",
        "Opens several videos as one new project (or one each). Args: {files: [{path, voice?}], name?, one_project?, same?: [[i, j]]}.",
    ),
    (
        "videos_add",
        "Adds videos to the project, on the end of its main cut. Args: {files: [{path, voice?}], same?}.",
    ),
    (
        "video_remove",
        "Removes a video and its parts from the project (undoable). Args: {source}.",
    ),
    (
        "video_relink",
        "Points a missing video at its new file. Args: {source, path}.",
    ),
    (
        "picture_offset_set",
        "Sets how much later a hi-res picture starts than the voice file. Args: {cut, offset_s, frame?, split?}.",
    ),
    (
        "pair_propose",
        "Proposes how a voice file and a hi-res picture line up. Args: {voice, hires, voice_track?}.",
    ),
    (
        "cut_import",
        "Imports a .renderplan.json as a cut. Args: {path}.",
    ),
    (
        "chat_import",
        "Imports a stream's chat log for a video. Args: {source, path, ...}.",
    ),
    (
        "chat_view",
        "A video's imported chat and its spikes. Args: {source}.",
    ),
    (
        "chat_moment",
        "Makes a moment from a chat spike. Args: {source, ...the spike}.",
    ),
    (
        "retention_import",
        "Imports an audience-retention curve for an export. Args: {export, text, name?}.",
    ),
    (
        "retention_view",
        "A cut's retention curve against its parts. Args: {cut}.",
    ),
    // cuts, parts and words
    (
        "cut_view",
        "Everything the cut view shows for one cut. Args: {cut}.",
    ),
    (
        "cut_checks",
        "The cut's checks (gaps, captions, safe zones, ...). Args: {cut}.",
    ),
    ("check_fix", "Applies a check's fix. Args: {cut, rule, n?}."),
    (
        "cut_trim",
        "Trims a part's edge by frames. Args: {cut, part, edge: in|out, delta}.",
    ),
    (
        "cut_split",
        "Splits the cut at a frame. Args: {cut, frame}.",
    ),
    (
        "cut_range_delete",
        "Ripple-deletes frames from..to. Args: {cut, from, to}.",
    ),
    (
        "cut_words",
        "The cut's words by part, struck or kept. Args: {cut}.",
    ),
    (
        "words_strike_ord",
        "Strikes words from..to (ordinals) out of the cut. Args: {cut, owner, from, to}.",
    ),
    (
        "words_restore_ord",
        "Restores struck words. Args: {cut, owner, from, to}.",
    ),
    (
        "word_nudge_ord",
        "Nudges a word's edge. Args: {cut, owner, word, edge, delta_ms}.",
    ),
    (
        "word_respell",
        "Changes how a word is written in captions. Args: {cut, owner, word, text}.",
    ),
    (
        "word_edge_reset",
        "Resets a word's nudged edges. Args: {cut, owner, word}.",
    ),
    (
        "words_add_part",
        "Adds words from..to as a new part. Args: {cut, owner, from, to, after?}.",
    ),
    (
        "words_stretch_last",
        "Stretches the last part to a word. Args: {cut, owner, to}.",
    ),
    (
        "transcript_view",
        "The transcript the cut's words come from. Args: {cut, owner?}.",
    ),
    (
        "story_view",
        "The cut as a story: parts, times, beats. Args: {cut}.",
    ),
    (
        "part_time_set",
        "Sets a part's edge to a time. Args: {cut, part, edge, text}.",
    ),
    (
        "part_nudge_ms",
        "Nudges a part's edge. Args: {cut, part, edge, delta_ms}.",
    ),
    (
        "part_move",
        "Moves a part up or down. Args: {cut, part, dir}.",
    ),
    (
        "part_reorder_to",
        "Moves a part to an index. Args: {cut, part, index}.",
    ),
    (
        "parts_move_to",
        "Moves several parts to an index together. Args: {cut, parts: [..], index}.",
    ),
    (
        "cut_chronological",
        "Puts the cut's parts back in recording order. Args: {cut}.",
    ),
    (
        "hold_at",
        "Holds a part's frame for a while. Args: {cut, part, at_s}.",
    ),
    (
        "hold_toggle",
        "Toggles a hold at a frame. Args: {cut, frame}.",
    ),
    ("holds_find", "Finds places a hold would help. Args: {cut}."),
    (
        "sweep_apply_app",
        "Applies a sweep preview. Args: {cut, preview_id, take?}.",
    ),
    (
        "moment_review",
        "A moment with its clip and transcript. Args: {moment, next?}.",
    ),
    (
        "moment_clip",
        "Makes (or finds) a moment's preview clip. Args: {moment}.",
    ),
    (
        "cut_from_moment",
        "Makes a new short cut from a moment. Args: {moment}.",
    ),
    ("tap_mark", "Marks a moment at a time. Args: {cut, ...}."),
    (
        "cut_like",
        "Makes a new cut like a template. Args: {cut, template?, save_as?}.",
    ),
    (
        "cut_template_save",
        "Saves a cut's shape as a template. Args: {cut, save_as}.",
    ),
    ("cut_templates", "The saved cut templates."),
    (
        "cut_template_delete",
        "Deletes a cut template. Args: {template}.",
    ),
    (
        "stream_view",
        "The stream (main cut) around a time. Args: {cut, at_s}.",
    ),
    (
        "stream_overlay",
        "The main cut's topics and parts over the stream. Args: {cut}.",
    ),
    (
        "topic_label_ord",
        "Labels words from..to as a topic. Args: {cut, owner, from, to, title, colour?}.",
    ),
    (
        "topic_edge_ord",
        "Moves a topic's edge to a word. Args: {cut, topic, edge, word}.",
    ),
    (
        "topic_split_ord",
        "Splits a topic at a word. Args: {cut, topic, at, title?}.",
    ),
    (
        "framing_pick",
        "Picks the framing a cut uses. Args: {cut, framing}.",
    ),
    (
        "timeline_lanes",
        "The timeline's lanes (parts, overlays, captions, sound). Args: {cut}.",
    ),
    (
        "timeline_audio",
        "The cut's waveform and onsets. Args: {cut, onsets_only?}.",
    ),
    (
        "longform_compare_view",
        "Compares a long-form with its shorts. Args: {cut}.",
    ),
    ("reactions_find", "Finds reaction moments. Args: {cut}."),
    // layout
    (
        "cut_output_set",
        "Sets the cut's output aspect. Args: {cut, aspect}.",
    ),
    (
        "layout_preset",
        "Applies a layout preset to parts. Args: {cut, preset, parts?}.",
    ),
    (
        "layout_apply_all",
        "Applies a part's layout to every part. Args: {cut, part}.",
    ),
    (
        "layout_copy_next",
        "Copies a part's layout to the next parts. Args: {cut, part, count}.",
    ),
    (
        "layout_copy_same_source",
        "Copies a part's layout and crops to every other part from the same source video. Args: {cut, part}.",
    ),
    ("layout_presets", "The saved layout presets. Args: {cut}."),
    (
        "layout_preset_save",
        "Saves a part's layout as a preset. Args: {cut, part, name}.",
    ),
    (
        "layout_preset_apply",
        "Applies a saved layout preset. Args: {cut, name, parts?}.",
    ),
    (
        "layout_preset_delete",
        "Deletes a saved layout preset. Args: {name}.",
    ),
    (
        "layout_preset_rename",
        "Renames a saved layout preset. Args: {name, to}.",
    ),
    (
        "hook_layout_keep",
        "Keeps the opening's layout until the hook ends. Args: {cut}.",
    ),
    (
        "inset_add",
        "Adds a detail inset to a part. Args: {cut, part}.",
    ),
    (
        "inset_delete",
        "Deletes an inset layer. Args: {cut, layer}.",
    ),
    (
        "layer_place_px",
        "Places a layer in pixels. Args: {cut, part, layer, x, y, w, rev?}.",
    ),
    (
        "layer_crop_px",
        "Crops a layer in pixels. Args: {cut, part, layer, x, y, w, h, rev?}.",
    ),
    (
        "layer_crop_norm",
        "Crops a layer in 0..1 units. Args: {cut, part, layer, x, y, w, h}.",
    ),
    (
        "layout_draw_view",
        "The draw-a-layout view of a video. Args: {source}.",
    ),
    (
        "layout_draw_frame",
        "A frame to draw a layout on. Args: {source, at_s}.",
    ),
    (
        "layout_draw_save",
        "Saves drawn regions as a layout. Args: {source, regions, preset?}.",
    ),
    (
        "backdrop_set",
        "Sets the cut's backdrop. Args: {cut, style, asset?}.",
    ),
    (
        "video_overlay_add",
        "Adds a video file as an overlay. Args: {cut, path, frame?, span?, fit?, loop?}.",
    ),
    (
        "overlay_track",
        "Tracks an overlay to the picture. Args: {cut, overlay}.",
    ),
    (
        "cover_add",
        "Covers (pixelates or blocks) a region. Args: {cut, frame, style, ...}.",
    ),
    (
        "cover_track",
        "Tracks a cover to the picture. Args: {cut, overlay}.",
    ),
    (
        "matte_status",
        "The cut's matte (cut-out) state. Args: {cut}.",
    ),
    ("matte_build", "Builds the cut's mattes. Args: {cut}."),
    ("matte_progress", "Matte build progress. Args: {cut}."),
    (
        "follow_action",
        "Crops to follow the action. Args: {cut, part?, dry_run?}.",
    ),
    (
        "follow_cursor",
        "Crops to follow the mouse cursor. Args: {cut, on, part?, max_zoom?}.",
    ),
    // captions, overlays and the look
    (
        "short_view",
        "Everything the Short panel shows: captions, hook, overlays. Args: {cut}.",
    ),
    ("captions_set", "Turns captions on or off. Args: {cut, on}."),
    (
        "caption_fix",
        "Fixes a misheard caption word. Args: {cut, from, to}.",
    ),
    (
        "caption_replace",
        "Replaces caption text at hits. Args: {cut, hits, to, rev?}.",
    ),
    (
        "caption_keywords",
        "Sets the words captions emphasise. Args: {cut, terms: [..]}.",
    ),
    (
        "caption_keep",
        "Keeps caption words as written. Args: {words, keep}.",
    ),
    ("caption_y", "Moves captions up or down. Args: {cut, y}."),
    (
        "cue_break",
        "Breaks or joins caption cues. Args: {cut, ordinal, break, rev?}.",
    ),
    (
        "term_gate",
        "Terms the cut's captions may get wrong. Args: {cut}.",
    ),
    (
        "term_add",
        "Adds a term (how it is heard and written). Args: {heard_as, written, remove?}.",
    ),
    (
        "term_fix_everywhere",
        "Fixes a term across the project. Args: {cut, owner, written, profile?}.",
    ),
    ("terms_view", "The saved terms."),
    (
        "hook_set",
        "Sets the hook text over the opening. Args: {cut, text, dur_ms?, until?, provenance?}.",
    ),
    (
        "pill_add",
        "Adds a text pill over parts. Args: {cut, part, text, frame?, to_part?}.",
    ),
    (
        "clock_add",
        "Adds a running clock overlay. Args: {cut, from_part, until_part, measurement, running?, done?}.",
    ),
    (
        "run_measure",
        "Measures a run (time between frames) for a clock. Args: {cut, from, to}.",
    ),
    (
        "run_measure_src",
        "Measures a run in source seconds. Args: {cut, from_s, to_s}.",
    ),
    (
        "short_overlay_delete",
        "Deletes an overlay. Args: {cut, overlay}.",
    ),
    (
        "overlay_split_at",
        "Cuts an overlay in two at an output frame. Args: {cut, overlay, frame}.",
    ),
    (
        "overlay_time_set",
        "Sets an overlay's time span. Args: {cut, overlay, from, to}.",
    ),
    ("overlays_view", "The cut's overlays. Args: {cut}."),
    (
        "caption_gallery",
        "Caption styles drawn on a frame. Args: {cut, frame?}.",
    ),
    (
        "caption_style_apply",
        "Applies a caption style. Args: {cut, name}.",
    ),
    (
        "caption_style_save_mine",
        "Saves the cut's caption style as mine. Args: {cut, name}.",
    ),
    (
        "caption_style_forget_mine",
        "Forgets one of my caption styles. Args: {name}.",
    ),
    (
        "caption_style_match",
        "Matches captions to a reference short. Args: {path}.",
    ),
    (
        "captions_file",
        "Writes the cut's captions as a file. Args: {cut, format: srt|vtt}.",
    ),
    (
        "image_overlay_add",
        "Adds an image overlay from a file. Args: {cut, path, frame?, name?}.",
    ),
    ("callout_add", "Adds a callout. Args: {cut, frame, kind}."),
    (
        "lower_third_add_at",
        "Adds a lower third. Args: {cut, frame, ...}.",
    ),
    (
        "end_card_add_app",
        "Adds an end card. Args: {cut, loop_layout?}.",
    ),
    (
        "overlay_template_save_app",
        "Saves an overlay as a template. Args: {overlay, name}.",
    ),
    (
        "overlay_template_insert_at",
        "Inserts an overlay template. Args: {cut, frame, name, measurement?}.",
    ),
    (
        "style_items_view",
        "Saved style items (plates, pills, callouts).",
    ),
    (
        "style_item_save_mine",
        "Saves a style item as mine. Args: {from, name}.",
    ),
    (
        "style_item_forget_mine",
        "Forgets one of my style items. Args: {kind, name}.",
    ),
    (
        "style_item_apply",
        "Applies a style item. Args: {kind, name, to, mine?}.",
    ),
    ("plate_save", "Saves a colour plate. Args: {name, stops}."),
    ("plates_view", "The colour plates. Args: {cut}."),
    ("brand_kit_view", "The project's brand kit."),
    (
        "brand_kit_save",
        "Saves the brand kit (colours, fonts, ...). Args: the kit.",
    ),
    (
        "brand_kit_use_mine",
        "Uses my saved brand kit in this project.",
    ),
    (
        "brand_logo_set",
        "Sets the brand logo from a file. Args: {path, name?}.",
    ),
    ("broll_suggest", "Suggests b-roll for the cut. Args: {cut}."),
    ("broll_add", "Adds a suggested b-roll hit. Args: {hit, as}."),
    (
        "polish_view",
        "Polish suggestions (emphasis, punch-ins, callouts). Args: {cut}.",
    ),
    (
        "polish_apply",
        "Applies polish suggestions. Args: {cut, ...}.",
    ),
    // sound
    (
        "sound_view",
        "The cut's sound: loudness, gain spans, hidden camera. Args: {cut}.",
    ),
    (
        "loudness_set",
        "Sets the cut's loudness target. Args: {cut, lufs}.",
    ),
    (
        "gain_span_add",
        "Adds a gain span. Args: {cut, from, to, gain_db}.",
    ),
    ("gain_span_delete", "Deletes a gain span. Args: {segment}."),
    (
        "gain_segment_edit",
        "Edits a gain segment. Args: {cut, segment, from, to, gain_db}.",
    ),
    (
        "part_gain_set",
        "Sets a part's gain. Args: {part, gain_db}.",
    ),
    (
        "cam_hide_add",
        "Hides the camera from..to. Args: {cut, from, to}.",
    ),
    (
        "cam_hide_delete",
        "Shows the camera again. Args: {cut, hide}.",
    ),
    (
        "no_captions_add",
        "No captions from..to. Args: {cut, from, to}.",
    ),
    ("no_captions_delete", "Captions again. Args: {cut, id}."),
    // openings, end cards, comments, thumbnails
    ("openings_view", "The cut's three openings. Args: {cut}."),
    (
        "opening_preview",
        "Plans an opening's preview. Args: {cut, opening}.",
    ),
    (
        "opening_add_words",
        "Adds an opening from words. Args: {cut, from, to, label?, hook_text?}.",
    ),
    (
        "opening_export",
        "Exports the cut with one opening. Args: {cut, opening}.",
    ),
    (
        "end_card_longform_view",
        "The end card that points to the long-form. Args: {cut}.",
    ),
    (
        "end_card_longform_add_app",
        "Adds the long-form end card. Args: {cut}.",
    ),
    (
        "end_card_longform_remove",
        "Removes the long-form end card. Args: {cut}.",
    ),
    ("comments_view", "The cut's comments. Args: {cut}."),
    (
        "comment_add_at",
        "Adds a comment. Args: {cut, text, box?, reply_to?}.",
    ),
    (
        "golden_view",
        "The cut's golden (approved) versions. Args: {cut}.",
    ),
    (
        "golden_compare",
        "Compares the cut with its golden version. Args: {cut}.",
    ),
    ("thumbs_view", "The cut's thumbnails. Args: {cut}."),
    (
        "thumb_add_at",
        "Adds a thumbnail at a frame. Args: {cut, pick, title?, style?}.",
    ),
    ("thumb_render", "Renders a thumbnail. Args: {thumbnail}."),
    // exports
    (
        "export_start",
        "Exports a cut (a job). Args: {cut, preset?, draft_mattes?}.",
    ),
    ("export_cancel", "Cancels an export. Args: {job}."),
    (
        "export_verify",
        "Re-verifies an exported file against its plan. Args: {export}.",
    ),
    ("exports_list", "The project's exports."),
    (
        "export_framings",
        "The cut's 9:16 and 16:9 framings. Args: {cut}.",
    ),
    (
        "framing_make",
        "Makes the other framing. Args: {cut, aspect}.",
    ),
    (
        "export_both",
        "Exports 9:16 and 16:9 together. Args: {cut, make_missing?}.",
    ),
];

/// The holder's way into the app's IPC.
pub struct Host(pub Ctx);

impl gmclip_mcp::holder::AppHost for Host {
    fn commands(&self) -> &'static [(&'static str, &'static str)] {
        COMMANDS
    }

    fn job(&self, id: &str, wait: Duration) -> Option<AppJob> {
        self.job_of(id, wait)
    }

    fn call(
        &self,
        project: Option<&str>,
        command: &str,
        args: &Value,
        caller: &Caller,
    ) -> Result<Value, CmdError> {
        if !COMMANDS.iter().any(|(n, _)| *n == command) {
            return Err(CmdError::NotFound);
        }
        let req = CallRequestWire {
            command: command.to_owned(),
            args: args.clone(),
            project: project.map(str::to_owned),
            request_id: None,
            gesture: None,
        };
        // its registry calls are the agent's, in its run (decision 0050)
        crate::core::acting_as(caller.clone(), || ipc::dispatch_inner(&self.0, &req)).map_err(|e| {
            // a core error comes back as itself; the app's own as the nearest kind
            if let Some(d) = &e.detail
                && let Ok(c) = serde_json::from_value::<CmdError>(d.clone())
            {
                return c;
            }
            match e.code.as_str() {
                "NotFound" => CmdError::NotFound,
                "Unauthorized" => CmdError::Unauthorized { scope: e.message },
                "Busy" => CmdError::Busy { job: e.message },
                "Invalid" => CmdError::invalid("/args", &e.message),
                _ => CmdError::Unsupported {
                    platform: "app".into(),
                    reason: format!("{}: {}", e.code, e.message),
                },
            }
        })
    }
}

impl Host {
    /// An export job of the app's, as `job_wait` and `verify` see it.
    fn job_of(&self, id: &str, wait: Duration) -> Option<AppJob> {
        let (v, done) = self.0.jobs.wait(id, wait)?;
        let result = match v.state.as_str() {
            "done" => Some(
                json!({"out": done.as_ref().map(|d| d.1.display().to_string()), "export": v.export, "frames": v.frames}),
            ),
            "failed" | "cancelled" => Some(json!({"error": v.error})),
            _ => None,
        };
        Some(AppJob {
            progress: (v.state == "running")
                .then(|| json!({"frames": v.done, "of": v.frames, "phase": v.phase})),
            state: v.state,
            project: v.project,
            cut: v.cut,
            result,
            plan: done.as_ref().map(|d| d.0.clone()),
            out: done.map(|d| d.1),
        })
    }
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("agentapp::Host").finish_non_exhaustive()
    }
}
