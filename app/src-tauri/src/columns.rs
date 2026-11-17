//! The timeline's binary columns (SPEC §12.1, backlog D.02): `gm://data/<project>/<kind>?cut=`
//! for `peaks`, `energy`, `quiet` and `words`, on the cut's **output** clock, each after
//! the 16-byte header (`version u16, kind u16, count u32, rev u64`, LE; `rev` the cut's
//! plan revision). Built from the analysis blobs (`peaks_100hz`, `energy_10ms`,
//! `silences_40ms`) and the transcript the parts read; blobs are decoded once per
//! revision and kept ([`Decoded`]), so a re-plan only re-maps.
//!
//! Bodies (all little-endian):
//! - `peaks` (kind 4): per output frame, `min f32, max f32` (−1…1; 0 where nothing plays).
//! - `energy` (kind 5): per output frame, `f32` dB (the loudest 10 ms hop in the frame;
//!   −120 where nothing plays).
//! - `quiet` (kind 6): per region, `start u32, end u32` output frames (end exclusive).
//! - `words` (kind 2): per word the cut plays, in output order, seven `u32`: transcript
//!   ordinal, source start and end (ms), output frame of its start, part index, and the
//!   word's text offset and byte length in the UTF-8 text that follows the table.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use gmclip_analysis::blob::{Blob, Column};
use gmclip_analysis::energy::Energy;
use gmclip_analysis::peaks::Peaks;
use gmclip_core::planner::audio_source;
use gmclip_model::entities::{Cut, MediaRef};
use gmclip_model::{Doc, Id};

use crate::core::{AppCore, OpenProject};
use crate::planner::FLICKS;

pub const KIND_WORDS: u16 = 2;
pub const KIND_PEAKS: u16 = 4;
pub const KIND_ENERGY: u16 = 5;
pub const KIND_QUIET: u16 = 6;

/// Level where nothing plays (dB).
pub const SILENT_DB: f32 = -120.0;

/// One blob, decoded.
#[derive(Debug)]
pub enum Decoded {
    Peaks(Peaks),
    Energy(Energy),
    /// Quiet regions in samples at `rate`.
    Quiet {
        rate: u32,
        regions: Vec<(i64, i64)>,
    },
}

type Cache = Mutex<HashMap<(String, String, String, String), Arc<Decoded>>>;

fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// The newest stored blob of `kind` for `owner`, decoded (and kept).
fn blob(p: &OpenProject, owner: &str, kind: &str) -> Option<Arc<Decoded>> {
    let rev = p
        .handle
        .analysis_keys()
        .into_iter()
        .filter(|(o, k, _)| o == owner && k == kind)
        .map(|(_, _, r)| r)
        .max()?;
    let key = (p.info.id.clone(), owner.to_owned(), kind.to_owned(), rev);
    if let Some(d) = cache().lock().ok()?.get(&key) {
        return Some(d.clone());
    }
    let bytes = p.handle.analysis(owner, kind, &key.3)?;
    let b = Blob::decode(&bytes).ok()?;
    let d = Arc::new(match kind {
        "peaks_100hz" => Decoded::Peaks(Peaks::from_blob(&b).ok()?),
        "energy_10ms" => Decoded::Energy(Energy::from_blob(&b).ok()?),
        "silences_40ms" => {
            let rate = b.int("rate").and_then(|r| u32::try_from(r).ok())?;
            let (Some(Column::I64(st)), Some(Column::I64(en))) = (b.get("start"), b.get("end"))
            else {
                return None;
            };
            Decoded::Quiet {
                rate,
                regions: st.iter().copied().zip(en.iter().copied()).collect(),
            }
        }
        _ => return None,
    });
    if let Ok(mut g) = cache().lock() {
        // one project's blobs at a time is plenty (a 3 h source's are ~30 MB decoded)
        if g.len() > 16 {
            g.clear();
        }
        g.insert(key, d.clone());
    }
    Some(d)
}

/// A part on the output clock: its first output frame, frame count, and where its audio
/// is (owner, the offset from the part's media clock to the owner's, flicks).
struct Span {
    part: usize,
    /// Output start (flicks) and first output frame.
    start: i64,
    first: u64,
    frames: u64,
    src_in: i64,
    audio: Option<(String, i64)>,
    media: MediaRef,
}

fn spans(doc: &Doc, cut: Id<Cut>, fpf: i64) -> Vec<Span> {
    crate::tighten::layout(doc, cut)
        .into_iter()
        .enumerate()
        .map(|(i, (p, start, dur))| Span {
            part: i,
            start,
            first: (start / fpf) as u64,
            frames: (dur / fpf).max(1) as u64,
            src_in: p.src_in.0,
            audio: audio_source(doc, p.media, p.src_in.0).map(|(s, off)| (s.0.to_string(), off)),
            media: p.media,
        })
        .collect()
}

/// A part's blob: under its audio source (as `timeline_audio` reads it), else under its
/// sync group (where the app's analyzer stores a group's).
fn blob_of(p: &OpenProject, s: &Span, owner: &str, kind: &str) -> Option<Arc<Decoded>> {
    blob(p, owner, kind).or_else(|| match s.media {
        MediaRef::Group(g) => blob(p, &g.0.to_string(), kind),
        MediaRef::Source(_) => None,
    })
}

fn header(kind: u16, count: u32, rev: u64) -> Vec<u8> {
    let mut b = Vec::with_capacity(16);
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&kind.to_le_bytes());
    b.extend_from_slice(&count.to_le_bytes());
    b.extend_from_slice(&rev.to_le_bytes());
    b
}

/// The column of `kind` for a cut, or `None` (not an open project's cut, or a kind this
/// module does not build).
pub fn column(core: &AppCore, project: &str, cut: &str, kind: &str) -> Option<Vec<u8>> {
    let p = core.project(project).ok()?;
    let doc = p.handle.snapshot().doc.clone();
    let cid: Id<Cut> = crate::planner::ulid_id(cut).ok()?;
    doc.get(cid)?;
    let pl = core.planned(project, cut).ok()?;
    let fpf = crate::planner::fpf(&pl.plan);
    let total = pl.plan.output.frames;
    let sp = spans(&doc, cid, fpf);
    match kind {
        "peaks" | "energy" => Some(levels(&p, &sp, fpf, total, kind == "peaks", pl.rev)),
        "quiet" => Some(quiet(&p, &sp, fpf, total, pl.rev)),
        "words" => Some(words(core, &p, &sp, fpf, pl.rev)),
        _ => None,
    }
}

/// `peaks` (min/max per frame) or `energy` (dB per frame).
fn levels(p: &OpenProject, sp: &[Span], fpf: i64, total: u64, peaks: bool, rev: u64) -> Vec<u8> {
    let n = total as usize;
    let (mut lo, mut hi) = (vec![0f32; n], vec![0f32; n]);
    let mut db = vec![SILENT_DB; n];
    for s in sp {
        let Some((owner, off)) = &s.audio else {
            continue;
        };
        let d = blob_of(
            p,
            s,
            owner,
            if peaks { "peaks_100hz" } else { "energy_10ms" },
        );
        for k in 0..s.frames {
            let f = (s.first + k) as usize;
            if f >= n {
                break;
            }
            // the frame's source time on the owner's clock (flicks)
            let a = s.src_in + k as i64 * fpf - off;
            let b = a + fpf;
            match d.as_deref() {
                Some(Decoded::Peaks(pk)) => {
                    let r = i64::from(gmclip_analysis::peaks::PEAKS_HZ);
                    let (i0, i1) = bucket_range(a, b, r, pk.min.len());
                    if i1 > i0 {
                        lo[f] = pk.min[i0..i1].iter().copied().fold(f32::INFINITY, f32::min);
                        hi[f] = pk.max[i0..i1]
                            .iter()
                            .copied()
                            .fold(f32::NEG_INFINITY, f32::max);
                    }
                }
                Some(Decoded::Energy(e)) => {
                    // hops whose start falls inside the frame
                    let hops_per_s = i64::from(e.rate) / e.hop.max(1) as i64;
                    let (i0, i1) = bucket_range(a, b, hops_per_s.max(1), e.db.len());
                    if i1 > i0 {
                        db[f] = e.db[i0..i1].iter().copied().fold(f32::MIN, f32::max);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = header(if peaks { KIND_PEAKS } else { KIND_ENERGY }, n as u32, rev);
    if peaks {
        out.reserve(n * 8);
        for (a, b) in lo.iter().zip(&hi) {
            out.extend_from_slice(&a.to_le_bytes());
            out.extend_from_slice(&b.to_le_bytes());
        }
    } else {
        out.reserve(n * 4);
        for v in &db {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

/// A part's level (dB per frame, as `energy`) on its own source, from `before` frames
/// before its in point to `after` past its out point (decision 0053): what a trim drag
/// reveals, drawn at the timeline's scale. `None` without the part or its levels.
pub fn part_energy(
    core: &AppCore,
    project: &str,
    cut: &str,
    part: &str,
    before: u64,
    after: u64,
) -> Option<serde_json::Value> {
    let p = core.project(project).ok()?;
    let doc = p.handle.snapshot().doc.clone();
    let pl = core.planned(project, cut).ok()?;
    let fpf = crate::planner::fpf(&pl.plan);
    let mp = doc.get(crate::planner::ulid_id::<gmclip_model::entities::Part>(part).ok()?)?;
    let len = ((mp.src_out.0 - mp.src_in.0) / fpf).max(1) as u64;
    let (before, after) = (before.min(36_000), after.min(36_000));
    let (owner, off) =
        audio_source(&doc, mp.media, mp.src_in.0).map(|(s, o)| (s.0.to_string(), o))?;
    let s = Span {
        part: 0,
        start: 0,
        first: 0,
        frames: before + len + after,
        src_in: mp.src_in.0 - before as i64 * fpf,
        audio: None,
        media: mp.media,
    };
    let d = blob_of(&p, &s, &owner, "energy_10ms")?;
    let Decoded::Energy(e) = d.as_ref() else {
        return None;
    };
    let hops_per_s = (i64::from(e.rate) / e.hop.max(1) as i64).max(1);
    let db: Vec<f32> = (0..s.frames)
        .map(|k| {
            let a = s.src_in + k as i64 * fpf - off;
            let (i0, i1) = bucket_range(a, a + fpf, hops_per_s, e.db.len());
            if a < 0 || i1 <= i0 {
                SILENT_DB
            } else {
                e.db[i0..i1].iter().copied().fold(f32::MIN, f32::max)
            }
        })
        .collect();
    Some(serde_json::json!({ "first": -(before as i64), "db": db }))
}

/// The buckets of a `per_s`-per-second series that start inside `[a, b)` flicks (at
/// least the one holding `a`), clamped to `len`.
fn bucket_range(a: i64, b: i64, per_s: i64, len: usize) -> (usize, usize) {
    if b <= 0 {
        return (0, 0);
    }
    let a = a.max(0);
    let i0 = (a as i128 * per_s as i128 / FLICKS as i128) as usize;
    let i1 = ((b as i128 * per_s as i128).div_euclid(FLICKS as i128) as usize).max(i0 + 1);
    (i0.min(len), i1.min(len))
}

/// `quiet`: the quiet regions the cut plays, in output frames.
fn quiet(p: &OpenProject, sp: &[Span], fpf: i64, total: u64, rev: u64) -> Vec<u8> {
    let mut regs: Vec<(u32, u32)> = Vec::new();
    for s in sp {
        let Some((owner, off)) = &s.audio else {
            continue;
        };
        let Some(d) = blob_of(p, s, owner, "silences_40ms") else {
            continue;
        };
        let Decoded::Quiet { rate, regions } = &*d else {
            continue;
        };
        let rate = i64::from(*rate);
        // the part's span on the owner's clock, in samples
        let (pa, pb) = (s.src_in - off, s.src_in - off + s.frames as i64 * fpf);
        let to_s = |f: i64| (f as i128 * rate as i128 / FLICKS as i128) as i64;
        let (sa, sb) = (to_s(pa), to_s(pb));
        let from = regions.partition_point(|r| r.1 <= sa);
        for &(ra, rb) in regions[from..].iter().take_while(|r| r.0 < sb) {
            let (ca, cb) = (ra.max(sa), rb.min(sb));
            if cb <= ca {
                continue;
            }
            let fl = |x: i64| (x as i128 * FLICKS as i128 / rate as i128) as i64;
            let fa = s.first as i64 + (fl(ca) - fl(sa)) / fpf;
            let fb = s.first as i64 + (fl(cb) - fl(sa) + fpf - 1) / fpf;
            let (fa, fb) = (fa.clamp(0, total as i64), fb.clamp(0, total as i64));
            if fb > fa {
                regs.push((fa as u32, fb as u32));
            }
        }
    }
    let mut out = header(KIND_QUIET, regs.len() as u32, rev);
    for (a, b) in regs {
        out.extend_from_slice(&a.to_le_bytes());
        out.extend_from_slice(&b.to_le_bytes());
    }
    out
}

fn owner_of(m: &MediaRef) -> String {
    match m {
        MediaRef::Source(s) => s.0.to_string(),
        MediaRef::Group(g) => g.0.to_string(),
    }
}

/// `words`: the words the cut plays (more than half inside a part), in output order.
fn words(core: &AppCore, p: &OpenProject, sp: &[Span], fpf: i64, rev: u64) -> Vec<u8> {
    let mut rows: Vec<[u32; 7]> = Vec::new();
    let mut text = String::new();
    let mut ts: HashMap<String, Option<Arc<crate::review::Transcript>>> = HashMap::new();
    let ms = |f: i64| (f.max(0) as i128 * 1000 / FLICKS as i128).min(u32::MAX as i128) as u32;
    for s in sp {
        let o = owner_of(&s.media);
        let t = ts
            .entry(o.clone())
            .or_insert_with(|| core.transcripts.latest(p, &o))
            .clone();
        let Some(t) = t else { continue };
        let (pa, pb) = (s.src_in, s.src_in + s.frames as i64 * fpf);
        // words are in start order: from the first that ends after the part starts
        let from = t.words.partition_point(|w| w.end <= pa);
        for (i, w) in t.words.iter().enumerate().skip(from) {
            if w.start >= pb {
                break;
            }
            let inside = (w.end.min(pb) - w.start.max(pa)).max(0);
            if 2 * inside < (w.end - w.start).max(1) {
                continue;
            }
            // as the words strip counts it
            let at = (s.start + w.start.max(pa) - pa) / fpf;
            let off = text.len();
            text.push_str(&w.text);
            rows.push([
                i as u32,
                ms(w.start),
                ms(w.end),
                at.max(0) as u32,
                s.part as u32,
                off as u32,
                w.text.len() as u32,
            ]);
        }
    }
    let mut out = header(KIND_WORDS, rows.len() as u32, rev);
    out.reserve(rows.len() * 28 + text.len());
    for r in &rows {
        for v in r {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out.extend_from_slice(text.as_bytes());
    out
}

/// Times the columns' build for a `hours`-long source (D.02, the perf row): the one read
/// of the voice track (the ASR's, window by window, which the analysis after it now
/// shares: [`AnalysisHost::keep_voice`]), then from that voice repeated to `hours` the
/// columns' blobs (peaks, energy ×3, the quiet grid) made and stored on `project` under a
/// scratch owner, and the VAD after them.
///
/// [`AnalysisHost::keep_voice`]: gmclip_core::host::AnalysisHost::keep_voice
#[doc(hidden)]
pub fn time_build(
    core: &AppCore,
    project: &str,
    path: &std::path::Path,
    track: Option<u32>,
    until: i64,
    hours: f64,
) -> serde_json::Value {
    let t = std::time::Instant::now();
    let Ok(pcm) = crate::analysis::track_16k(
        path,
        track,
        gmclip_model::Flicks(0),
        gmclip_model::Flicks(until),
    ) else {
        return serde_json::json!({"error": "decode failed"});
    };
    let decode = t.elapsed().as_secs_f64();
    let src_h = pcm.len() as f64 / 16_000.0 / 3600.0;
    let want = (hours * 3600.0 * 16_000.0) as usize;
    let mut long = Vec::with_capacity(want);
    while long.len() < want && !pcm.is_empty() {
        let n = (want - long.len()).min(pcm.len());
        long.extend_from_slice(&pcm[..n]);
    }
    drop(pcm);
    let (Some(cli), Ok(p)) = (core.analysis.cli(), core.project(project)) else {
        return serde_json::json!({"error": "no analysis host or project"});
    };
    let a = gmclip_core::host::AudioSource {
        path: path.to_path_buf(),
        track,
        fingerprint: "d02-timing".into(),
    };
    let t = std::time::Instant::now();
    let mut columns_s = None;
    let mut bytes = 0usize;
    let mut store = std::time::Duration::ZERO;
    let r = cli.analyze_pcm(
        &a,
        &long,
        &std::sync::atomic::AtomicBool::new(false),
        &mut |b| {
            bytes += b.body.len();
            let ts = std::time::Instant::now();
            let _ = p
                .handle
                .put_analysis("d02-timing", &b.kind, &b.revision, b.body);
            store += ts.elapsed();
            if b.kind == "silences_40ms" {
                columns_s = Some(t.elapsed().as_secs_f64());
            }
            Ok(())
        },
    );
    let all = t.elapsed().as_secs_f64();
    serde_json::json!({
        "source_h": src_h,
        "decode_s": decode,
        "decode_per_h_s": decode / src_h,
        "long_h": long.len() as f64 / 16_000.0 / 3600.0,
        "columns_s": columns_s,
        "store_s": store.as_secs_f64(),
        "blob_bytes": bytes,
        "vad_s": columns_s.map(|c| all - c),
        "ok": r.is_ok(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_inside_a_frame() {
        // a 30 fps frame at 1 s holds 10 ms buckets 100..103 (3.33 of them)
        let f = FLICKS / 30;
        assert_eq!(bucket_range(FLICKS, FLICKS + f, 100, 10_000), (100, 103));
        // before the start and past the end
        assert_eq!(bucket_range(-2 * f, -f, 100, 10), (0, 0));
        assert_eq!(bucket_range(FLICKS, FLICKS + f, 100, 50), (50, 50));
        // a frame shorter than a bucket still gets the one holding its start
        assert_eq!(bucket_range(FLICKS, FLICKS + 1, 100, 10_000), (100, 101));
    }
}
