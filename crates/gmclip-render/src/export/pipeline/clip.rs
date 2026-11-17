//! Moment clips by remux (H.10; SPEC §5.14.1): a plan whose every part shows the scrub
//! proxy full frame (H.264, GOP 4, no B-frames: `super::proxy`) is written without
//! decoding or encoding a picture — the proxy's coded samples are copied in order and
//! the sound is mixed and AAC-encoded exactly as an export does (`encode_audio`), then
//! muxed by the export's own sink. A 30 s clip costs its audio pass and a file copy.
//!
//! What it accepts (anything else is `Unsupported`, and the caller renders the plan
//! instead): every part's picture an H.264 source at the canvas size with no frame
//! reordering, starting on a sync sample, every part with the same `avcC`.

use std::path::Path;
use std::time::Instant;

use gmclip_media::backend::{EncodedVideoSample, SampleSource, VideoSampleSink};
use gmclip_media::index::SampleIdx;
use gmclip_media::mux::video_timescale;
use gmclip_plan::Plan;

use super::{MuxSink, encode_audio, failed};
use crate::export::{Inputs, VideoCodec};
use crate::ops::RenderError;

/// What a clip remux did.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipReport {
    pub frames: u64,
    pub bytes: u64,
    pub sha256: String,
    pub seconds: f64,
}

fn unsupported(reason: impl Into<String>) -> RenderError {
    RenderError::Unsupported {
        step: "clip_remux",
        reason: reason.into(),
    }
}

/// Writes `plan` to `out` by copying its pictures' coded samples (see the module docs).
pub fn remux_clip(
    plan: &Plan,
    inputs: &mut dyn Inputs,
    out: &Path,
) -> Result<ClipReport, RenderError> {
    let t0 = Instant::now();
    if let Some(e) = gmclip_plan::check_plan(plan).into_iter().next() {
        return Err(RenderError::Plan(e));
    }
    let (cw, ch) = (
        plan.output.canvas.w.get() as u32,
        plan.output.canvas.h.get() as u32,
    );
    let (fnum, fden) = plan.output.fps.rate();
    let (fps, fps_den) = (fnum as u32, fden as u32);
    // the pictures first: refuse before any audio work
    let mut pieces = Vec::with_capacity(plan.parts.len());
    let mut avcc: Option<Vec<u8>> = None;
    for p in &plan.parts {
        if p.layers.len() != 1 || p.layers[0].picture_source.is_some() {
            return Err(unsupported("a part with other layers than its own picture"));
        }
        if p.layers[0].frame.is_some() {
            return Err(unsupported("a framed layer (decision 0046)"));
        }
        if p.speed.is_some() {
            return Err(unsupported("a part at another speed (decision 0030)"));
        }
        if p.picture.fps.is_some() {
            return Err(unsupported(
                "a picture at another rate (rate conversion re-encodes)",
            ));
        }
        let v = inputs.video(plan, p.picture.source.as_str())?;
        let VideoCodec::H264 { avcc: a } = &v.codec else {
            return Err(unsupported("not H.264"));
        };
        if (v.width, v.height) != (cw, ch) {
            return Err(unsupported("the picture is not the canvas size"));
        }
        match &avcc {
            None => avcc = Some(a.clone()),
            Some(have) if have != a => {
                return Err(unsupported("parts with different parameter sets"));
            }
            _ => {}
        }
        let idx = v.source.index();
        let (f0, n) = (p.picture.first_frame, p.out.len());
        let end = f0
            .checked_add(n)
            .filter(|&e| e <= idx.len() as u64)
            .ok_or_else(|| unsupported("a part past its picture's end"))?;
        if !(f0..end).all(|f| idx.pts_order[f as usize] as u64 == f) {
            return Err(unsupported("frame reordering (B-frames)"));
        }
        if !idx.sync[f0 as usize] {
            return Err(unsupported("a part that does not start on a sync sample"));
        }
        pieces.push((v.source, f0, end));
    }
    let avcc = avcc.ok_or_else(|| unsupported("no parts"))?;
    let mut notes = Vec::new();
    let audio = encode_audio(plan, inputs, &mut notes, false)?;
    let partial = out.with_extension("partial.mp4");
    let ts = video_timescale(fps, fps_den);
    let tick = i64::from(ts) * i64::from(fps_den) / i64::from(fps.max(1));
    let mut sink = MuxSink {
        path: partial.clone(),
        writer: None,
        audio: &audio,
        next_audio: 0,
        audio_pts: 0,
        width: cw as u16,
        height: ch as u16,
        fps,
        fps_den,
        frames: plan.output.frames,
        video_ts: i64::from(ts),
        video_written: 0,
    };
    let r = (|| -> Result<(), RenderError> {
        sink.video_config(&avcc)?;
        let mut buf = Vec::new();
        let mut k = 0i64;
        for (src, f0, end) in &mut pieces {
            for f in *f0..*end {
                src.read(SampleIdx(f as u32), &mut buf)?;
                let sync = src.index().sync[f as usize];
                sink.video_sample(&EncodedVideoSample {
                    data: &buf,
                    pts: k * tick,
                    dts: k * tick,
                    duration: tick as u32,
                    sync,
                })?;
                k += 1;
            }
        }
        Ok(())
    })();
    if let Err(e) = r {
        drop(sink);
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }
    let path = sink.finish().map_err(|e| {
        let _ = std::fs::remove_file(&partial);
        RenderError::Media(e)
    })?;
    let bytes = std::fs::read(&path).map_err(|e| failed("clip_read", None, e))?;
    std::fs::rename(&path, out).map_err(|e| failed("clip_install", None, e))?;
    Ok(ClipReport {
        frames: plan.output.frames,
        bytes: bytes.len() as u64,
        sha256: crate::sha256::hex(&bytes),
        seconds: t0.elapsed().as_secs_f64(),
    })
}
