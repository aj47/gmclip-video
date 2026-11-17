//! The scrub proxy (H.05; SPEC §6.3, §13): every frame of a source re-encoded by the OS
//! encoder at ≤ 640 px wide, GOP 4, no B-frames, so any frame of it decodes from at most
//! three others. Scrubbing reads it; playback and paused (exact) frames never do.
//!
//! - **Decode** goes through the export's decoder recipe on its own thread, in
//!   presentation order, at the source's size.
//! - **Encode** is the export's H.264 encoder (High, CABAC, closed GOP of 4 forced
//!   on the frame grid, no frame reordering) into lane B's `Mp4Writer`. The compression
//!   session is made at the proxy size and VideoToolbox scales each source picture as
//!   it takes it — no compositor pass, no CPU copy. (Measured on (a) and (b): scaling in
//!   the encoder is 1.3× faster than asking the decoder for scaled output, which slows
//!   the VP9 decoder to half speed, and the bytes are identical.) Media Foundation's
//!   encoder (Windows) does not scale: each picture is box-downscaled on the CPU first.
//! - **Layout** is one MP4 per segment of [`SEG_FRAMES`] source frames
//!   (`seg-00000.mp4`, …) plus `proxy.json`: the proxy covers the playhead as soon as
//!   that segment is written (a 2 h source is usable from the first minute), a build
//!   killed half way resumes at its next segment, and frame `n` of the source is frame
//!   `n mod SEG_FRAMES` of segment `n / SEG_FRAMES` (one proxy picture per source
//!   picture, in presentation order, so the mapping is by index even for VFR sources).
//! - **Keyed** by [`content_key`]: a BLAKE3 over the file's size and three 1 MiB
//!   samples (head, middle, tail) — a renamed or moved file keeps its proxy, an edited
//!   one gets a new one, and keying a 3 GB file costs three reads, not a full hash.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gmclip_media::MediaError;
use gmclip_media::backend::{EncodedVideoSample, FrameStream, VideoSampleSink};
use gmclip_media::decode::DecodedFrame;
use gmclip_media::mux::{ColorTags, Mp4Writer, MuxPlan, TrackConfig, video_timescale};
use serde::{Deserialize, Serialize};

use super::os::{AnyDecoder, EncoderConfig, H264Encoder, cpu_planes, encode_picture, recipe};
use super::{Src, failed};
use crate::export::DemuxInputs;
use crate::ops::RenderError;

/// Widest proxy picture (SPEC §6.3).
pub const MAX_W: u32 = 640;
/// Keyframe interval in frames (SPEC §6.3: GOP 4, no B-frames).
pub const GOP: u32 = 4;
/// Source frames per proxy segment (a minute at 30 fps; a multiple of [`GOP`], so the
/// keyframe grid is global: every frame index divisible by 4 is a proxy keyframe).
pub const SEG_FRAMES: u64 = 1800;
/// `proxy.json`'s schema.
pub const SCHEMA: &str = "gmclip.proxy/0";
/// Average bitrate of the proxy (bits/s): ≈ 270 MB per hour of source.
pub const BITRATE: u32 = 600_000;

/// `proxy.json`: what the folder holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: String,
    /// [`content_key`] of the source.
    pub key: String,
    pub source_frames: u64,
    pub source_w: u32,
    pub source_h: u32,
    pub w: u32,
    pub h: u32,
    pub fps: u32,
    pub gop: u32,
    pub seg_frames: u64,
    /// Segments written, contiguous from 0.
    pub segments_done: u64,
    pub complete: bool,
}

impl Manifest {
    /// Source frames `[0, covered)` have proxy pictures.
    pub fn covered(&self) -> u64 {
        (self.segments_done * self.seg_frames).min(self.source_frames)
    }

    pub fn segments(&self) -> u64 {
        self.source_frames.div_ceil(self.seg_frames.max(1))
    }

    /// Reads `<dir>/proxy.json`.
    pub fn read(dir: &Path) -> Option<Manifest> {
        let b = std::fs::read(dir.join("proxy.json")).ok()?;
        let m: Manifest = serde_json::from_slice(&b).ok()?;
        (m.schema == SCHEMA && m.seg_frames > 0 && m.gop > 0).then_some(m)
    }

    fn write(&self, dir: &Path) -> Result<(), RenderError> {
        let tmp = dir.join("proxy.json.tmp");
        let body = serde_json::to_vec_pretty(self)
            .map_err(|e| failed("proxy_manifest", None, e.to_string()))?;
        std::fs::write(&tmp, body).map_err(|e| failed("proxy_manifest", None, e))?;
        std::fs::rename(&tmp, dir.join("proxy.json")).map_err(|e| failed("proxy_manifest", None, e))
    }
}

/// Segment `i`'s file.
pub fn segment_path(dir: &Path, i: u64) -> PathBuf {
    dir.join(format!("seg-{i:05}.mp4"))
}

/// The proxy picture size for a `w × h` source: ≤ [`MAX_W`] wide, the source's aspect,
/// even sides, never larger than the source.
pub fn proxy_size(w: u32, h: u32) -> (u32, u32) {
    let even = |v: f64| ((v.round() as u32) & !1).max(2);
    if w == 0 || h == 0 {
        return (2, 2);
    }
    let s = (f64::from(MAX_W) / f64::from(w)).min(1.0);
    (even(f64::from(w) * s), even(f64::from(h) * s))
}

/// The source's content key: BLAKE3 over its size and 1 MiB at its head, middle and
/// tail, as 32 hex characters.
pub fn content_key(path: &Path) -> std::io::Result<String> {
    const CHUNK: u64 = 1 << 20;
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    let mut buf = Vec::with_capacity(3 * CHUNK as usize + 32);
    buf.extend_from_slice(b"gmclip.proxy-key/0");
    buf.extend_from_slice(&size.to_le_bytes());
    for at in [
        0,
        (size / 2).saturating_sub(CHUNK / 2),
        size.saturating_sub(CHUNK),
    ] {
        f.seek(SeekFrom::Start(at))?;
        let mut chunk = Vec::with_capacity(CHUNK as usize);
        (&mut f).take(CHUNK).read_to_end(&mut chunk)?;
        buf.extend_from_slice(&chunk);
    }
    let h = gmclip_media::export_record::blake3::hash(&buf);
    Ok(h[..16].iter().map(|b| format!("{b:02x}")).collect())
}

/// Called with the manifest after each segment is written.
pub type OnSegment = Arc<dyn Fn(&Manifest) + Send + Sync>;

/// Build options.
#[derive(Clone, Default)]
pub struct BuildOptions {
    /// Set to stop; checked every frame. A stopped build keeps its finished segments.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Source frames covered so far (the manager's progress).
    pub progress: Option<Arc<AtomicU64>>,
    /// Stop once the proxy covers this many source frames (whole segments; tests and
    /// measurements). `None`: the whole source.
    pub until: Option<u64>,
    /// After each segment (the manager's progress events).
    pub on_segment: Option<OnSegment>,
}

impl std::fmt::Debug for BuildOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuildOptions")
            .field("until", &self.until)
            .finish_non_exhaustive()
    }
}

/// What a build did.
#[derive(Clone, Debug, PartialEq)]
pub struct BuildReport {
    pub frames: u64,
    pub encoded: u64,
    pub seconds: f64,
    pub resumed_at: u64,
    pub bytes: u64,
    pub size: (u32, u32),
    pub cancelled: bool,
}

/// One segment's MP4, written as samples arrive.
struct SegSink {
    path: PathBuf,
    writer: Option<Mp4Writer<BufWriter<File>>>,
    size: (u16, u16),
    fps: u32,
    frames: u32,
    written: u64,
}

fn mux_err(step: &'static str, frame: Option<u64>) -> MediaError {
    MediaError {
        step,
        frame,
        source_id: None,
        os_status: None,
    }
}

impl VideoSampleSink for SegSink {
    fn video_config(&mut self, avcc: &[u8]) -> Result<(), MediaError> {
        let file = File::create(&self.path).map_err(|_| mux_err("proxy_create", None))?;
        let tracks = vec![TrackConfig::h264(
            avcc.to_vec(),
            self.size.0,
            self.size.1,
            self.fps,
            1,
            Some(ColorTags::BT709_LIMITED),
        )];
        let plan = MuxPlan {
            sample_counts: vec![self.frames],
            max_payload_bytes: None,
            gapless: None,
        };
        self.writer = Some(
            Mp4Writer::create(BufWriter::new(file), tracks, &plan)
                .map_err(|_| mux_err("proxy_create", None))?,
        );
        Ok(())
    }

    fn video_sample(&mut self, s: &EncodedVideoSample<'_>) -> Result<(), MediaError> {
        let w = self
            .writer
            .as_mut()
            .ok_or(mux_err("proxy_no_writer", None))?;
        w.write_sample(
            0,
            &gmclip_media::mux::Sample {
                data: s.data,
                dts: s.dts,
                pts: s.pts,
                duration: s.duration,
                sync: s.sync,
            },
        )
        .map_err(|_| mux_err("proxy_mux", Some(self.written)))?;
        self.written += 1;
        Ok(())
    }
}

/// The source's nominal integer frame rate (the proxy's timestamps; frames map by
/// index, so a VFR source only gets evenly spaced proxy timestamps).
fn nominal_fps(idx: &gmclip_media::SampleIndex) -> u32 {
    if let Some(r) = idx.cfr
        && r.den > 0
    {
        return ((r.num as f64 / r.den as f64).round() as u32).clamp(1, 240);
    }
    let n = idx.len();
    if n < 2 || idx.timescale == 0 {
        return 30;
    }
    let pts = |i: usize| idx.dts[i] + i64::from(idx.cts_off[i]);
    let (lo, hi) = (0..n).fold((i64::MAX, i64::MIN), |(a, b), i| {
        (a.min(pts(i)), b.max(pts(i)))
    });
    let secs = (hi - lo) as f64 / f64::from(idx.timescale);
    if secs <= 0.0 {
        return 30;
    }
    (((n - 1) as f64 / secs).round() as u32).clamp(1, 240)
}

fn encoder_config(w: u32, h: u32, fps: u32) -> EncoderConfig {
    let mut c = EncoderConfig::reels(w, h);
    c.fps = fps;
    c.timescale = video_timescale(fps, 1) as i32;
    c.level = super::fitting_level(w, h, fps);
    c.average_bitrate = BITRATE;
    c.max_bitrate = BITRATE * 4;
    c.max_window_s = 1.0;
    c.gop = GOP;
    c.b_frames = false;
    c
}

/// Builds (or resumes) the proxy of `source` in `dir` (created if missing). `key` is the
/// source's [`content_key`]; a folder holding another key's proxy is cleared first.
pub fn build(
    source: &Path,
    dir: &Path,
    key: &str,
    opts: &BuildOptions,
) -> Result<BuildReport, RenderError> {
    let t0 = Instant::now();
    std::fs::create_dir_all(dir).map_err(|e| failed("proxy_dir", None, e))?;
    let mut inputs = DemuxInputs::new(None);
    let v = inputs.video_file(source, "proxy")?;
    let (sw, sh) = (v.width, v.height);
    let (pw, ph) = proxy_size(sw, sh);
    let frames = v.source.index().len() as u64;
    let fps = nominal_fps(v.source.index());
    let fresh = Manifest {
        schema: SCHEMA.into(),
        key: key.into(),
        source_frames: frames,
        source_w: sw,
        source_h: sh,
        w: pw,
        h: ph,
        fps,
        gop: GOP,
        seg_frames: SEG_FRAMES,
        segments_done: 0,
        complete: false,
    };
    let mut m = match Manifest::read(dir) {
        Some(old)
            if Manifest {
                segments_done: 0,
                complete: false,
                ..old.clone()
            } == fresh =>
        {
            old
        }
        _ => {
            // another key's (or an older layout's) proxy: start over
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let n = e.file_name();
                    let n = n.to_string_lossy();
                    if n.starts_with("seg-") || n.starts_with("proxy.json") {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
            fresh.write(dir)?;
            fresh
        }
    };
    let resumed_at = m.covered();
    if let Some(p) = &opts.progress {
        p.store(resumed_at, Ordering::Relaxed);
    }
    let mut report = BuildReport {
        frames,
        encoded: 0,
        seconds: 0.0,
        resumed_at,
        bytes: 0,
        size: (pw, ph),
        cancelled: false,
    };
    if m.complete {
        report.seconds = t0.elapsed().as_secs_f64();
        return Ok(report);
    }
    let (r, src) = recipe(v.source, &v.codec, sw, sh)?;
    let mut stream: FrameStream<Src, AnyDecoder> = FrameStream::new(src, move || r.make());
    let cancelled = || {
        opts.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    };
    // the frames this run encodes: whole segments from the first missing one
    let first = m.covered();
    let last_seg = match opts.until {
        Some(u) => u
            .div_ceil(SEG_FRAMES)
            .max(m.segments_done)
            .min(m.segments()),
        None => m.segments(),
    };
    let end = (last_seg * SEG_FRAMES).min(frames);
    // Decode on its own thread, a few pictures ahead of the encoder.
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Arc<DecodedFrame>, MediaError>>(8);
    let decoder = {
        let stop = stop.clone();
        std::thread::Builder::new()
            .name("gmclip-proxy-decode".into())
            .spawn(move || {
                for i in first..end {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let r = stream.frame(i);
                    let failed = r.is_err();
                    if tx.send(r).is_err() || failed {
                        return;
                    }
                }
            })
            .map_err(|e| failed("proxy_decode", None, e))?
    };
    let result = (|| -> Result<(), RenderError> {
        let ts_per_frame = 512i64; // video_timescale(fps, 1) / fps
        for seg in m.segments_done..last_seg {
            let f0 = seg * SEG_FRAMES;
            let f1 = (f0 + SEG_FRAMES).min(frames);
            let path = segment_path(dir, seg);
            let partial = dir.join(format!("seg-{seg:05}.mp4.partial"));
            let mut sink = SegSink {
                path: partial.clone(),
                writer: None,
                size: (pw as u16, ph as u16),
                fps,
                frames: (f1 - f0) as u32,
                written: 0,
            };
            let mut enc = H264Encoder::new(&encoder_config(pw, ph, fps))?;
            for i in f0..f1 {
                if cancelled() {
                    drop(sink);
                    let _ = std::fs::remove_file(&partial);
                    report.cancelled = true;
                    return Ok(());
                }
                let fr = rx
                    .recv()
                    .map_err(|_| failed("proxy_decode", Some(i), "the decoder stopped"))?
                    .map_err(RenderError::Media)?;
                encode_picture(
                    &mut enc,
                    &fr.image,
                    (i - f0) as i64 * ts_per_frame,
                    ts_per_frame,
                    &mut sink,
                )
                .ok_or_else(|| failed("proxy_decode", Some(i), "decoder returned CPU planes"))??;
                report.encoded += 1;
            }
            enc.finish(&mut sink)?;
            let w = sink
                .writer
                .take()
                .ok_or_else(|| failed("proxy_mux", None, "no samples"))?;
            let (mut file, _) = w
                .finish()
                .map_err(|e| failed("proxy_mux", None, e.to_string()))?;
            file.flush().map_err(|e| failed("proxy_mux", None, e))?;
            drop(file);
            std::fs::rename(&partial, &path).map_err(|e| failed("proxy_install", None, e))?;
            report.bytes += std::fs::metadata(&path).map_or(0, |md| md.len());
            m.segments_done = seg + 1;
            m.complete = m.segments_done >= m.segments();
            m.write(dir)?;
            if let Some(p) = &opts.progress {
                p.store(m.covered(), Ordering::Relaxed);
            }
            if let Some(cb) = &opts.on_segment {
                cb(&m);
            }
        }
        Ok(())
    })();
    stop.store(true, Ordering::Relaxed);
    drop(rx);
    let _ = decoder.join();
    result?;
    report.seconds = t0.elapsed().as_secs_f64();
    Ok(report)
}

/// Reads a proxy folder (possibly still being built): pictures by source frame index.
pub struct ProxyReader {
    dir: PathBuf,
    manifest: Manifest,
    checked: Instant,
    inputs: DemuxInputs,
    /// Open segments, most recent last (at most 2).
    open: Vec<(u64, FrameStream<Src, AnyDecoder>)>,
}

impl std::fmt::Debug for ProxyReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyReader")
            .field("dir", &self.dir)
            .field("covered", &self.manifest.covered())
            .finish_non_exhaustive()
    }
}

impl ProxyReader {
    /// `None` when the folder has no proxy yet.
    pub fn open(dir: &Path) -> Option<ProxyReader> {
        let manifest = Manifest::read(dir)?;
        Some(ProxyReader {
            dir: dir.to_path_buf(),
            manifest,
            checked: Instant::now(),
            inputs: DemuxInputs::new(None),
            open: Vec::new(),
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The proxy picture size.
    pub fn size(&self) -> (u32, u32) {
        (self.manifest.w, self.manifest.h)
    }

    /// Whether source frame `n` has a proxy picture (re-reading `proxy.json` at most
    /// every 500 ms while the build is running).
    pub fn covers(&mut self, n: u64) -> bool {
        if n < self.manifest.covered() {
            return true;
        }
        if !self.manifest.complete && self.checked.elapsed() >= Duration::from_millis(500) {
            self.checked = Instant::now();
            if let Some(m) = Manifest::read(&self.dir)
                && m.key == self.manifest.key
            {
                self.manifest = m;
            }
        }
        n < self.manifest.covered()
    }

    /// The proxy picture of source frame `n` (must be covered).
    pub fn frame(&mut self, n: u64) -> Result<Arc<DecodedFrame>, RenderError> {
        if !self.covers(n) {
            return Err(failed("proxy_frame", Some(n), "not covered yet"));
        }
        let seg = n / self.manifest.seg_frames;
        let k = match self.open.iter().position(|(s, _)| *s == seg) {
            Some(k) => k,
            None => {
                let path = segment_path(&self.dir, seg);
                let v = self.inputs.video_file(&path, "proxy")?;
                let (r, src) = recipe(v.source, &v.codec, v.width, v.height)?;
                if self.open.len() >= 2 {
                    self.open.remove(0);
                }
                self.open
                    .push((seg, FrameStream::new(src, move || r.make())));
                self.open.len() - 1
            }
        };
        let local = n - seg * self.manifest.seg_frames;
        self.open[k].1.frame(local).map_err(RenderError::Media)
    }

    /// The proxy keyframe at or before source frame `n` (decodes from one sample).
    pub fn keyframe_of(&self, n: u64) -> u64 {
        n - n % u64::from(self.manifest.gop.max(1))
    }

    /// An RGB thumbnail of source frame `n` fitting `w × 9w/16` (a portrait source gets a
    /// narrower one), read at its proxy keyframe (one decode): `(w, h, rgb)`.
    pub fn thumbnail(&mut self, n: u64, w: u32) -> Result<(u32, u32, Vec<u8>), RenderError> {
        let (pw, ph) = self.size();
        let max_h = u64::from((w * 9 / 16).max(2));
        let w = (u64::from(w)).min(max_h * u64::from(pw) / u64::from(ph.max(1))) as u32;
        let k = self.keyframe_of(n);
        let fr = self.frame(k)?;
        let p = cpu_planes(&fr.image)
            .ok_or_else(|| failed("thumbnail", Some(n), "decoder returned CPU planes"))?
            .map_err(|e| failed("thumbnail", Some(n), e.to_string()))?;
        Ok(planes_thumbnail(
            &p.y,
            &p.uv,
            p.format.bytes_per_sample(),
            p.width,
            p.height,
            w,
        ))
    }
}

/// An RGB thumbnail of frame `n` of a video file (the master, when it has no proxy yet),
/// `w` wide at most within `w × 9w/16`: one decode from the keyframe before `n` through
/// the export's decoder recipe. Frames past the end read the last one.
pub fn file_thumbnail(path: &Path, n: u64, w: u32) -> Result<(u32, u32, Vec<u8>), RenderError> {
    let mut inputs = DemuxInputs::new(None);
    let v = inputs.video_file(path, "thumbnail")?;
    let (pw, ph) = (v.width, v.height);
    let frames = v.source.index().dts.len() as u64;
    let (r, src) = recipe(v.source, &v.codec, v.width, v.height)?;
    let mut stream = FrameStream::new(src, move || r.make());
    let fr = stream
        .frame(n.min(frames.saturating_sub(1)))
        .map_err(RenderError::Media)?;
    let p = cpu_planes(&fr.image)
        .ok_or_else(|| failed("thumbnail", Some(n), "decoder returned CPU planes"))?
        .map_err(|e| failed("thumbnail", Some(n), e.to_string()))?;
    let max_h = u64::from((w * 9 / 16).max(2));
    let w = (u64::from(w)).min(max_h * u64::from(pw) / u64::from(ph.max(1))) as u32;
    Ok(planes_thumbnail(
        &p.y,
        &p.uv,
        p.format.bytes_per_sample(),
        p.width,
        p.height,
        w,
    ))
}

/// Keyframe thumbnails of a video file with no proxy yet (the timeline's filmstrip until
/// the proxy covers it, lane K5): the file is demuxed and its decoder recipe made once and
/// kept open; each thumbnail is the master's sync sample at or before the frame, one
/// decode from a fresh chain, so a 3 h recording shows a filmstrip before any proxy
/// exists. Frames past the end read the last keyframe.
pub struct MasterThumbs {
    reader: gmclip_media::backend::frames::FrameReader<Src, AnyDecoder>,
    size: (u32, u32),
}

impl std::fmt::Debug for MasterThumbs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MasterThumbs")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl MasterThumbs {
    pub fn open(path: &Path) -> Result<MasterThumbs, RenderError> {
        let mut inputs = DemuxInputs::new(None);
        let v = inputs.video_file(path, "thumbnail")?;
        let size = (v.width, v.height);
        let (r, src) = recipe(v.source, &v.codec, v.width, v.height)?;
        Ok(MasterThumbs {
            reader: gmclip_media::backend::frames::FrameReader::new(src, move || r.make()),
            size,
        })
    }

    /// The file's frames (presentation order).
    pub fn frames(&self) -> u64 {
        self.reader.source().index().pts_order.len() as u64
    }

    /// The decode index of the keyframe frame `n` is shown from (a cache key).
    pub fn keyframe_of(&self, n: u64) -> Option<u64> {
        let idx = self.reader.source().index();
        let last = idx.pts_order.len().checked_sub(1)?;
        let t = *idx.pts_order.get((n as usize).min(last))?;
        gmclip_media::backend::seek::seek_start(idx, gmclip_media::index::SampleIdx(t))
            .map(|k| k as u64)
    }

    /// An RGB thumbnail of the keyframe at or before frame `n`, `w` wide at most within
    /// `w × 9w/16` (a portrait source gets a narrower one): `(w, h, rgb)`.
    pub fn thumbnail(&mut self, n: u64, w: u32) -> Result<RgbPicture, RenderError> {
        let k = self
            .keyframe_of(n)
            .ok_or_else(|| failed("thumbnail", Some(n), "no keyframe before the frame"))?
            as usize;
        let idx = self.reader.source().index();
        let pts = idx.dts[k] + i64::from(idx.cts_off[k]);
        let fr = self.reader.frame_at_pts(pts).map_err(RenderError::Media)?;
        let p = cpu_planes(&fr.image)
            .ok_or_else(|| failed("thumbnail", Some(n), "decoder returned CPU planes"))?
            .map_err(|e| failed("thumbnail", Some(n), e.to_string()))?;
        let (pw, ph) = self.size;
        let max_h = u64::from((w * 9 / 16).max(2));
        let w = (u64::from(w)).min(max_h * u64::from(pw) / u64::from(ph.max(1))) as u32;
        Ok(planes_thumbnail(
            &p.y,
            &p.uv,
            p.format.bytes_per_sample(),
            p.width,
            p.height,
            w,
        ))
    }
}

/// `(w, h, rgb)`: an RGB picture.
pub type RgbPicture = (u32, u32, Vec<u8>);

/// RGB frames of a video file at about `every_s` seconds apart over its first `max_s`
/// seconds, each `w` wide at most (the source's aspect): `(fps of the file, frames)`.
/// One sequential decode through the export's decoder recipe (the caption matcher reads
/// a dropped short this way).
pub fn file_frames(
    path: &Path,
    every_s: f64,
    max_s: f64,
    w: u32,
) -> Result<(f64, Vec<RgbPicture>), RenderError> {
    let mut inputs = DemuxInputs::new(None);
    let v = inputs.video_file(path, "frames")?;
    let idx = v.source.index();
    let n = idx.dts.len() as u64;
    let ts = f64::from(idx.timescale.max(1));
    let (lo, hi) = (
        idx.dts.iter().min().copied().unwrap_or(0),
        idx.dts.iter().max().copied().unwrap_or(0),
    );
    let fps = if hi > lo && n > 1 {
        (n - 1) as f64 * ts / (hi - lo) as f64
    } else {
        30.0
    };
    let step = ((fps * every_s).round() as u64).max(1);
    let last = n.min((fps * max_s).ceil() as u64);
    let (r, src) = recipe(v.source, &v.codec, v.width, v.height)?;
    let mut stream = FrameStream::new(src, move || r.make());
    let mut out = Vec::new();
    let mut f = 0;
    while f < last {
        let fr = stream.frame(f).map_err(RenderError::Media)?;
        let p = cpu_planes(&fr.image)
            .ok_or_else(|| failed("frames", Some(f), "decoder returned CPU planes"))?
            .map_err(|e| failed("frames", Some(f), e.to_string()))?;
        out.push(planes_thumbnail(
            &p.y,
            &p.uv,
            p.format.bytes_per_sample(),
            p.width,
            p.height,
            w.min(p.width),
        ));
        f += step;
    }
    Ok((fps, out))
}

/// [`nv12_thumbnail`] of decoded planes of any depth: 16-bit samples (P010, the 10 bits in
/// the high bits, little-endian) are narrowed to their high byte first, so a 10-bit
/// master's thumbnails and frames read right (they read 8-bit before, and came out noise).
fn planes_thumbnail(
    y: &[u8],
    uv: &[u8],
    bytes_per_sample: usize,
    w: u32,
    h: u32,
    tw: u32,
) -> RgbPicture {
    if bytes_per_sample == 2 {
        let hi = |b: &[u8]| b.chunks_exact(2).map(|c| c[1]).collect::<Vec<u8>>();
        nv12_thumbnail(&hi(y), &hi(uv), w, h, tw)
    } else {
        nv12_thumbnail(y, uv, w, h, tw)
    }
}

/// Box-filtered BT.709 limited-range NV12 → RGB at `tw` wide (the source's aspect).
pub fn nv12_thumbnail(y: &[u8], uv: &[u8], w: u32, h: u32, tw: u32) -> (u32, u32, Vec<u8>) {
    let tw = tw.clamp(2, w.max(2));
    let th = ((u64::from(h) * u64::from(tw) / u64::from(w.max(1))) as u32).max(2);
    let (w, h) = (w as usize, h as usize);
    let (tw_, th_) = (tw as usize, th as usize);
    let mut out = Vec::with_capacity(tw_ * th_ * 3);
    for ty in 0..th_ {
        let (y0, y1) = (ty * h / th_, ((ty + 1) * h / th_).max(ty * h / th_ + 1));
        for tx in 0..tw_ {
            let (x0, x1) = (tx * w / tw_, ((tx + 1) * w / tw_).max(tx * w / tw_ + 1));
            let (mut sy, mut su, mut sv, mut n, mut nc) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for yy in y0..y1.min(h) {
                for xx in x0..x1.min(w) {
                    sy += u32::from(y.get(yy * w + xx).copied().unwrap_or(16));
                    n += 1;
                    if yy % 2 == 0 && xx % 2 == 0 {
                        let c = (yy / 2) * w + (xx / 2) * 2;
                        su += u32::from(uv.get(c).copied().unwrap_or(128));
                        sv += u32::from(uv.get(c + 1).copied().unwrap_or(128));
                        nc += 1;
                    }
                }
            }
            let yv = (sy as f32 / n.max(1) as f32 - 16.0) * (255.0 / 219.0);
            let (cb, cr) = if nc > 0 {
                (
                    (su as f32 / nc as f32 - 128.0) * (255.0 / 224.0),
                    (sv as f32 / nc as f32 - 128.0) * (255.0 / 224.0),
                )
            } else {
                (0.0, 0.0)
            };
            let r = yv + 1.5748 * cr;
            let g = yv - 0.1873 * cb - 0.4681 * cr;
            let b = yv + 1.8556 * cb;
            for c in [r, g, b] {
                out.push(c.round().clamp(0.0, 255.0) as u8);
            }
        }
    }
    (tw, th, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(proxy_size(2560, 1440), (640, 360));
        assert_eq!(proxy_size(1920, 1080), (640, 360));
        assert_eq!(proxy_size(1080, 1920), (640, 1138));
        assert_eq!(proxy_size(480, 270), (480, 270));
        assert_eq!(proxy_size(641, 361), (640, 360));
        assert_eq!(SEG_FRAMES % u64::from(GOP), 0);
    }

    #[test]
    fn a_10_bit_frame_thumbnails_like_its_8_bit_twin() {
        let (w, h) = (32u32, 18u32);
        // a gradient in Y, warm chroma: 8-bit planes and the same picture in P010
        let y8: Vec<u8> = (0..w * h).map(|i| 16 + (i % 200) as u8).collect();
        let uv8: Vec<u8> = (0..w * h / 2)
            .map(|i| if i % 2 == 0 { 100 } else { 170 })
            .collect();
        let wide = |b: &[u8]| {
            b.iter()
                .flat_map(|&v| (u16::from(v) << 8).to_le_bytes())
                .collect::<Vec<u8>>()
        };
        let a = planes_thumbnail(&y8, &uv8, 1, w, h, 16);
        let b = planes_thumbnail(&wide(&y8), &wide(&uv8), 2, w, h, 16);
        assert_eq!(a, b);
    }

    #[test]
    fn thumbnail_of_grey_is_grey() {
        let (w, h) = (64u32, 36u32);
        let y = vec![126u8; (w * h) as usize];
        let uv = vec![128u8; (w * h / 2) as usize];
        let (tw, th, rgb) = nv12_thumbnail(&y, &uv, w, h, 16);
        assert_eq!((tw, th), (16, 9));
        assert_eq!(rgb.len(), 16 * 9 * 3);
        assert!(rgb.iter().all(|&v| (127..=129).contains(&v)), "{rgb:?}");
    }

    #[test]
    fn content_key_is_stable_and_content_sensitive() {
        let d = std::env::temp_dir().join(format!("gmclip-proxy-key-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let a = d.join("a.bin");
        std::fs::write(&a, vec![7u8; 3 << 20]).unwrap();
        let k1 = content_key(&a).unwrap();
        assert_eq!(k1.len(), 32);
        let b = d.join("b.bin");
        std::fs::copy(&a, &b).unwrap();
        assert_eq!(content_key(&b).unwrap(), k1, "a copy keeps its key");
        let mut v = vec![7u8; 3 << 20];
        v[(3 << 20) / 2] = 8;
        std::fs::write(&b, v).unwrap();
        assert_ne!(content_key(&b).unwrap(), k1, "the middle sample changed");
        let _ = std::fs::remove_dir_all(&d);
    }
}
