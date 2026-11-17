//! A project of several videos (AJ 2026-10-15: "i should be able to load multiple video
//! files into one project"; SPEC §4.3 `Project.sources[]`, §5.1 import).
//!
//! - `source_list`: every video with its probe summary, the sync group it is in, the
//!   stream edit (the main cut, holding every video: decision 0036) when it is on it,
//!   and the cuts that use it.
//! - `source_remove`: refused while any cut other than the stream edit plays it (the
//!   refusal lists them); otherwise the source goes with its stretch of the stream edit
//!   (parts, topics, fluff), its regions and its moments, and a same-recording group it
//!   was in is dissolved (the other member stays a video of its own), as one undoable
//!   step.
//! - `source_relink`: points a source whose file moved at the file's new place (the
//!   probe is the importer's; a path, so agents never reach it, R-FS-1).
//! - `source_import`: the agent way in. Answered by the holder, which checks the file is
//!   inside the agent read roots before the app probes and adds it.

use gmclip_macros::command;
use gmclip_model::entities::*;
use gmclip_model::{EntityId, Id, Op};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::longform::{MapSource, cuts_using, map_sources, media_of_source};
use crate::command::{Command, Cx, model_err};

// ---------------------------------------------------------------------------------------
// source_list
// ---------------------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceListArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SourceListed {
    pub sources: Vec<MapSource>,
    /// Each recorded screen's cursor (decision 0045), by source id: how it is drawn.
    pub cursors: std::collections::BTreeMap<String, SourceCursor>,
}

#[derive(Debug)]
pub struct SourceList;

#[command(
    name = "source_list",
    scope = "read",
    undo = "read_only",
    doc = "List the project's videos (sources): file name, length, picture size and rate, audio tracks, the same-recording group each is in, its stream edit, and the cuts that play it. Parts of any cut may come from any of them (part_add with media {source} or {group}).",
    example = r#"{}"#
)]
impl Command for SourceList {
    type Args = SourceListArgs;
    type Output = SourceListed;
    fn run(_: SourceListArgs, cx: &mut Cx<'_>) -> Result<SourceListed, CmdError> {
        let doc = cx.tx.peek();
        Ok(SourceListed {
            sources: map_sources(doc),
            cursors: doc
                .iter::<Source>()
                .filter_map(|s| Some((s.id.0.to_string(), s.cursor.clone()?)))
                .collect(),
        })
    }
}

// ---------------------------------------------------------------------------------------
// source_remove
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceRemoveArgs {
    pub source: Id<Source>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SourceRemoved {
    /// The stream edit it was taken out of (removed too when it was its last video).
    pub stream_edits: Vec<String>,
    pub moments: usize,
    /// The same-recording group dissolved, if it was in one.
    pub group: Option<String>,
}

#[derive(Debug)]
pub struct SourceRemove;

/// Deletes `e` and everything under it, deepest first (one transaction).
pub(crate) fn delete_subtree(cx: &mut Cx<'_>, e: gmclip_model::EntityId) -> Result<(), CmdError> {
    let doc = cx.tx.peek();
    let ops: Vec<Op> = doc
        .subtree(e)
        .into_iter()
        .filter_map(|x| doc.encode(x).map(|value| Op::Delete { e: x, value }))
        .collect();
    cx.tx.staging().apply_ops(&ops).map_err(model_err)
}

#[command(
    name = "source_remove",
    scope = "edit",
    undo = "undoable",
    doc = "Remove a video from the project: its stretch of the stream edit (parts, topics, strikes; the stream edit itself when it was its last video) and its moments, as one undoable step. Refused while any long-form or short plays it: the refusal names them. A same-recording group it was in is dissolved; the other member stays.",
    example = r#"{"source": "01J00000000000000000000002"}"#
)]
impl Command for SourceRemove {
    type Args = SourceRemoveArgs;
    type Output = SourceRemoved;
    fn run(a: SourceRemoveArgs, cx: &mut Cx<'_>) -> Result<SourceRemoved, CmdError> {
        cx.tx.get(a.source)?;
        let (media, group) = media_of_source(cx.tx.peek(), a.source);
        let mut uses = vec![MediaRef::Source(a.source)];
        if media != MediaRef::Source(a.source) {
            uses.push(media);
        }
        // the reads the refusal rests on (recorded, so a concurrent part_add conflicts)
        let cuts: Vec<Id<Cut>> = cx.tx.peek().iter::<Cut>().map(|c| c.id).collect();
        for c in &cuts {
            for p in cx.tx.children::<Part>(c.entity()) {
                cx.tx.get(p)?;
            }
        }
        let users = cuts_using(cx.tx.peek(), &uses);
        if !users.is_empty() {
            let doc = cx.tx.peek();
            let mut names: Vec<String> = users
                .iter()
                .take(5)
                .filter_map(|c| doc.get(*c))
                .map(|c| format!("«{}»", c.title.chars().take(60).collect::<String>()))
                .collect();
            if users.len() > 5 {
                names.push(format!("and {} more", users.len() - 5));
            }
            return Err(CmdError::invalid(
                "/source",
                format!(
                    "{} cut{} use{} this video: {}",
                    users.len(),
                    if users.len() == 1 { "" } else { "s" },
                    if users.len() == 1 { "s" } else { "" },
                    names.join(", ")
                )
                .as_str(),
            ));
        }
        let mut out = SourceRemoved {
            stream_edits: Vec::new(),
            moments: 0,
            group: None,
        };
        // its stretch of the stream edit (decision 0036): the video's parts, topics and
        // fluff proposals go; the stream edit stays with the other videos (and goes when
        // this was its last)
        if let Some(c) = super::topic::stream_edit(cx.tx.peek()) {
            cx.tx.get(c)?;
            let doc = cx.tx.peek();
            let mine: Vec<EntityId> = doc
                .children::<Part>(c.entity())
                .filter(|p| doc.get(*p).is_some_and(|p| uses.contains(&p.media)))
                .map(|p| p.entity())
                .chain(
                    doc.children::<Topic>(c.entity())
                        .filter(|t| doc.get(*t).is_some_and(|t| uses.contains(&t.media)))
                        .map(|t| t.entity()),
                )
                .chain(
                    doc.children::<FluffProposal>(c.entity())
                        .filter(|f| doc.get(*f).is_some_and(|f| uses.contains(&f.media)))
                        .map(|f| f.entity()),
                )
                .collect();
            let others = doc.children::<Part>(c.entity()).count()
                > mine
                    .iter()
                    .filter(|e| e.kind == <Part as gmclip_model::Entity>::KIND)
                    .count();
            if others {
                for e in mine {
                    delete_subtree(cx, e)?;
                }
            } else {
                // a long-form's lineage may name the stream edit or its topics: those are
                // only links (it plays none of this video, or it would have refused)
                delete_subtree(cx, c.entity())?;
            }
            out.stream_edits.push(c.0.to_string());
        }
        let moments: Vec<Id<Moment>> = cx
            .tx
            .peek()
            .iter::<Moment>()
            .filter(|m| uses.contains(&m.media))
            .map(|m| m.id)
            .collect();
        for m in moments {
            cx.tx.get(m)?;
            delete_subtree(cx, m.entity())?;
            out.moments += 1;
        }
        if let Some((g, _)) = group {
            cx.tx.get(g)?;
            delete_subtree(cx, g.entity())?;
            out.group = Some(g.0.to_string());
        }
        delete_subtree(cx, a.source.entity())?;
        Ok(out)
    }
}

// ---------------------------------------------------------------------------------------
// source_relink
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceRelinkArgs {
    pub source: Id<Source>,
    /// The file's new place (the importer checked and probed it).
    pub path: String,
    pub fingerprint: String,
    #[serde(default)]
    pub probe: Option<Probe>,
}

#[derive(Debug)]
pub struct SourceRelink;

#[command(
    name = "source_relink",
    scope = "edit",
    undo = "undoable",
    doc = "Point a video whose file moved at its new place (the importer probes the file first). Every cut keeps its parts.",
    example = r#"{"source": "01J00000000000000000000002", "path": "/Volumes/Archive/2026-10-04 stream.mkv", "fingerprint": "sha256:ab12"}"#
)]
impl Command for SourceRelink {
    type Args = SourceRelinkArgs;
    type Output = super::Done;
    fn run(a: SourceRelinkArgs, cx: &mut Cx<'_>) -> Result<super::Done, CmdError> {
        if a.path.is_empty() || a.path.len() > 4096 || a.fingerprint.len() > 256 {
            return Err(CmdError::invalid(
                "/path",
                "1..=4096 bytes; fingerprint ≤ 256",
            ));
        }
        let old = cx.tx.get(a.source)?.path.clone();
        cx.tx.update(a.source, |s| {
            s.path = a.path;
            s.fingerprint = a.fingerprint;
            if let Some(p) = a.probe {
                s.probe = p;
            }
            s.relink_hint = Some(old.chars().take(4096).collect());
        })?;
        Ok(super::Done {})
    }
}

// ---------------------------------------------------------------------------------------
// source_import (holder)
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceImportArgs {
    /// A video file (MP4, MOV, M4V, MKV or WebM) inside one of the agent read roots.
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SourceImported {
    pub source: String,
    /// The stream edit, the main cut it was appended to.
    pub stream: Option<String>,
    pub duration_ms: i64,
}

/// Adds a video to the project (answered by the holder: read roots, probe, analysis).
#[derive(Debug)]
pub struct SourceImport;

#[command(
    name = "source_import",
    scope = "edit",
    undo = "undoable",
    doc = "Add a video file to this project as another source (only files inside the agent read roots): it is probed, appended whole to the end of the project's main cut (its stream edit), and transcribed in the background. Then add parts from it to any cut with part_add {media: {source}}.",
    example = r#"{"path": "/Users/aj/Movies/2026-10-04 second camera.mp4"}"#
)]
impl Command for SourceImport {
    type Args = SourceImportArgs;
    type Output = SourceImported;
    fn run(_: SourceImportArgs, _: &mut Cx<'_>) -> Result<SourceImported, CmdError> {
        Err(CmdError::Unsupported {
            platform: "actor".into(),
            reason: "answered by the holder".into(),
        })
    }
}

// ---------------------------------------------------------------------------------------
// source_stream_layout_set
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceStreamLayoutSetArgs {
    pub source: Id<Source>,
    /// The layout's name (a profile preset's, as the app matched it).
    pub name: String,
    /// The canvas its boxes were measured on: the source picture's size.
    pub canvas: Canvas,
    /// Its boxes, normalised to the canvas (1–8; camera, screen, chat, alerts…).
    pub regions: Vec<StreamRegion>,
}

/// Gives a source a stream layout (SPEC §5.7.1 step 3: seeding takes the face from the
/// source's camera box): a profile preset matched to the source, or one typed by hand.
#[derive(Debug)]
pub struct SourceStreamLayoutSet;

#[command(
    name = "source_stream_layout_set",
    scope = "edit",
    undo = "undoable",
    doc = "Give a video a stream layout: where its camera, screen, chat and alerts boxes are (normalised to its picture), replacing any it had. New 9:16 cuts seed the face from the camera box.",
    example = r#"{"source": "01J00000000000000000000002", "name": "Main scene", "canvas": {"w": 2560, "h": 1440}, "regions": [{"role": "cam", "rect": {"x": 0.8047, "y": 0.4403, "w": 0.1938, "h": 0.2361}}]}"#
)]
impl Command for SourceStreamLayoutSet {
    type Args = SourceStreamLayoutSetArgs;
    type Output = super::Done;
    fn run(a: SourceStreamLayoutSetArgs, cx: &mut Cx<'_>) -> Result<super::Done, CmdError> {
        let name = a.name.trim().to_owned();
        if name.is_empty() || name.chars().count() > 200 {
            return Err(CmdError::invalid("/name", "1..=200 characters"));
        }
        if a.canvas.w == 0 || a.canvas.h == 0 || a.canvas.w > 16384 || a.canvas.h > 16384 {
            return Err(CmdError::invalid("/canvas", "1..=16384 pixels a side"));
        }
        if !(1..=8).contains(&a.regions.len()) {
            return Err(CmdError::invalid("/regions", "1..=8 boxes"));
        }
        let e = 1e-9;
        if let Some(i) = a.regions.iter().position(|r| {
            !r.rect.is_valid() || r.rect.x + r.rect.w > 1.0 + e || r.rect.y + r.rect.h > 1.0 + e
        }) {
            return Err(CmdError::invalid(
                &format!("/regions/{i}"),
                "a box inside the canvas (normalised, 0..1)",
            ));
        }
        let src = cx.tx.get(a.source)?.clone();
        // the picture's size, when the probe knows it, must be the layout's canvas
        if let Some(v) = src.probe.tracks.iter().find(|t| t.kind == TrackKind::Video)
            && let (Some(w), Some(h)) = (v.width, v.height)
            && (w, h) != (a.canvas.w, a.canvas.h)
        {
            return Err(CmdError::invalid(
                "/canvas",
                &format!("the picture is {w}x{h}"),
            ));
        }
        let old = src.stream_layout;
        let id: Id<StreamLayout> = cx.tx.mint();
        cx.tx.create(StreamLayout {
            id,
            name,
            canvas_size: a.canvas,
            fingerprint: src.fingerprint,
            regions: a.regions,
            audio_roles: None,
            default_template: None,
        })?;
        cx.tx.update(a.source, |s| s.stream_layout = Some(id))?;
        // the one it replaces goes when no other source uses it
        if let Some(o) = old
            && !cx
                .tx
                .peek()
                .iter::<Source>()
                .any(|s| s.stream_layout == Some(o))
        {
            cx.tx.delete(o)?;
        }
        Ok(super::Done {})
    }
}

// ---------------------------------------------------------------------------------------
// source_cursor_set
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceCursorSetArgs {
    pub source: Id<Source>,
    /// The recorded input track (an `application/vnd.gmclip.input+json` asset). Required
    /// the first time; absent afterwards keeps it.
    #[serde(default)]
    pub track: Option<Id<Asset>>,
    /// The cursor images (PNG assets), by the track's shape index; with `track`.
    #[serde(default)]
    pub shapes: Option<Vec<Id<Asset>>>,
    /// Draw the cursor at all.
    #[serde(default)]
    pub visible: Option<bool>,
    /// Size × the recorded cursor's, 0.25–8.
    #[serde(default)]
    pub scale: Option<f64>,
    /// Smoothing window, ms each side, 0–500.
    #[serde(default)]
    pub smooth_ms: Option<u32>,
    /// Fade out after this long without moving, ms (0 = never), up to 60 000.
    #[serde(default)]
    pub hide_idle_ms: Option<u32>,
    /// A ring where each click lands (older form of `click`: true = `ring`).
    #[serde(default)]
    pub clicks: Option<bool>,
    /// How a click shows: `none`, `ring` or `ripple` (decision 0046).
    #[serde(default)]
    pub click: Option<CursorClick>,
    /// The cursor's feel, 0–1: 0 = the raw path, 1 = a spring with a 250 ms time constant.
    #[serde(default)]
    pub smoothness: Option<f64>,
    /// The cursor dips to 80 % for 120 ms on each click.
    #[serde(default)]
    pub click_shrink: Option<bool>,
    /// Slow moves smaller than this many px are held still, 0–20 (0 = off).
    #[serde(default)]
    pub shake_px: Option<u32>,
    /// The last N ms of each part glide back to its first position, 0–10000 (0 = off).
    #[serde(default)]
    pub loop_ms: Option<u32>,
    /// Motion blur on frames where the cursor moves (off by default).
    #[serde(default)]
    pub motion_blur: Option<bool>,
    /// Remove the cursor from the source (the recording then shows none).
    #[serde(default)]
    pub clear: bool,
}

/// A recorded screen file's cursor (decision 0045): the import attaches the input track
/// and cursor images; the inspector's Cursor controls change how it is drawn.
#[derive(Debug)]
pub struct SourceCursorSet;

#[command(
    name = "source_cursor_set",
    scope = "edit",
    undo = "undoable",
    doc = "Attach or change a recorded screen video's cursor (decision 0045): the recording hides the cursor and gmclip draws it from the logged input track. Set `track` + `shapes` once (assets), then `visible`, `scale` (× the recorded size), `smooth_ms`, `hide_idle_ms` and `clicks` change how it is drawn, and the feel (decision 0046): `smoothness` 0–1, `click` none|ring|ripple, `click_shrink`, `shake_px` 0–20, `loop_ms` 0–10000, `motion_blur`; `clear` removes it.",
    example = r#"{"source": "01J00000000000000000000002", "scale": 1.5, "hide_idle_ms": 2000}"#
)]
impl Command for SourceCursorSet {
    type Args = SourceCursorSetArgs;
    type Output = super::Done;
    fn run(a: SourceCursorSetArgs, cx: &mut Cx<'_>) -> Result<super::Done, CmdError> {
        let existing = cx.tx.get(a.source)?.cursor.clone();
        if a.clear {
            cx.tx.update(a.source, |s| s.cursor = None)?;
            return Ok(super::Done {});
        }
        if let Some(t) = a.track {
            let asset = cx.tx.get(t)?;
            if asset.media_type != gmclip_model::rec::INPUT_TRACK_MEDIA_TYPE {
                return Err(CmdError::invalid("/track", "an input track asset"));
            }
        }
        for (i, sh) in a.shapes.iter().flatten().enumerate() {
            if cx.tx.get(*sh)?.media_type != "image/png" {
                return Err(CmdError::invalid(&format!("/shapes/{i}"), "a PNG asset"));
            }
        }
        if a.shapes.as_ref().is_some_and(|s| s.len() > 512) {
            return Err(CmdError::invalid("/shapes", "at most 512 cursor images"));
        }
        if a.scale.is_some_and(|s| !(0.25..=8.0).contains(&s)) {
            return Err(CmdError::invalid("/scale", "0.25–8"));
        }
        if a.smooth_ms.is_some_and(|s| s > 500) {
            return Err(CmdError::invalid("/smooth_ms", "0–500"));
        }
        if a.hide_idle_ms.is_some_and(|s| s > 60_000) {
            return Err(CmdError::invalid("/hide_idle_ms", "0–60000"));
        }
        if a.smoothness.is_some_and(|s| !(0.0..=1.0).contains(&s)) {
            return Err(CmdError::invalid("/smoothness", "0–1"));
        }
        if a.shake_px.is_some_and(|s| s > 20) {
            return Err(CmdError::invalid("/shake_px", "0–20"));
        }
        if a.loop_ms.is_some_and(|s| s > 10_000) {
            return Err(CmdError::invalid("/loop_ms", "0–10000"));
        }
        let mut c = match (existing, a.track) {
            (_, Some(t)) => SourceCursor::new(t, a.shapes.clone().unwrap_or_default()),
            (Some(c), None) => c,
            (None, None) => {
                return Err(CmdError::invalid(
                    "/track",
                    "this video has no recorded cursor yet",
                ));
            }
        };
        if a.track.is_none()
            && let Some(sh) = a.shapes
        {
            c.shapes = sh;
        }
        if let Some(v) = a.visible {
            c.visible = v;
        }
        if let Some(v) = a.scale {
            c.scale = v;
        }
        if let Some(v) = a.smooth_ms {
            c.smooth_ms = v;
        }
        if let Some(v) = a.hide_idle_ms {
            c.hide_idle_ms = v;
        }
        if let Some(v) = a.clicks {
            c.clicks = v;
            c.click = None;
        }
        if let Some(v) = a.click {
            // `clicks` stays true for any shown click, for readers that know only it
            c.clicks = v != CursorClick::None;
            c.click = Some(v);
        }
        if let Some(v) = a.smoothness {
            c.smoothness = v;
        }
        if let Some(v) = a.click_shrink {
            c.click_shrink = v;
        }
        if let Some(v) = a.shake_px {
            c.shake_px = v;
        }
        if let Some(v) = a.loop_ms {
            c.loop_ms = v;
        }
        if let Some(v) = a.motion_blur {
            c.motion_blur = v;
        }
        cx.tx.update(a.source, |s| s.cursor = Some(c))?;
        Ok(super::Done {})
    }
}
