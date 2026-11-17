//! Moments and review mode (SPEC §5.2, backlog U.05).
//!
//! - `moments_list`: the project's moments with what the panel shows (score, type,
//!   excerpt, state, origin label) and their source span in seconds.
//! - Review mode plays a moment from its excerpt's first word to its last word + 1 s
//!   (the span when the excerpt cannot be placed), looping. The viewer gets a one-part
//!   RenderPlan of the stream's picture over that span (full frame, scaled to ≤ 1280 px
//!   wide), built here from the sync group's picture and audio members.
//! - Transcripts come from the project's analysis blobs (`words`, `lines` per sync group
//!   or source and ASR revision); line anchors resolve through the line table.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gmclip_model::anchor::Anchor;
use gmclip_model::entities::{
    MediaRef, MemberRole, Moment, MomentOrigin, Source, SyncGroup, SyncMember, SyncSegment,
};
use gmclip_model::{Doc, Flicks, Id};
use gmclip_plan::Plan;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::core::OpenProject;
use crate::error::{AppError, AppResult};
use crate::planner::FLICKS;

/// One transcript word (flicks).
#[derive(Clone, Debug, Deserialize)]
pub struct Word {
    pub text: String,
    pub start: i64,
    pub end: i64,
}

/// One transcript line and its word range `[w0, w1)`.
#[derive(Clone, Debug, Deserialize)]
pub struct Line {
    pub start: i64,
    pub end: i64,
    #[serde(default)]
    pub words: [u32; 2],
    /// The line as written (an imported transcript's), when the blob has it.
    #[serde(default)]
    pub text: String,
}

/// A transcript (one ASR revision of one sync group or source).
#[derive(Clone, Debug, Default)]
pub struct Transcript {
    pub words: Vec<Word>,
    pub lines: Vec<Line>,
}

#[derive(Deserialize)]
struct WordsBody {
    words: Vec<Word>,
}

#[derive(Deserialize)]
struct LinesBody {
    lines: Vec<Line>,
}

/// Transcripts per `(project, owner, revision)`, parsed once.
#[derive(Debug, Default)]
pub struct Transcripts {
    cache: Mutex<HashMap<(String, String, String), Arc<Transcript>>>,
}

impl Transcripts {
    /// The transcript stored for `owner` at `revision`, or `None`.
    pub fn get(&self, p: &OpenProject, owner: &str, revision: &str) -> Option<Arc<Transcript>> {
        let key = (p.info.id.clone(), owner.to_owned(), revision.to_owned());
        if let Some(t) = self.cache.lock().ok()?.get(&key) {
            return Some(t.clone());
        }
        let words = p.handle.analysis(owner, "words", revision)?;
        let words: WordsBody = serde_json::from_slice(&words).ok()?;
        let lines = p
            .handle
            .analysis(owner, "lines", revision)
            .and_then(|b| serde_json::from_slice::<LinesBody>(&b).ok())
            .map(|l| l.lines)
            .unwrap_or_default();
        let t = Arc::new(Transcript {
            words: words.words,
            lines,
        });
        self.cache.lock().ok()?.insert(key, t.clone());
        Some(t)
    }

    /// The newest revision stored for `owner` (imports write one).
    /// The transcript the project uses for `owner`: its active ASR revision (revisions
    /// are hashes, so "the largest" is arbitrary and can be a partial first window).
    pub fn latest(&self, p: &OpenProject, owner: &str) -> Option<Arc<Transcript>> {
        self.latest_rev(p, owner).map(|x| x.1)
    }

    /// [`Self::latest`] and its ASR revision.
    pub fn latest_rev(&self, p: &OpenProject, owner: &str) -> Option<(String, Arc<Transcript>)> {
        let stored: Vec<String> = p
            .handle
            .analysis_keys()
            .into_iter()
            .filter(|(o, k, _)| o == owner && k == "words")
            .map(|(_, _, r)| r)
            .collect();
        let active = p
            .handle
            .snapshot()
            .doc
            .iter::<gmclip_model::entities::Project>()
            .next()
            .and_then(|pr| pr.active_asr.get(owner).cloned())
            .filter(|a| stored.contains(a));
        let rev = active.or_else(|| stored.into_iter().max())?;
        let t = self.get(p, owner, &rev)?;
        Some((rev, t))
    }
}

pub fn owner_of(m: &Moment) -> String {
    match m.media {
        MediaRef::Group(g) => g.0.to_string(),
        MediaRef::Source(s) => s.0.to_string(),
    }
}

/// An anchor's source time, with line anchors through the transcript.
fn anchor_time(doc: &Doc, a: &Anchor, t: Option<&Transcript>) -> Option<Flicks> {
    match a {
        Anchor::LineStart(l) => t?.lines.get(l.line as usize).map(|l| Flicks(l.start)),
        Anchor::LineEnd(l) => t?.lines.get(l.line as usize).map(|l| Flicks(l.end)),
        Anchor::Raw(r) => gmclip_model::clock::time_to_src(doc, &r.time),
        other => gmclip_model::clock::anchor_src(doc, other),
    }
}

fn norm(s: &str) -> String {
    s.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// The moment's span, and the excerpt's first word → last word + 1 s inside it.
pub fn spans(
    doc: &Doc,
    m: &Moment,
    t: Option<&Transcript>,
) -> Option<((Flicks, Flicks), (Flicks, Flicks))> {
    let a = anchor_time(doc, &m.span.start, t)?;
    let b = anchor_time(doc, &m.span.end, t)?;
    if b <= a {
        return None;
    }
    let span = (a, b);
    let review = (|| {
        let t = t?;
        let want: Vec<String> = m
            .excerpt
            .text
            .split_whitespace()
            .map(norm)
            .filter(|w| !w.is_empty())
            .collect();
        if want.is_empty() {
            return None;
        }
        // Words overlapping the span (2 s grace: imported spans are line-edged).
        let grace = 2 * FLICKS;
        let inside: Vec<&Word> = t
            .words
            .iter()
            .filter(|w| w.end > a.0 - grace && w.start < b.0 + grace)
            .collect();
        let texts: Vec<String> = inside.iter().map(|w| norm(&w.text)).collect();
        let i = texts
            .windows(want.len())
            .position(|win| win == want.as_slice())?;
        let first = inside.get(i)?;
        let last = inside.get(i + want.len() - 1)?;
        Some((Flicks(first.start), Flicks(last.end + FLICKS)))
    })()
    .unwrap_or((a, Flicks(b.0 + FLICKS)));
    Some((span, review))
}

fn agent_label(m: &Moment) -> String {
    format!(
        "Found by your agent{} — recall not measured",
        m.harness_label
            .as_deref()
            .map(|h| format!(" ({h})"))
            .unwrap_or_default()
    )
}

fn origin_label(m: &Moment) -> String {
    match m.origin {
        // K.06: a laugh or a big reaction in the recording's sound with its words
        MomentOrigin::Local if m.r#type == "reaction" => {
            "Found by sound and transcript: a laugh or reaction".into()
        }
        MomentOrigin::Local => "Found by sound and transcript".into(),
        MomentOrigin::Agent => agent_label(m),
        // A.01: gmclip's own full sweep, run by the user's agent
        MomentOrigin::Sweep
            if m.pass_ref
                .as_deref()
                .is_some_and(|r| r.starts_with("full_sweep@")) =>
        {
            agent_label(m)
        }
        MomentOrigin::Sweep => "Found by a sweep".into(),
        MomentOrigin::Scout => "Found by the visual scout".into(),
        MomentOrigin::Variant => "Alternate opening".into(),
        MomentOrigin::Manual => "Added by you".into(),
        MomentOrigin::Chat => "Found in your chat log".into(),
    }
}

fn secs(f: Flicks) -> f64 {
    (f.0 as f64 / FLICKS as f64 * 1000.0).round() / 1000.0
}

/// The moments panel's rows: core's `moments_list` projection (A2.03: filters, sort,
/// the span in source seconds) plus what only the app knows — the
/// video's name and order, the origin label, and review mode's excerpt span.
pub fn moments_list(p: &OpenProject, ts: &Transcripts) -> Value {
    let doc = p.handle.snapshot().doc.clone();
    // the project's videos (each moment is on one): listed per video and all together
    let videos: Vec<(String, String)> = crate::analysis::owners(&doc)
        .into_iter()
        .map(|(o, s)| {
            let f = crate::file_part(&s.path);
            (o, f.rsplit_once('.').map_or(f, |(n, _)| n).to_owned())
        })
        .collect();
    let listed = p
        .handle
        .call(gmclip_core::CallRequest {
            command: "moments_list".into(),
            args: json!({}),
            base: None,
            request_id: None,
            caller: gmclip_core::Caller::human(),
        })
        .map(|ok| ok.output)
        .unwrap_or(Value::Null);
    let mut rows = Vec::new();
    for r in listed["moments"].as_array().into_iter().flatten() {
        let Some(m) = r["id"]
            .as_str()
            .and_then(|id| crate::planner::ulid_id::<Moment>(id).ok())
            .and_then(|id| doc.get(id))
        else {
            continue;
        };
        let owner = owner_of(m);
        let t = ts.latest(p, &owner);
        let sp = spans(&doc, m, t.as_deref());
        let vi = videos.iter().position(|(o, _)| *o == owner);
        let mut row = r.clone();
        row["video"] = json!(vi.map(|i| videos[i].1.clone()));
        row["video_order"] = json!(vi);
        row["origin_label"] = json!(origin_label(m));
        // the panel's span is the transcript's (line anchors via the app's cache)
        row["start_s"] = json!(sp.map(|s| secs(s.0.0)).or(r["start_s"].as_f64()));
        row["end_s"] = json!(sp.map(|s| secs(s.0.1)).or(r["end_s"].as_f64()));
        row["review_s"] = json!(sp.map(|s| [secs(s.1.0), secs(s.1.1)]));
        rows.push(row);
    }
    rows.sort_by(|a, b| {
        let v = |r: &Value| r["video_order"].as_u64().unwrap_or(u64::MAX);
        v(a).cmp(&v(b)).then(
            a["start_s"]
                .as_f64()
                .unwrap_or(0.0)
                .total_cmp(&b["start_s"].as_f64().unwrap_or(0.0)),
        )
    });
    let videos: Vec<Value> = videos
        .into_iter()
        .map(|(o, n)| {
            let d = density(&doc, p, ts, &o);
            json!({"owner": o, "name": n, "density": d})
        })
        .collect();
    json!({"moments": rows, "videos": videos, "counts": listed["counts"],
           "transcript": !rows.is_empty() && rows.iter().any(|r| !r["review_s"].is_null())})
}

/// The density gate for one video (D.04, `gmclip_core::density`): its moments per
/// active-talk minute against the 0.40 floor, and its largest uncarded stretches.
/// `null` until the video has a transcript.
pub fn density(doc: &Doc, p: &OpenProject, ts: &Transcripts, owner: &str) -> Value {
    let Some(t) = ts.latest(p, owner) else {
        return Value::Null;
    };
    let lines: Vec<gmclip_core::density::Line> = t
        .lines
        .iter()
        .map(|l| gmclip_core::density::Line {
            start: secs(Flicks(l.start)),
            end: secs(Flicks(l.end)),
            words: if l.text.is_empty() {
                l.words[1].saturating_sub(l.words[0]) as usize
            } else {
                l.text.split_whitespace().count()
            },
        })
        .collect();
    let spans: Vec<(f64, f64)> = doc
        .iter::<Moment>()
        .filter(|m| owner_of(m) == owner)
        .filter_map(|m| spans(doc, m, Some(&t)))
        .map(|(s, _)| (secs(s.0), secs(s.1)))
        .collect();
    gmclip_core::density::density(&lines, &spans).map_or(Value::Null, |d| {
        serde_json::to_value(d).unwrap_or(Value::Null)
    })
}

/// The member's time for a group time (SPEC §4.3: member = group − offset).
fn member_time(doc: &Doc, m: Id<SyncMember>, t: i64) -> i64 {
    let segs: Vec<&SyncSegment> = doc
        .children::<SyncSegment>(m.entity())
        .filter_map(|s| doc.get(s))
        .collect();
    let off = segs
        .iter()
        .find(|s| s.src_range.start.0 <= t && t < s.src_range.end.0)
        .or(segs.first())
        .map_or(0, |s| s.offset.0);
    t - off
}

/// `(picture source, its time, audio source, its time)` for a moment's media at `t`.
pub(crate) fn media_at(
    doc: &Doc,
    media: MediaRef,
    t: i64,
) -> Option<(Id<Source>, i64, Id<Source>, i64)> {
    match media {
        MediaRef::Source(s) => Some((s, t, s, t)),
        MediaRef::Group(g) => {
            let group: &SyncGroup = doc.get(g)?;
            let members: Vec<&SyncMember> = doc
                .children::<SyncMember>(g.entity())
                .filter_map(|m| doc.get(m))
                .collect();
            let pick = |want: Option<Id<SyncMember>>, role: MemberRole| {
                want.and_then(|w| doc.get(w)).or_else(|| {
                    members
                        .iter()
                        .copied()
                        .find(|m| m.role == role || m.role == MemberRole::Both)
                })
            };
            let pic = pick(group.picture, MemberRole::Picture)?;
            let aud = pick(group.master_audio, MemberRole::Audio)?;
            Some((
                pic.source,
                member_time(doc, pic.id, t),
                aud.source,
                member_time(doc, aud.id, t),
            ))
        }
    }
}

/// A picture source's exact rate when RenderPlan outputs it (NTSC included, decision
/// 0020): a plan that addresses its frames by index runs at it, so the picture cannot
/// drift from the sound. `None` for any other rate.
pub(crate) fn picture_rate(ps: &Source) -> Option<gmclip_plan::types::Fps> {
    let f = ps.probe.tracks.iter().find(|t| t.width.is_some())?.fps?;
    gmclip_plan::types::Fps::new(u64::try_from(f.num).ok()?, u64::from(f.den)).ok()
}

/// `{num, den}` and the schema version of a plan at `fps`.
pub(crate) fn rate_json(fps: gmclip_plan::types::Fps) -> (serde_json::Value, &'static str) {
    let (n, d) = fps.rate();
    (
        json!({"num": n, "den": d}),
        if d == 1 {
            "gmclip.renderplan/0"
        } else {
            "gmclip.renderplan/1"
        },
    )
}

/// A picture source's size and integer frame rate (30 when the probe has none).
pub(crate) fn picture_info(ps: &Source) -> AppResult<(u32, u32, i64)> {
    let (sw, sh, fps) = match ps.probe.tracks.iter().find(|t| t.width.is_some()) {
        Some(t) => (
            t.width.unwrap_or(0),
            t.height.unwrap_or(0),
            t.fps
                .filter(|f| f.den == 1 && f.num > 0)
                .map_or(30, |f| f.num),
        ),
        None => {
            let ix = crate::inputs::index_file(std::path::Path::new(&ps.path))?;
            let v = ix
                .video
                .ok_or_else(|| AppError::invalid("the stream has no video"))?;
            (v.width, v.height, 30)
        }
    };
    if sw == 0 || sh == 0 {
        return Err(AppError::invalid("the stream's picture size is unknown"));
    }
    Ok((sw, sh, fps))
}

/// A one-part RenderPlan showing `[t0, t1)` of the moment's stream, full frame.
pub fn review_plan(doc: &Doc, m: &Moment, span: (Flicks, Flicks)) -> AppResult<Plan> {
    stream_plan(doc, m.media, &format!("review {}", m.id.0), span).map(|x| x.0)
}

/// A one-part RenderPlan showing `[t0, t1)` of a stream (media clock), full frame, and
/// the media time of its frame 0 (`t0` snapped to the picture's frame grid).
pub fn stream_plan(
    doc: &Doc,
    media: gmclip_model::entities::MediaRef,
    title: &str,
    (t0, t1): (Flicks, Flicks),
) -> AppResult<(Plan, Flicks)> {
    let (pic, tp, aud, ta) = media_at(doc, media, t0.0)
        .ok_or_else(|| AppError::not_found("the stream is not in this project"))?;
    let ps = doc
        .get(pic)
        .ok_or_else(|| AppError::not_found("no picture source"))?;
    let asrc = doc
        .get(aud)
        .ok_or_else(|| AppError::not_found("no audio source"))?;
    let (sw, sh, int_fps) = picture_info(ps)?;
    // the picture's own rate (NTSC exactly, decision 0020), so index addressing cannot
    // drift from the sound
    let rate = picture_rate(ps).unwrap_or_else(|| {
        gmclip_plan::types::Fps::new(int_fps.max(1) as u64, 1)
            .unwrap_or_else(|_| gmclip_plan::types::Fps::integer(30))
    });
    let (fps_json, schema) = rate_json(rate);
    let cw = (sw.min(1280) & !1).max(2);
    let ch = ((f64::from(cw) * f64::from(sh) / f64::from(sw)).round() as u32 & !1).max(2);
    let fpf = rate.flicks_per_frame() as i64;
    let first = (tp + fpf / 2).div_euclid(fpf).max(0);
    let n = ((t1.0 - t0.0 + fpf / 2) / fpf).clamp(1, 30 * 60 * FLICKS / fpf);
    let first_sample = (i128::from(ta.max(0)) * 48_000 / i128::from(FLICKS)) as i64;
    let v = json!({
        "schema": schema,
        "meta": {"title": title},
        "output": {
            "canvas": {"w": cw, "h": ch},
            "fps": fps_json,
            "frames": n,
            "video": {"codec": "h264"},
            "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2}
        },
        "background": {"kind": "colour", "rgb": "#000000"},
        "sources": [{"id": "pic", "path": ps.path}, {"id": "aud", "path": asrc.path}],
        "parts": [{
            "id": "moment",
            "out": [0, n],
            "picture": {"source": "pic", "first_frame": first},
            "audio": {"source": "aud", "first_sample": first_sample, "rate": 48000},
            "layers": [{
                "id": "stream", "role": "other",
                "crop": {"x": 0, "y": 0, "w": sw, "h": sh},
                "place": {"x": 0, "y": 0, "w": cw, "h": ch},
                "z": 0
            }]
        }],
        "audio": {"loudness": {"mode": "as_recorded"}}
    });
    let plan = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap_or_default()).map_err(|e| {
        AppError::invalid(format!(
            "the review plan is invalid: {} ({})",
            e.reason(),
            e.path()
        ))
    })?;
    // frame 0 shows picture frame `first`: on the media clock that is t0 moved by the snap
    Ok((plan, Flicks(t0.0 + first * fpf - tp)))
}

/// The review plan for a moment id.
pub fn review_for(
    p: &OpenProject,
    ts: &Transcripts,
    moment: &str,
) -> AppResult<(Plan, (f64, f64))> {
    let doc = p.handle.snapshot().doc.clone();
    let id: Id<Moment> = crate::planner::ulid_id(moment)?;
    let m = doc
        .get(id)
        .ok_or_else(|| AppError::not_found("no such moment"))?;
    let t = ts.latest(p, &owner_of(m));
    let (_, review) = spans(&doc, m, t.as_deref()).ok_or_else(|| {
        AppError::new(
            "Unsupported",
            "this moment's times need its transcript (line anchors)",
        )
    })?;
    let plan = review_plan(&doc, m, review)?;
    Ok((plan, (secs(review.0), secs(review.1))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmclip_model::anchor::{Anchor, RawTime};
    use gmclip_model::clock::ModelTime;
    use gmclip_model::entities::{Edges, Excerpt, MediaRef, MomentSpan, MomentState};

    fn moment(origin: MomentOrigin, pass_ref: Option<&str>) -> Moment {
        let s = Id::new(gmclip_model::Ulid::parse("01J00000000000000000000003").unwrap());
        let raw = |t| {
            Anchor::Raw(RawTime {
                time: ModelTime::Src {
                    source: Some(s),
                    group: None,
                    t: Flicks(t),
                },
                reason: "t".into(),
            })
        };
        Moment {
            id: Id::new(gmclip_model::Ulid::parse("01J00000000000000000000009").unwrap()),
            media: MediaRef::Source(s),
            span: MomentSpan {
                start: raw(0),
                end: raw(FLICKS),
                edges: Edges::Word,
            },
            payoff: None,
            quote: "q".into(),
            excerpt: Excerpt {
                text: "q".into(),
                exact: true,
            },
            score: 3,
            r#type: "result".into(),
            why: String::new(),
            payoff_hint: None,
            text_hook: None,
            origin,
            origin_ref: None,
            entry_angle: None,
            variant_of: None,
            visual: None,
            state: MomentState::New,
            pass_ref: pass_ref.map(str::to_owned),
            harness_label: None,
        }
    }

    /// A.01: the pass's moments, the full sweep's included, carry the neutral label; a
    /// sweep moment without one of gmclip's pass refs is labelled as a sweep.
    #[test]
    fn agent_executed_moments_are_labelled_recall_not_measured() {
        let label = "Found by your agent — recall not measured";
        assert_eq!(
            origin_label(&moment(
                MomentOrigin::Agent,
                Some("find_moments@1:abc/pr_1")
            )),
            label
        );
        assert_eq!(
            origin_label(&moment(MomentOrigin::Sweep, Some("full_sweep@1:abc/pr_2"))),
            label
        );
        assert_eq!(
            origin_label(&moment(MomentOrigin::Sweep, None)),
            "Found by a sweep"
        );
    }
}
