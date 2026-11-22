//! A new cut from a moment (the walkthrough's first blocker: a staged moment had no way
//! into a cut — review mode's `g` only said "the story panel arrives with U.07"). One
//! click on a moment row, or `g` in review mode: a cut on the moment (lineage kept), a
//! 9:16 framing at the stream picture's rate, the profile kit's layout, loudness and
//! caption preset (with no profile: AJ's screen + facecam at −16 LUFS), captions on, and one part over the moment's span on the
//! frame grid. The story panel then grows it.

use gmclip_model::Id;
use gmclip_model::entities::{Moment, Source, SyncMember, TrackKind};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::{FLICKS, ulid_id};

pub fn from_moment(core: &AppCore, project: &str, moment: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let mid: Id<Moment> = ulid_id(moment)?;
    let m: &Moment = doc
        .get(mid)
        .ok_or_else(|| AppError::not_found("no such moment"))?;
    let t = core.transcripts.latest(&p, &crate::review::owner_of(m));
    let (span, _) = crate::review::spans(&doc, m, t.as_deref())
        .ok_or_else(|| AppError::new("Unsupported", "this moment's times need its transcript"))?;
    // the picture's frame rate (a sync group: its picture member's)
    let pic: Option<&Source> = match m.media {
        gmclip_model::entities::MediaRef::Source(s) => doc.get(s),
        gmclip_model::entities::MediaRef::Group(g) => doc
            .iter::<SyncMember>()
            .find(|x| x.group == g && x.role == gmclip_model::entities::MemberRole::Picture)
            .and_then(|x| doc.get(x.source)),
    };
    let fps = pic
        .and_then(|s| s.probe.tracks.iter().find(|t| t.kind == TrackKind::Video))
        .and_then(|t| t.fps)
        .map_or(
            gmclip_model::Rational::new(30, 1),
            crate::video::framing_fps,
        );
    // a short's name is a few words (AJ 2026-11-22): its hook, else the topic it starts
    // in, else its first words; renamed like any cut
    let title = short_title(
        m.text_hook.as_deref(),
        topic_at(&doc, m, span.0, t.as_deref()).as_deref(),
        &m.excerpt.text,
    );
    let out = |v: Value| v["output"].clone();
    let cut = out(core.call(
        project,
        "cut_create",
        json!({"title": title, "moment": moment}),
    )?)["cut"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    crate::studio::seed_kit(core, project);
    let framing = out(core.call(
        project,
        "framing_create",
        json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": fps.num, "den": fps.den}}),
    )?)["id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    // a profile stream layout that names this source gives it its camera box (A2.20)
    let matched = crate::takelayout::match_stream_layout(core, project, &m.media)?;
    let doc = p.handle.snapshot().doc.clone();
    // a landscape video with no layout at all: the cut seeds without a face, and the UI
    // offers "Draw your layout once" (SPEC §5.7.2) for that video
    let needs_layout = crate::framing::picture_source(&doc, &m.media)
        .and_then(|s| doc.get(s))
        .filter(|s| s.stream_layout.is_none())
        .filter(|_| crate::framing::source_boxes(&doc, &m.media).1.is_none())
        .filter(|s| {
            s.probe
                .tracks
                .iter()
                .find(|t| t.kind == gmclip_model::entities::TrackKind::Video)
                .and_then(|t| Some(t.width? > t.height?))
                .unwrap_or(false)
        })
        .map(|s| s.id.0.to_string());
    // the profile kit's template, loudness and caption preset (SPEC §5.7.1 step 2); with
    // no profile, AJ's screen + facecam at −16 LUFS
    let seed = crate::framing::KitSeed::of(core);
    let preset = seed.preset(
        crate::framing::source_boxes(&doc, &m.media).1.is_some(),
        "screen_cam",
    );
    core.set_active_framing(project, &cut, &framing)?;
    let fpf = gmclip_core::planner::framing_fpf(fps).unwrap_or(FLICKS / 30);
    // inside what every member of the media covers (a hi-res picture may start later)
    let (lo, hi) = media_range(&doc, m.media);
    let (a, b) = (span.0.0.max(lo), span.1.0.min(hi));
    if b <= a {
        return Err(AppError::invalid(
            "this moment is outside what the paired files both cover",
        ));
    }
    let k0 = (a + fpf - 1) / fpf;
    let k1 = (b / fpf).max(k0 + 1);
    core.call(
        project,
        "part_add",
        json!({"cut": cut, "media": m.media, "src_in": k0 * fpf, "src_out": k1 * fpf, "role": "hook"}),
    )?;
    // SPEC §5.7.1 steps 3 and 5: the face from the source's cam region (its own, else its
    // stream layout's) and the caption seat written into the seeded layout
    crate::framing::apply_preset_seated(
        core,
        project,
        &cut,
        &framing,
        &[],
        preset,
        seed.seat(preset),
    )?;
    crate::framing::seed_framing(core, project, &cut, &framing, &seed, preset)?;
    crate::short::captions_set(core, project, &cut, true)?;
    Ok(
        json!({"cut": cut, "framing": framing, "seconds": ((k1 - k0) * fpf) as f64 / FLICKS as f64,
              "stream_layout": matched, "needs_layout": needs_layout}),
    )
}

/// The media-clock range a part on `media` can cover: a source's own length, or for a
/// sync group the stretch every member covers (member time = group time − offset).
pub fn media_range(doc: &gmclip_model::Doc, media: gmclip_model::entities::MediaRef) -> (i64, i64) {
    use gmclip_model::entities::{MediaRef, SyncSegment};
    match media {
        MediaRef::Source(s) => (
            0,
            doc.get::<Source>(s).map_or(i64::MAX, |x| {
                if x.probe.duration.0 > 0 {
                    x.probe.duration.0
                } else {
                    i64::MAX
                }
            }),
        ),
        MediaRef::Group(g) => {
            let (mut lo, mut hi) = (0i64, i64::MAX);
            for mid in doc.children::<SyncMember>(g.entity()) {
                let Some(mm) = doc.get::<SyncMember>(mid) else {
                    continue;
                };
                let dur = doc
                    .get::<Source>(mm.source)
                    .map_or(0, |x| x.probe.duration.0);
                let mut segs: Vec<&SyncSegment> = doc
                    .children::<SyncSegment>(mm.id.entity())
                    .filter_map(|x| doc.get(x))
                    .collect();
                segs.sort_by_key(|x| x.src_range.start);
                if let (Some(f), Some(l)) = (segs.first(), segs.last()) {
                    lo = lo.max(f.offset.0);
                    if dur > 0 {
                        hi = hi.min(l.offset.0 + dur);
                    }
                }
            }
            (lo, hi)
        }
    }
}

/// The title of the accepted topic (on the moment's media) that `at` falls in, if any.
pub fn topic_at(
    doc: &gmclip_model::Doc,
    m: &Moment,
    at: gmclip_model::Flicks,
    t: Option<&crate::review::Transcript>,
) -> Option<String> {
    use gmclip_model::entities::{ProposalState, Topic};
    doc.iter::<Topic>()
        .filter(|x| x.state == ProposalState::Accepted && x.media == m.media)
        .find(|x| {
            let a = crate::review::anchor_time(doc, &x.span.start, t);
            let b = crate::review::anchor_time(doc, &x.span.end, t);
            matches!((a, b), (Some(a), Some(b)) if a <= at && at < b)
        })
        .map(|x| x.title.clone())
        .filter(|x| !x.trim().is_empty())
}

/// A short's name in a few words: the hook if it has one, else its topic, else the first
/// words of what is said (up to five, ending at a sentence's end, never on "and" or "the").
pub fn short_title(hook: Option<&str>, topic: Option<&str>, said: &str) -> String {
    const MAX_WORDS: usize = 5;
    const MAX_CHARS: usize = 40;
    const WEAK: &[&str] = &[
        "and", "or", "but", "so", "the", "a", "an", "to", "of", "with", "for", "in", "on", "at",
        "is", "it's", "that", "this", "my", "your", "i", "we",
    ];
    let pick = |x: Option<&str>| {
        x.map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_owned)
    };
    // a hook or a topic is a name someone chose: whole when it is short, else cut at a word
    if let Some(name) = pick(hook).or_else(|| pick(topic)) {
        if name.chars().count() <= MAX_CHARS {
            return name;
        }
        let mut out = String::new();
        for w in name.split_whitespace() {
            if out.chars().count() + w.chars().count() + 1 > MAX_CHARS {
                break;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(w);
        }
        return out;
    }
    let src = said.trim().to_owned();
    let mut words: Vec<String> = Vec::new();
    for w in src.split_whitespace() {
        let end = w.ends_with(['.', '?', '!']);
        words.push(w.to_owned());
        let chars: usize = words.iter().map(|x| x.chars().count() + 1).sum();
        if words.len() >= MAX_WORDS || chars > MAX_CHARS || (end && words.len() >= 2) {
            break;
        }
    }
    let clean = |w: &str| {
        w.trim_end_matches(['.', ',', ';', ':', '!', '?', '…', '-', '—'])
            .to_owned()
    };
    while words.len() > 1 && WEAK.contains(&clean(words.last().unwrap()).to_lowercase().as_str()) {
        words.pop();
    }
    let mut title = words
        .iter()
        .map(|w| w.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    title = clean(&title);
    if title.is_empty() {
        return "New short".to_owned();
    }
    let mut c = title.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => title,
    }
}

#[cfg(test)]
mod title_tests {
    use super::short_title;

    #[test]
    fn a_few_words() {
        assert_eq!(
            short_title(
                None,
                None,
                "I stopped using Codex and Claude Code harness directly to use something no one's really heard of."
            ),
            "I stopped using Codex"
        );
        assert_eq!(
            short_title(
                None,
                None,
                "it's called BB, the agentic IDE. It runs the real code"
            ),
            "It's called BB, the agentic"
        );
        assert_eq!(
            short_title(
                Some("My PC made this in 67 seconds"),
                Some("Hardware"),
                "whatever"
            ),
            "My PC made this in 67 seconds"
        );
        assert_eq!(
            short_title(None, Some("What BB is"), "whatever"),
            "What BB is"
        );
        assert_eq!(
            short_title(Some("  "), Some("What BB is"), "whatever"),
            "What BB is"
        );
        assert_eq!(short_title(None, None, "   "), "New short");
        assert_eq!(short_title(None, None, "Wow."), "Wow");
    }
}
