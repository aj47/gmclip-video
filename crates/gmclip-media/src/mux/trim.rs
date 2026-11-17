//! Lossless spans (L.06, consolidate): only the stretches of an H.264 + AAC MP4 that a
//! project uses, re-wrapped with our own writer — every sample byte-identical, nothing
//! re-encoded — and where each stretch landed, so the project's times can be mapped onto
//! the new file through its sync model.
//!
//! - **Video** is cut on whole closed GOPs: a span starts at the last sync sample at or
//!   before it and runs to the next sync sample at or after its end. A GOP that presents a
//!   frame before its own sync sample (an open GOP) is refused: copying it would decode
//!   wrongly. Pieces are joined so decode times stay contiguous (the writer's rule); for a
//!   constant-rate stream presentation is then contiguous too. Variable frame rate is
//!   refused (frame identity is position on the rate's grid).
//! - **Audio** (AAC, any rate) takes the packets covering each piece plus one packet
//!   either side (the decoder's overlap needs the one before), counted as decoders count
//!   them — 1024 samples a packet, whatever the input's `stts` says. The new file has no
//!   priming (iTunSMPB priming 0, as exports write), so decoded sample `q` of the new file
//!   is `q`; the input's priming (its `elst` media time) is taken into account.
//! - The input is read with [`super::inspect`] (whole file in memory, ≤ 4 GiB), like
//!   [`super::remux`]. One video and/or one audio track; others are refused.

use std::path::Path;

use super::inspect::{self, SampleInfo, TrackInfo};
use super::remux::{MAX_INPUT_BYTES, RemuxError, single_edit};
use super::{ColorTags, Gapless, Mp4Writer, MuxPlan, MuxReport, Sample, TrackConfig};

/// Samples an AAC-LC packet decodes to.
const AAC_FRAME: i64 = 1024;

fn bad(s: impl Into<String>) -> RemuxError {
    RemuxError::Input(s.into())
}

/// Where one copied stretch sits in the input and in the output.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Piece {
    /// Video: presentation index (on the frame-rate grid) of the piece's first frame in
    /// the input / output, and its frame count (0 without video).
    pub in_frame0: i64,
    pub out_frame0: i64,
    pub frames: u64,
    /// Audio: decoded sample position (input rate) of the piece's first packet in the
    /// input (after its priming; may be negative) / output, and its sample count.
    pub in_sample0: i64,
    pub out_sample0: i64,
    pub samples: u64,
    /// The input milliseconds the piece covers (its video, else its audio).
    pub in_start_ms: i64,
    pub in_end_ms: i64,
}

/// What [`trim`] wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct TrimReport {
    pub pieces: Vec<Piece>,
    /// The video's frame rate as `(ticks per second, ticks per frame)`; `None` without video.
    pub frame_rate: Option<(u32, u32)>,
    pub audio_rate: Option<u32>,
    pub video_samples: usize,
    pub audio_samples: usize,
    pub bytes_in: u64,
    pub mux: MuxReport,
}

struct Track<'a> {
    t: &'a TrackInfo,
    /// The input's edit: media ticks before the presentation starts.
    shift: i64,
}

fn at(s: &[SampleInfo], i: usize) -> Result<&SampleInfo, RemuxError> {
    s.get(i).ok_or_else(|| bad("sample index"))
}

fn sample_bytes<'a>(file: &'a [u8], s: &SampleInfo) -> Result<&'a [u8], RemuxError> {
    let start = usize::try_from(s.offset).map_err(|_| bad("offset"))?;
    file.get(start..start.saturating_add(s.size as usize))
        .ok_or_else(|| bad("sample outside the file"))
}

/// Decode-order ranges `[d0, d1)` of whole closed GOPs covering each span (presented
/// ticks), merged when they touch.
fn gop_ranges(v: &Track<'_>, spans: &[(i64, i64)]) -> Result<Vec<(usize, usize)>, RemuxError> {
    let s = &v.t.samples;
    let syncs: Vec<usize> = s
        .iter()
        .enumerate()
        .filter(|(_, x)| x.sync)
        .map(|(i, _)| i)
        .collect();
    if syncs.first() != Some(&0) {
        return Err(bad("the video does not start on a sync sample"));
    }
    let ppts = |i: usize| -> Result<i64, RemuxError> { Ok(at(s, i)?.pts - v.shift) };
    let mut out: Vec<(usize, usize)> = Vec::new();
    for &(a, b) in spans {
        let mut d0 = 0;
        for &k in syncs.iter().rev() {
            if ppts(k)? <= a {
                d0 = k;
                break;
            }
        }
        let mut d1 = s.len();
        for &k in &syncs {
            if k > d0 && ppts(k)? >= b {
                d1 = k;
                break;
            }
        }
        out.push((d0, d1));
    }
    out.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for r in out {
        match merged.last_mut() {
            Some(m) if r.0 <= m.1 => m.1 = m.1.max(r.1),
            _ => merged.push(r),
        }
    }
    // closed GOPs only: every frame of a range presents at or after its first sync frame,
    // and nothing after the range presents inside it
    for &(d0, d1) in &merged {
        let start = ppts(d0)?;
        for i in d0..d1 {
            if ppts(i)? < start {
                return Err(bad("an open GOP (a frame presents before its sync sample)"));
            }
        }
        if d1 < s.len() {
            let end = ppts(d1)?;
            for i in d1..s.len() {
                let p = ppts(i)?;
                if p < end && p >= start {
                    return Err(bad("an open GOP after a cut point"));
                }
            }
        }
    }
    Ok(merged)
}

/// Copies the stretches of `input` covering `spans_s` (seconds on its presentation clock)
/// into `out`. `video`/`audio` say which tracks the project reads from this file.
#[allow(clippy::too_many_lines)]
pub fn trim(
    input: &Path,
    out: &Path,
    spans_s: &[(f64, f64)],
    video: bool,
    audio: bool,
) -> Result<TrimReport, RemuxError> {
    let len = std::fs::metadata(input).map_err(RemuxError::Read)?.len();
    if len > MAX_INPUT_BYTES {
        return Err(bad(format!(
            "{len} bytes is over the {MAX_INPUT_BYTES}-byte limit"
        )));
    }
    if spans_s.is_empty()
        || spans_s
            .iter()
            .any(|(a, b)| !(a.is_finite() && b.is_finite() && b > a))
    {
        return Err(bad("spans must be non-empty and each end after its start"));
    }
    if !video && !audio {
        return Err(bad("nothing to copy"));
    }
    let file = std::fs::read(input).map_err(RemuxError::Read)?;
    let tracks = inspect::tracks(&file).map_err(|e| bad(e.to_string()))?;
    let movie_ts = inspect::movie_timescale(&file).map_err(|e| bad(e.to_string()))?;
    let vt: Vec<&TrackInfo> = tracks.iter().filter(|t| &t.handler == b"vide").collect();
    let st: Vec<&TrackInfo> = tracks.iter().filter(|t| &t.handler == b"soun").collect();
    if vt.len() > 1 || st.len() > 1 {
        return Err(bad("more than one video or audio track"));
    }
    let v = match (video, vt.first()) {
        (true, Some(t)) => {
            if &t.sample_entry != b"avc1" || t.entry.config.is_empty() {
                return Err(bad("the video is not H.264 (avc1 with avcC)"));
            }
            let (shift, _) = single_edit(t, movie_ts)?;
            Some(Track { t, shift })
        }
        (true, None) => return Err(bad("no video track")),
        _ => None,
    };
    let a = match (audio, st.first()) {
        (true, Some(t)) => {
            if &t.sample_entry != b"mp4a" || t.entry.config.is_empty() {
                return Err(bad("the audio is not AAC (mp4a with esds)"));
            }
            if t.timescale != t.entry.sample_rate {
                return Err(bad("audio timescale differs from its sample rate"));
            }
            let (priming, _) = single_edit(t, movie_ts)?;
            Some(Track { t, shift: priming })
        }
        (true, None) => return Err(bad("no audio track")),
        _ => None,
    };

    // video pieces as decode ranges, and the frame rate
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut frame_rate = None;
    if let Some(v) = &v {
        let s = &v.t.samples;
        if s.is_empty() {
            return Err(bad("the video has no samples"));
        }
        let ts = f64::from(v.t.timescale);
        let spans: Vec<(i64, i64)> = spans_s
            .iter()
            .map(|(a, b)| ((a * ts).floor() as i64, (b * ts).ceil() as i64))
            .collect();
        ranges = gop_ranges(v, &spans)?;
        let mut durs: Vec<u32> = s.iter().map(|x| x.duration).collect();
        durs.sort_unstable();
        let fd = durs.get(durs.len() / 2).copied().unwrap_or(1).max(1);
        // a muxer may shorten the last sample; anything else is variable rate
        if s.iter().filter(|x| x.duration != fd).count() > 1 {
            return Err(bad("variable frame rate video"));
        }
        frame_rate = Some((v.t.timescale, fd));
    }
    // the input stretch (ms) each piece covers
    let pieces_ms: Vec<(i64, i64)> = match &v {
        Some(v) => {
            let s = &v.t.samples;
            let ts = i64::from(v.t.timescale.max(1));
            let end = s
                .iter()
                .map(|x| x.pts + i64::from(x.duration))
                .max()
                .unwrap_or(0)
                - v.shift;
            let mut out = Vec::new();
            for &(d0, d1) in &ranges {
                let p0 = at(s, d0)?.pts - v.shift;
                let p1 = s.get(d1).map_or(end, |x| x.pts - v.shift);
                out.push((p0 * 1000 / ts, p1 * 1000 / ts));
            }
            out
        }
        None => {
            let mut sp: Vec<(i64, i64)> = spans_s
                .iter()
                .map(|(a, b)| ((a * 1000.0).floor() as i64, (b * 1000.0).ceil() as i64))
                .collect();
            sp.sort_unstable();
            let mut m: Vec<(i64, i64)> = Vec::new();
            for r in sp {
                match m.last_mut() {
                    Some(l) if r.0 <= l.1 => l.1 = l.1.max(r.1),
                    _ => m.push(r),
                }
            }
            m
        }
    };
    // audio packet ranges per piece: covering it, one packet either side, never overlapping
    let mut aranges: Vec<(usize, usize)> = Vec::new();
    let mut astarts: Vec<i64> = Vec::new();
    if let Some(a) = &a {
        let s = &a.t.samples;
        let rate = i64::from(a.t.timescale);
        // a decoder gives 1024 samples per AAC packet whatever the container's `stts`
        // says, and positions are counted that way (as the renderer's reader counts them)
        let mut pos = -a.shift;
        for _ in s {
            astarts.push(pos);
            pos += AAC_FRAME;
        }
        astarts.push(pos);
        let mut prev_end = 0usize;
        for &(t0, t1) in &pieces_ms {
            let (q0, q1) = (t0 * rate / 1000, (t1 * rate + 999).div_euclid(1000));
            let k0 = astarts.partition_point(|&p| p <= q0).saturating_sub(1);
            let k1 = astarts.partition_point(|&p| p < q1).min(s.len());
            let k0 = k0.saturating_sub(1).max(prev_end).min(s.len());
            let k1 = (k1 + 1).min(s.len()).max(k0);
            aranges.push((k0, k1));
            prev_end = k1;
        }
    }

    // the output's tracks and plan
    let mut cfgs = Vec::new();
    let mut counts = Vec::new();
    let mut payload = 0u64;
    if let Some(v) = &v {
        let colour =
            v.t.entry
                .colr
                .map(|(primaries, transfer, matrix, full_range)| ColorTags {
                    primaries,
                    transfer,
                    matrix,
                    full_range,
                });
        let mut cfg = TrackConfig::h264(
            v.t.entry.config.clone(),
            v.t.entry.width,
            v.t.entry.height,
            1,
            1,
            colour,
        );
        cfg.timescale = v.t.timescale;
        cfgs.push(cfg);
        let mut n = 0usize;
        for &(d0, d1) in &ranges {
            for x in v.t.samples.get(d0..d1).unwrap_or_default() {
                payload += u64::from(x.size);
                n += 1;
            }
        }
        counts.push(u32::try_from(n).map_err(|_| bad("too many video samples"))?);
    }
    let mut a_total = 0u64;
    if let Some(a) = &a {
        cfgs.push(TrackConfig::aac(
            a.t.entry.config.clone(),
            a.t.entry.channels,
            a.t.timescale,
        ));
        let mut n = 0usize;
        for &(k0, k1) in &aranges {
            for x in a.t.samples.get(k0..k1).unwrap_or_default() {
                payload += u64::from(x.size);
                a_total += AAC_FRAME as u64;
                n += 1;
            }
        }
        counts.push(u32::try_from(n).map_err(|_| bad("too many audio samples"))?);
    }
    let plan = MuxPlan {
        sample_counts: counts,
        max_payload_bytes: Some(payload + (1 << 20)),
        gapless: a.as_ref().map(|_| Gapless {
            priming: 0,
            padding: 0,
            length: a_total,
        }),
    };
    let (vi, ai) = (0usize, usize::from(v.is_some()));
    let tmp = out.with_extension("partial.mp4");
    let result = (|| -> Result<(Vec<Piece>, usize, usize, MuxReport), RemuxError> {
        let f = std::fs::File::create(&tmp).map_err(RemuxError::Write)?;
        let mut w = Mp4Writer::create(std::io::BufWriter::new(f), cfgs, &plan)?;
        let mut pieces = Vec::new();
        let (mut nv, mut na) = (0usize, 0usize);
        // the video tick shift of the current piece, and where the last piece ended
        let mut delta: Option<i64> = None;
        let mut prev_d1 = 0usize;
        let mut a_pos: i64 = 0;
        for (i, &(m0, m1)) in pieces_ms.iter().enumerate() {
            let mut piece = Piece {
                in_start_ms: m0,
                in_end_ms: m1,
                ..Piece::default()
            };
            let mut vs: Vec<(&SampleInfo, i64, i64)> = Vec::new();
            if let (Some(v), Some(&(d0, d1))) = (&v, ranges.get(i)) {
                let s = &v.t.samples;
                let first = at(s, d0)?;
                // decode times continue where the last piece ended: this piece's first
                // sample decodes where the sample after the last piece would have
                let d = match delta {
                    None => -(first.pts - v.shift),
                    Some(prev) => prev - (first.dts - at(s, prev_d1)?.dts),
                };
                delta = Some(d);
                prev_d1 = d1;
                let fd = i64::from(frame_rate.map_or(1, |r| r.1));
                let p0 = first.pts - v.shift;
                piece.in_frame0 = (p0 + fd / 2).div_euclid(fd);
                piece.out_frame0 = (p0 + d + fd / 2).div_euclid(fd);
                piece.frames = (d1 - d0) as u64;
                for x in s.get(d0..d1).unwrap_or_default() {
                    vs.push((x, x.dts - v.shift + d, x.pts - v.shift + d));
                }
            }
            let mut asamp: Vec<(&SampleInfo, i64)> = Vec::new();
            if let (Some(a), Some(&(k0, k1))) = (&a, aranges.get(i)) {
                piece.in_sample0 = astarts.get(k0).copied().unwrap_or(0);
                piece.out_sample0 = a_pos;
                for x in a.t.samples.get(k0..k1).unwrap_or_default() {
                    asamp.push((x, a_pos));
                    a_pos += AAC_FRAME;
                }
                piece.samples = u64::try_from(a_pos - piece.out_sample0).unwrap_or(0);
            }
            // interleave on output decode time
            let vts = i128::from(frame_rate.map_or(1, |r| r.0));
            let ats = i128::from(a.as_ref().map_or(1, |a| a.t.timescale));
            let (mut x, mut y) = (0usize, 0usize);
            while x < vs.len() || y < asamp.len() {
                let take_v = match (vs.get(x), asamp.get(y)) {
                    (Some(sv), Some(sa)) => i128::from(sv.1) * ats <= i128::from(sa.1) * vts,
                    (Some(_), None) => true,
                    _ => false,
                };
                if take_v {
                    let Some(&(sv, dts, pts)) = vs.get(x) else {
                        break;
                    };
                    w.write_sample(
                        vi,
                        &Sample {
                            data: sample_bytes(&file, sv)?,
                            dts,
                            pts,
                            duration: sv.duration,
                            sync: sv.sync,
                        },
                    )?;
                    nv += 1;
                    x += 1;
                } else {
                    let Some(&(sa, dts)) = asamp.get(y) else {
                        break;
                    };
                    w.write_sample(
                        ai,
                        &Sample {
                            data: sample_bytes(&file, sa)?,
                            dts,
                            pts: dts,
                            duration: AAC_FRAME as u32,
                            sync: true,
                        },
                    )?;
                    na += 1;
                    y += 1;
                }
            }
            pieces.push(piece);
        }
        let (_, mux) = w.finish()?;
        Ok((pieces, nv, na, mux))
    })();
    match result {
        Ok((pieces, nv, na, mux)) => {
            std::fs::rename(&tmp, out).map_err(RemuxError::Write)?;
            Ok(TrimReport {
                pieces,
                frame_rate,
                audio_rate: a.as_ref().map(|a| a.t.timescale),
                video_samples: nv,
                audio_samples: na,
                bytes_in: len,
                mux,
            })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_what_it_cannot_cut() {
        let dir = std::env::temp_dir().join(format!("gmclip-trim-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let not_mp4 = dir.join("x.mp4");
        let _ = std::fs::write(&not_mp4, b"not an mp4 at all");
        let out = dir.join("o.mp4");
        assert!(trim(&dir.join("missing.mp4"), &out, &[(0.0, 1.0)], true, true).is_err());
        assert!(trim(&not_mp4, &out, &[], true, true).is_err(), "no spans");
        assert!(
            trim(&not_mp4, &out, &[(2.0, 1.0)], true, true).is_err(),
            "end before start"
        );
        assert!(
            trim(&not_mp4, &out, &[(0.0, 1.0)], false, false).is_err(),
            "nothing to copy"
        );
        assert!(
            trim(&not_mp4, &out, &[(0.0, 1.0)], true, false).is_err(),
            "not an MP4"
        );
        assert!(!out.exists(), "nothing left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
