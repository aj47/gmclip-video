//! Edit-list application (`elst`, ISO/IEC 14496-12 §8.6.6): maps the media timeline
//! onto the presentation timeline and marks the samples that are decoded only as
//! reference (pre-roll) or lie past the edit.
//!
//! Supported at M0: any number of leading empty edits, then one media edit at rate 1
//! (trailing empty edits are ignored). That is every edit list in the corpus, the lab
//! sources and what ffmpeg and Apple write for trims, B-frame delay and AAC priming.
//! More than one media segment, or a rate other than 1, is `Unsupported` (M2).
//!
//! Where presentation starts and stops follows what the reference demuxer reports for
//! the same file (`tools/make_goldens.py`, `View::Edited`), established black-box
//! against the edit-list zoo in `test-media/` (no reference code was read):
//! - video starts at the last sync sample whose pts is at or before the edit's media
//!   time; the timeline is shifted so the first frame at or after the media time is at
//!   the edit's start; frames outside the edit are `discard`. Decoding continues to the
//!   first sync sample whose end reaches the edit's end — the second such one when the
//!   track has composition offsets, since trailing B-frames may still belong inside.
//! - audio starts with the first packet that ends less than one second (the track's
//!   timescale) before the media time, so a decoder can settle; the timeline is shifted
//!   by the media time exactly (a packet straddling it is kept, not discarded, and the
//!   decoder trims it); a packet wholly before or at/after the edit is `discard`;
//!   packets stop after the first one reaching the edit's end.
//!
//! All arithmetic is checked `i128` (R-MEDIA-SEC-2).

use super::super::{DemuxError, Packet, Result, Track, TrackKind};

fn to_i64(v: i128) -> Result<i64> {
    i64::try_from(v).map_err(|_| DemuxError::invalid("edit list timestamp overflows", 0))
}

/// Movie-timescale ticks to media-timescale ticks, rounded half up.
pub(crate) fn rescale(d: u64, movie: u32, media: u32) -> Result<i128> {
    if movie == 0 {
        return Err(DemuxError::invalid(
            "edit list with a zero movie timescale",
            0,
        ));
    }
    let num = i128::from(d)
        .checked_mul(i128::from(media))
        .and_then(|v| v.checked_add(i128::from(movie) / 2))
        .ok_or_else(|| DemuxError::invalid("edit duration overflows", 0))?;
    Ok(num / i128::from(movie))
}

/// Check at index time that every edit's arithmetic fits: rescaled durations, media
/// times and their sums stay inside `i64` (an overflowing `elst` is `Invalid`).
pub fn validate(edits: &[super::super::Edit], movie: u32, media: u32) -> Result<()> {
    let mut total: i128 = 0;
    for e in edits {
        let d = rescale(e.segment_duration, movie, media)?;
        total = total
            .checked_add(d)
            .ok_or_else(|| DemuxError::invalid("edit list overflows", 0))?;
        let reach = i128::from(e.media_time)
            .checked_add(d)
            .ok_or_else(|| DemuxError::invalid("edit overflows", 0))?;
        if i64::try_from(total).is_err()
            || i64::try_from(reach).is_err()
            || e.media_time > i64::MAX / 4
        {
            return Err(DemuxError::invalid("edit list overflows 64-bit time", 0));
        }
    }
    Ok(())
}

/// Apply the track's edit list. `raw(i)` is sample `i` as stored.
pub fn apply(track: &Track, raw: &dyn Fn(usize) -> Option<Packet>) -> Result<Vec<Packet>> {
    let n = track.samples.len();
    let edits = &track.edits;
    let mut i = 0;
    let mut empty: i128 = 0;
    while let Some(e) = edits.get(i).filter(|e| e.media_time == -1) {
        empty = empty
            .checked_add(rescale(
                e.segment_duration,
                track.movie_timescale,
                track.timescale,
            )?)
            .ok_or_else(|| DemuxError::invalid("empty edits overflow", 0))?;
        i += 1;
    }
    let media: Vec<_> = edits
        .iter()
        .skip(i)
        .filter(|e| e.media_time != -1)
        .collect();
    let edit = match media.as_slice() {
        [one] => *one,
        [] => return Err(DemuxError::unsupported("edit list with no media segment")),
        more => {
            return Err(DemuxError::unsupported(format!(
                "edit list with {} media segments (M2)",
                more.len()
            )));
        }
    };
    if edit.media_time < 0 {
        return Err(DemuxError::invalid(
            format!("edit media_time {}", edit.media_time),
            0,
        ));
    }
    if edit.rate != 0x0001_0000 {
        return Err(DemuxError::unsupported(format!(
            "edit rate {:#x} (M2)",
            edit.rate
        )));
    }
    let m = i128::from(edit.media_time);
    let end = if edit.segment_duration == 0 {
        i128::MAX / 4
    } else {
        rescale(
            edit.segment_duration,
            track.movie_timescale,
            track.timescale,
        )?
    };
    let get = |i: usize| raw(i).ok_or_else(|| DemuxError::invalid("ragged index", 0));
    let pts_of = |p: &Packet| i128::from(p.pts.unwrap_or(0));
    let dts_of = |p: &Packet| i128::from(p.dts.unwrap_or(0));
    let dur = |i: usize| i128::from(track.durations.get(i).copied().unwrap_or(0));
    let mut out = super::super::io::vec_with_capacity(n, 0)?;
    let mut push = |p: Packet, pts: i128, dts: i128, discard: bool| -> Result<()> {
        out.push(Packet {
            pts: Some(to_i64(
                pts.checked_add(empty)
                    .ok_or_else(|| DemuxError::invalid("pts overflow", 0))?,
            )?),
            dts: Some(to_i64(
                dts.checked_add(empty)
                    .ok_or_else(|| DemuxError::invalid("dts overflow", 0))?,
            )?),
            discard,
            ..p
        });
        Ok(())
    };
    if track.kind == TrackKind::Audio {
        let one_second = i128::from(track.timescale);
        let mut start = n;
        for i in 0..n {
            let p = get(i)?;
            if pts_of(&p) - m + dur(i) > -one_second {
                start = i;
                break;
            }
        }
        for i in start..n {
            let p = get(i)?;
            let rel = pts_of(&p) - m;
            let discard = rel + dur(i) <= 0 || rel >= end;
            push(p, rel, dts_of(&p) - m, discard)?;
            if rel + dur(i) >= end {
                break;
            }
        }
    } else {
        let mut start = 0;
        for i in 0..n {
            let p = get(i)?;
            if p.key && pts_of(&p) <= m {
                start = i;
            }
        }
        let mut shift = None::<i128>;
        for i in start..n {
            let pts = pts_of(&get(i)?);
            if pts >= m && shift.is_none_or(|s| pts < s) {
                shift = Some(pts);
            }
        }
        let shift = shift.unwrap_or(m);
        let has_cts = track.samples.cts_off.iter().any(|&c| c != 0);
        let mut ends_seen = 0;
        for i in start..n {
            let p = get(i)?;
            let rel = pts_of(&p) - shift;
            let discard = rel < 0 || rel >= end;
            push(p, rel, dts_of(&p) - shift, discard)?;
            if p.key && rel + dur(i) >= end {
                ends_seen += 1;
                if !has_cts || ends_seen == 2 {
                    break;
                }
            }
        }
    }
    Ok(out)
}
