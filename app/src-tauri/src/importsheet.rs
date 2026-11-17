//! The import sheet (SPEC §5.1; backlog U.15 essentials): before a video opens, the
//! capability line for its video and each audio track (with the exact fix, `caps`), and
//! for a file with more than one audio track, "Which track is your mic?": a 3 s preview
//! per track (the same moment on each, so they compare) and a suggestion by speech ratio
//! (Silero VAD on a minute of each track). The answer sets the source's audio roles
//! (`source_audio_roles_set`), so transcripts and moment-finding hear the mic.
//!
//! A file with one audio track or only fine lines opens without the sheet; the one-track
//! note comes back as a note on open.

use std::path::Path;

use gmclip_media::demux::Codec;
use gmclip_model::Flicks;
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::FLICKS;

const RATE: usize = 16_000;
/// The stretch listened to for the suggestion.
const LISTEN_S: f64 = 60.0;
const PREVIEW_S: f64 = 3.0;

fn codec_label(c: &Codec) -> String {
    match c {
        Codec::H264 { .. } => "h264".into(),
        Codec::Hevc { .. } => "hevc".into(),
        Codec::Vp9 { .. } => "vp9".into(),
        Codec::Av1 { .. } => "av1".into(),
        Codec::Aac { .. } => "aac".into(),
        Codec::Pcm { .. } => "pcm".into(),
        Codec::Opus { .. } => "opus".into(),
        Codec::Other { name } => name
            .chars()
            .filter(|c| c.is_ascii_graphic())
            .take(24)
            .collect(),
    }
}

pub(crate) fn b64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// 16-bit little-endian PCM, base64 (the UI plays it through Web Audio at 16 kHz).
fn pcm16_b64(pcm: &[f32]) -> String {
    let mut b = Vec::with_capacity(pcm.len() * 2);
    for s in pcm {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        b.extend_from_slice(&v.to_le_bytes());
    }
    b64(&b)
}

fn rms_db(pcm: &[f32]) -> f64 {
    if pcm.is_empty() {
        return -120.0;
    }
    let ms = pcm
        .iter()
        .map(|s| f64::from(*s) * f64::from(*s))
        .sum::<f64>()
        / pcm.len() as f64;
    (10.0 * ms.max(1e-12).log10()).max(-120.0)
}

/// The 3 s of `pcm` with the most energy (sample offset).
fn loudest(pcm: &[f32], len: usize) -> usize {
    if pcm.len() <= len {
        return 0;
    }
    let step = RATE / 4;
    let e: Vec<f64> = pcm
        .chunks(step)
        .map(|c| c.iter().map(|s| f64::from(*s).powi(2)).sum())
        .collect();
    let k = len / step;
    let mut best = (0usize, f64::MIN);
    for i in 0..e.len().saturating_sub(k) {
        let s: f64 = e[i..i + k].iter().sum();
        if s > best.1 {
            best = (i, s);
        }
    }
    best.0 * step
}

/// What the sheet shows for `path`.
pub fn inspect(core: &AppCore, path: &str) -> AppResult<Value> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(AppError::invalid("an absolute path"));
    }
    let p = p
        .canonicalize()
        .map_err(|_| AppError::not_found("no file there"))?;
    let pr = crate::video::probe(&p)?;
    let ix = crate::inputs::index_file(&p)
        .map_err(|_| AppError::invalid("gmclip cannot read that file"))?;
    let v = ix
        .video
        .as_ref()
        .ok_or_else(|| AppError::invalid("that file has no video track"))?;
    let video = crate::caps::video(
        &codec_label(&v.codec),
        v.width,
        v.height,
        crate::caps::vp9_decoder(),
    );
    let dur = pr.probe.duration.0 as f64 / FLICKS as f64;
    let known = crate::video::load_recent(core)
        .iter()
        .any(|r| r.path == p && r.size == pr.size && r.home.is_dir());

    // a minute from a fifth of the way in (past intros), on every AAC track
    let from = (dur * 0.2).clamp(0.0, 30.0);
    let to = (from + LISTEN_S).min(dur);
    let fl = |s: f64| Flicks((s * FLICKS as f64).round() as i64);
    let cli = core.analysis.cli();
    let ask = ix.audio_all.len() > 1;
    let mut heard: Vec<Option<Vec<f32>>> = Vec::new();
    let mut tracks = Vec::new();
    for (n, t) in ix.audio_all.iter().enumerate() {
        let codec = codec_label(&t.codec);
        let line = crate::caps::audio(&codec, t.sample_rate, t.channels);
        let pcm = (ask && codec == "aac" && to > from)
            .then(|| {
                crate::analysis::track_16k(
                    &p,
                    Some(u32::try_from(t.id).unwrap_or(0)),
                    fl(from),
                    fl(to),
                )
                .ok()
            })
            .flatten();
        let speech = pcm
            .as_ref()
            .and_then(|x| cli.as_ref().and_then(|c| c.speech_ratio(x)));
        tracks.push(json!({
            "index": t.id, "n": n + 1, "codec": codec, "rate": t.sample_rate, "channels": t.channels,
            "line": line, "speech": speech, "level_db": pcm.as_ref().map(|x| (rms_db(x) * 10.0).round() / 10.0),
        }));
        heard.push(pcm);
    }
    // the suggestion: the most speech (ties: the louder); without the VAD, none
    let suggested = tracks
        .iter()
        .filter(|t| t["speech"].is_number())
        .max_by(|a, b| {
            let k = |t: &Value| {
                (
                    t["speech"].as_f64().unwrap_or(0.0),
                    t["level_db"].as_f64().unwrap_or(-120.0),
                )
            };
            k(a).partial_cmp(&k(b)).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|t| t["index"].clone());
    // one 3 s moment for every preview: where the suggested (else first) track is loudest
    let lead = suggested
        .as_ref()
        .and_then(|s| tracks.iter().position(|t| &t["index"] == s))
        .unwrap_or(0);
    let len = (PREVIEW_S * RATE as f64) as usize;
    let at = heard
        .get(lead)
        .and_then(|x| x.as_ref())
        .map_or(0, |x| loudest(x, len));
    for (t, pcm) in tracks.iter_mut().zip(&heard) {
        if let Some(x) = pcm {
            let end = (at + len).min(x.len());
            t["preview"] = json!(pcm16_b64(&x[at.min(end)..end]));
        }
    }
    let notes: Vec<&str> = match ix.audio_all.len() {
        0 => vec![crate::caps::NO_AUDIO],
        1 => vec![crate::caps::ONE_TRACK],
        _ => vec![],
    };
    let needs_sheet =
        !known && (ask || video.level != "ok" || tracks.iter().any(|t| t["line"]["level"] != "ok"));
    // the ETA chain (SPEC §12.5): the transcript at this Mac's speed, then the moments,
    // which wait for a click with an agent (D17)
    let (speed, basis) = crate::analysis::asr_speed();
    let eta = json!({"transcript_s": (dur / speed.max(0.1)).round(), "speed": (speed * 10.0).round() / 10.0, "basis": basis});
    Ok(json!({
        "path": p, "name": p.file_stem().map(|s| s.to_string_lossy().into_owned()),
        "duration_s": dur, "video": video, "tracks": tracks, "ask": ask, "eta": eta,
        "suggested": suggested, "preview_at_s": from + at as f64 / RATE as f64, "preview_rate": RATE,
        "notes": notes, "known": known, "sheet": needs_sheet,
    }))
}

/// After the video opened: the mic the human chose becomes the source's voice track.
pub fn set_voice(core: &AppCore, project: &str, voice: u32) -> AppResult<()> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let src = doc
        .iter::<gmclip_model::entities::Source>()
        .next()
        .ok_or_else(|| AppError::internal("no source"))?;
    core.call(
        project,
        "source_audio_roles_set",
        json!({"source": src.id.0.to_string(), "voice": voice}),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn loudest_window() {
        let mut x = vec![0.0f32; RATE * 10];
        for s in &mut x[RATE * 6..RATE * 9] {
            *s = 0.5;
        }
        assert_eq!(loudest(&x, RATE * 3), RATE * 6);
    }
}
