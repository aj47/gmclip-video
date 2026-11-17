//! Motion tracking (decision 0031): a point of a source's picture followed frame by frame
//! with Apple Vision's object tracker (`VNTrackObjectRequest`, macOS). A square box
//! around the point is handed to Vision on the first frame; on each following frame the
//! box's centre is the point. Tracking stops where Vision's confidence drops below
//! [`LOST`] (the object left, or was covered). Windows says it is not there yet.
//!
//! The result is keys on the source's frames; an overlay stores them (`Overlay.track`), so
//! export and preview never need Vision.

use std::sync::atomic::{AtomicBool, Ordering};

use gmclip_plan::Plan;

use super::failed;
use super::mattes::open_stream;
use super::os::{Tracker, track_next};
use crate::export::Inputs;
use crate::ops::RenderError;

/// Below this confidence the object is lost and tracking stops.
pub const LOST: f32 = 0.3;

/// The box handed to Vision: this share of the picture's height, square in pixels.
pub const BOX: f64 = 0.15;

/// One tracked frame: the point (normalised picture coordinates, origin top left).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackSample {
    /// Presentation index in the source.
    pub frame: u64,
    pub x: f64,
    pub y: f64,
    pub confidence: f32,
}

/// What a run followed.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackRun {
    /// From the first frame on, in order.
    pub samples: Vec<TrackSample>,
    /// The frame where the object was lost, if it was.
    pub lost_at: Option<u64>,
    /// The source picture's size.
    pub size: (u32, u32),
}

/// The box around `point` (normalised) on a `w × h` picture: `size` of its height, square
/// in pixels, kept inside the picture.
pub fn start_box(point: [f64; 2], size: f64, w: u32, h: u32) -> [f64; 4] {
    let bh = size.clamp(0.01, 1.0);
    let bw = (bh * f64::from(h) / f64::from(w.max(1))).min(1.0);
    let x = (point[0] - bw / 2.0).clamp(0.0, 1.0 - bw);
    let y = (point[1] - bh / 2.0).clamp(0.0, 1.0 - bh);
    [x, y, bw, bh]
}

/// Follows `point` (normalised picture coordinates on frame `first`) through frames
/// `first..=last` of `source` (a plan source id) with a box of `size` of the picture's
/// height ([`BOX`] by default).
#[allow(clippy::too_many_arguments)]
pub fn track_point(
    plan: &Plan,
    inputs: &mut dyn Inputs,
    source: &str,
    first: u64,
    last: u64,
    point: [f64; 2],
    size: f64,
    cancel: Option<&AtomicBool>,
) -> Result<TrackRun, RenderError> {
    if last < first {
        return Err(failed(
            "track",
            None,
            "an empty span or a point off the picture",
        ));
    }
    track_spans(plan, inputs, source, &[(first, last)], point, size, cancel)
}

/// Frames of a source tracked straight through between two takes (5 s at 30 fps): a take
/// that starts at most this far after the previous one ends is reached by following the
/// frames between them, so the tracker never loses the thing it follows.
pub const GAP: u64 = 150;

/// Follows `point` (normalised, on the first span's first frame) through `spans` of
/// `source` (presentation-index ranges `first..=last`, in the order a cut plays them;
/// decision 0033, a track across takes). A span starting at most [`GAP`] frames after the
/// tracker's last frame is reached through the frames in between; any other (a take
/// further on, or earlier in the video) starts a new tracker where the point was last
/// seen. The samples come back in frame order, one per frame (the first seen wins).
/// Tracking stops at the first frame Vision loses the point.
#[allow(clippy::too_many_arguments)]
pub fn track_spans(
    plan: &Plan,
    inputs: &mut dyn Inputs,
    source: &str,
    spans: &[(u64, u64)],
    point: [f64; 2],
    size: f64,
    cancel: Option<&AtomicBool>,
) -> Result<TrackRun, RenderError> {
    if spans.is_empty() || spans.iter().any(|s| s.1 < s.0) || !point.iter().all(|v| v.is_finite()) {
        return Err(failed(
            "track",
            None,
            "an empty span or a point off the picture",
        ));
    }
    let dims = {
        let v = inputs.video(plan, source)?;
        (v.width, v.height)
    };
    let frame_err = |mut e: gmclip_media::MediaError| {
        e.source_id = Some(source.to_owned());
        RenderError::Media(e)
    };
    let mut samples: Vec<TrackSample> = Vec::new();
    let mut lost_at = None;
    let mut at = point;
    // the running tracker, its stream and the last frame it saw
    let mut run: Option<(_, Tracker, u64)> = None;
    'spans: for &(first, last) in spans {
        let resume = run
            .as_ref()
            .is_some_and(|r: &(_, Tracker, u64)| first > r.2 && first - r.2 <= GAP);
        if !resume {
            let mut stream = open_stream(inputs.video(plan, source)?)?;
            let mut tracker = Tracker::new(start_box(at, size, dims.0, dims.1))
                .map_err(|e| failed("track", Some(first), e))?;
            // the first frame is the start: Vision sees it first (the box is on it), so
            // the next frame's answer is a move, not the start again
            let pic = stream.frame(first).map_err(frame_err)?;
            track_next(&mut tracker, &pic.image, first)?;
            samples.push(TrackSample {
                frame: first,
                x: at[0],
                y: at[1],
                confidence: 1.0,
            });
            run = Some((stream, tracker, first));
        }
        let Some((stream, tracker, seen)) = run.as_mut() else {
            continue;
        };
        for f in *seen + 1..=last {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return Err(failed("cancelled", Some(f), "tracking cancelled"));
            }
            let pic = stream.frame(f).map_err(frame_err)?;
            let (b, conf) = track_next(tracker, &pic.image, f)?;
            *seen = f;
            if conf < LOST {
                lost_at = Some(f);
                break 'spans;
            }
            at = [b[0] + b[2] / 2.0, b[1] + b[3] / 2.0];
            samples.push(TrackSample {
                frame: f,
                x: at[0],
                y: at[1],
                confidence: conf,
            });
        }
    }
    samples.sort_by_key(|s| s.frame);
    samples.dedup_by_key(|s| s.frame);
    Ok(TrackRun {
        samples,
        lost_at,
        size: dims,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_start_box_is_square_in_pixels_and_inside() {
        let b = start_box([0.5, 0.5], 0.1, 1920, 1080);
        assert!((b[3] - 0.1).abs() < 1e-12);
        assert!((b[2] * 1920.0 - b[3] * 1080.0).abs() < 1e-9);
        assert!((b[0] + b[2] / 2.0 - 0.5).abs() < 1e-12);
        let c = start_box([0.0, 1.0], 0.1, 1920, 1080);
        assert_eq!((c[0], c[1] + c[3]), (0.0, 1.0));
    }
}
