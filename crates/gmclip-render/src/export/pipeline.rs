//! The export pipeline, shared by macOS and Windows: the plan's pictures decoded
//! frame-exactly, composited on the GPU, converted to NV12 and encoded, the mix encoded
//! once, both muxed and verified. The OS pieces (decoders, picture import, NV12 output,
//! H.264 and AAC encoders, segmentation) are `os`: VideoToolbox, Metal, AudioToolbox
//! and Vision on macOS (`os_apple`); Media Foundation and a GPU readback on Windows
//! (`os_windows`).

use std::collections::HashMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use gmclip_gpu::{GpuContext, GpuError, GpuOptions};
use gmclip_media::MediaError;
use gmclip_media::audio::graph::{AudioPlan, LoudnessMode, PcmSource};
use gmclip_media::audio::resample::Resampler;
use gmclip_media::audio::{loudness, priming};
use gmclip_media::backend::aac::{AacRandomReader, AacTrackDecoder};
use gmclip_media::backend::frames::FrameReader;
use gmclip_media::backend::pcm::PcmRandomReader;
use gmclip_media::backend::{
    EncodedVideoSample, FrameStream, RATE_CAP_LADDER, SOCIAL_PEAK_BPS, SampleSource,
    VideoSampleSink,
};
use gmclip_media::decode::DecodedFrame;
use gmclip_media::export_record::{
    DurationExpected, ExportRecord, LoudnessBefore, LoudnessRecord, install_export, render_sig_of,
};
use gmclip_media::mux::{ColorTags, Mp4Writer, MuxPlan, TrackConfig, video_timescale};
use gmclip_plan::Plan;

use super::{ExportOptions, ExportReport, Inputs};
use crate::compositor::{ChromaSiting, ComposeError};
use crate::ops::RenderError;

type Src = Box<dyn SampleSource + Send>;

#[cfg(target_os = "macos")]
mod os_apple;
#[cfg(target_os = "macos")]
use os_apple as os;
#[cfg(windows)]
mod os_windows;
use os::{
    AacEncoder, AnyDecoder, EncoderConfig, H264Encoder, H264Level, Nv12Output, OutFrame, recipe,
};
#[cfg(windows)]
use os_windows as os;

/// The preview path (lane U): the same per-frame compositor at the displayed size.
pub mod preview;

/// The scrub proxy (H.05): ≤ 640 px, GOP 4, no B-frames, by segment.
pub mod proxy;

/// Moment clips by remux of the scrub proxy (H.10).
pub mod clip;

/// The per-frame compose shared by export, `render_frames` and the preview (H.04).
mod compose;
/// Roto mattes: Vision mask tracks and per-frame mask resolution (R.01).
pub mod mattes;
/// Motion tracking: a point followed through a source with Apple Vision (decision 0031).
pub mod track;
use compose::{ExportPictures, FrameComposer};

fn failed(step: &'static str, frame: Option<u64>, reason: impl ToString) -> RenderError {
    RenderError::Failed {
        step,
        frame,
        reason: reason.to_string(),
    }
}

fn gpu(step: &'static str, frame: Option<u64>) -> impl FnOnce(GpuError) -> RenderError {
    move |e| match e.os_status() {
        Some(s) => RenderError::Media(MediaError {
            step,
            frame,
            source_id: None,
            os_status: Some(s),
        }),
        None => failed(step, frame, e),
    }
}

/// A picture that could not be read or decoded (R-EXP-4): the media step that failed, at
/// the output frame being made, with the source frame and the OS status as the reason
/// (the plan's source id, never a path).
fn decode_failed(e: MediaError, source: &str, frame: u64, out_frame: u64) -> RenderError {
    let os = e
        .os_status
        .map(|s| format!(", OSStatus {s}"))
        .unwrap_or_default();
    failed(
        e.step,
        Some(out_frame),
        format!("source {source} frame {frame}{os}"),
    )
}

fn compose_err(frame: u64) -> impl FnOnce(ComposeError) -> RenderError {
    move |e| match e {
        ComposeError::Unsupported(r) => RenderError::Unsupported {
            step: "compose",
            reason: r,
        },
        ComposeError::Text(r) => failed("text", Some(frame), r),
        other => failed("compose", Some(frame), other),
    }
}

/// Decoded frames by (source, presentation index): a forward stream per source for
/// live layers, a cache (filled by a separate seeking reader) for still layers.
type Stream = FrameStream<Src, AnyDecoder>;

/// A spare forward stream for one source (the preview's join preroll, SPEC §6.3: "preroll
/// a second session ≥ 2 joins ahead"): idle, or decoding up to a part's first frame on
/// its own thread. When playback reaches that frame the spare becomes the source's
/// stream (already positioned in the new chain) and the old stream becomes a spare.
enum Spare {
    Idle(Box<Stream>),
    Busy {
        frame: u64,
        job: std::thread::JoinHandle<(Stream, Result<Arc<DecodedFrame>, MediaError>)>,
    },
}

struct Pictures {
    /// Decode streams by stream key ([`crate::compositor::LayerDraw::stream`]: the source
    /// id for the parts' layers; one apart for the backdrop and the clip events).
    streams: HashMap<String, Stream>,
    /// Spare streams per stream key (preview only; none for export).
    spares: HashMap<String, Vec<Spare>>,
    /// Each stream key's source.
    stream_source: HashMap<String, String>,
    still_readers: HashMap<String, FrameReader<Src, AnyDecoder>>,
    stills: HashMap<(String, u64), Arc<DecodedFrame>>,
    meta: HashMap<String, (u32, u32, ChromaSiting)>,
    /// Each source's presentation clock, for parts that convert its rate (decision 0017).
    clocks: HashMap<String, gmclip_plan::retime::PtsClock>,
}

impl crate::compositor::SourceClocks for Pictures {
    fn clock(&self, source: &str) -> Option<&gmclip_plan::retime::PtsClock> {
        self.clocks.get(source)
    }
}

/// A sample index's presentation clock (PTS in presentation order).
fn pts_clock(idx: &gmclip_media::index::SampleIndex) -> gmclip_plan::retime::PtsClock {
    gmclip_plan::retime::PtsClock::new(
        u64::from(idx.timescale),
        idx.pts_order
            .iter()
            .map(|&i| idx.dts[i as usize] + i64::from(idx.cts_off[i as usize])),
    )
}

impl Pictures {
    fn open(plan: &Plan, inputs: &mut dyn Inputs) -> Result<Self, RenderError> {
        let mut me = Self {
            streams: HashMap::new(),
            spares: HashMap::new(),
            stream_source: HashMap::new(),
            still_readers: HashMap::new(),
            stills: HashMap::new(),
            meta: HashMap::new(),
            clocks: HashMap::new(),
        };
        for (key, id) in crate::compositor::plan_streams(plan) {
            let v = inputs.video(plan, &id)?;
            let (r, src) = recipe(v.source, &v.codec, v.width, v.height)?;
            me.meta.insert(id.clone(), (v.width, v.height, v.siting));
            let r2 = r.clone();
            let mut st = FrameStream::new(src, move || r2.make());
            st.set_lookahead(r.lookahead());
            // every source's (a preview keeps its pictures across plans of the same
            // sources, and a later plan may convert one this plan does not)
            me.clocks
                .entry(id.clone())
                .or_insert_with(|| pts_clock(st.source().index()));
            me.streams.insert(key.clone(), st);
            me.stream_source.insert(key, id.clone());
            if me.still_readers.contains_key(&id) {
                continue;
            }
            // Still frames: a second, seeking reader on its own file handle.
            let v2 = inputs.video(plan, &id)?;
            let r3 = r.clone();
            me.still_readers
                .insert(id.clone(), FrameReader::new(v2.source, move || r3.make()));
        }
        Ok(me)
    }

    /// Like [`Pictures::open`], with `n` spare streams per source for join preroll.
    fn open_with_spares(
        plan: &Plan,
        inputs: &mut dyn Inputs,
        n: usize,
    ) -> Result<Self, RenderError> {
        let mut me = Self::open(plan, inputs)?;
        let keys: Vec<(String, String)> = me
            .stream_source
            .iter()
            .map(|(k, s)| (k.clone(), s.clone()))
            .collect();
        for (key, id) in keys {
            let mut v = Vec::new();
            for _ in 0..n {
                let vi = inputs.video(plan, &id)?;
                let (r, src) = recipe(vi.source, &vi.codec, vi.width, vi.height)?;
                let ahead = r.lookahead();
                let mut st = FrameStream::new(src, move || r.make());
                st.set_lookahead(ahead);
                v.push(Spare::Idle(Box::new(st)));
            }
            me.spares.insert(key, v);
        }
        Ok(me)
    }

    /// Starts decoding each of `frames` of stream `source` (the first frames of the next
    /// joins) on a spare stream, unless one already is or none is free. Finished jobs for
    /// frames no longer wanted are reclaimed first; wanted ones keep their picture.
    fn preroll(&mut self, source: &str, frames: &[u64]) {
        let Some(spares) = self.spares.get_mut(source) else {
            return;
        };
        for sp in std::mem::take(spares) {
            match sp {
                Spare::Busy { frame: f, job } if !frames.contains(&f) && job.is_finished() => {
                    if let Ok((st, _)) = job.join() {
                        spares.push(Spare::Idle(Box::new(st)));
                    }
                }
                other => spares.push(other),
            }
        }
        for &frame in frames {
            if spares
                .iter()
                .any(|sp| matches!(sp, Spare::Busy { frame: f, .. } if *f == frame))
            {
                continue;
            }
            let Some(k) = spares.iter().position(|sp| matches!(sp, Spare::Idle(_))) else {
                return;
            };
            let Spare::Idle(st) = spares.swap_remove(k) else {
                return;
            };
            let mut st = *st;
            let job = std::thread::Builder::new()
                .name("gmclip-preroll".into())
                .spawn(move || {
                    let r = st.frame(frame);
                    (st, r)
                });
            if let Ok(job) = job {
                spares.push(Spare::Busy { frame, job });
            }
        }
    }

    /// Samples a fresh decode chain must decode to show `frame` of `source` (from the
    /// sync sample before it), or `None` when unknown.
    fn chain_len(&self, source: &str, frame: u64) -> Option<u64> {
        let idx = self.streams.get(source)?.source().index();
        let t = *idx.pts_order.get(frame as usize)? as usize;
        let k = (0..=t)
            .rev()
            .find(|&i| idx.sync.get(i).copied().unwrap_or(false))?;
        Some((t - k + 1) as u64)
    }

    /// A prerolled picture for `(source, frame)`: the spare becomes the source's stream.
    fn take_prerolled(
        &mut self,
        source: &str,
        frame: u64,
    ) -> Option<Result<Arc<DecodedFrame>, MediaError>> {
        let spares = self.spares.get_mut(source)?;
        let k = spares
            .iter()
            .position(|sp| matches!(sp, Spare::Busy { frame: f, .. } if *f == frame))?;
        let Spare::Busy { job, .. } = spares.swap_remove(k) else {
            return None;
        };
        let (st, r) = job.join().ok()?;
        let old = self.streams.insert(source.to_owned(), st)?;
        spares.push(Spare::Idle(Box::new(old)));
        Some(r)
    }

    /// The picture for a layer: frame `frame` of `source`, read on stream `key`
    /// ([`crate::compositor::LayerDraw::stream`]).
    fn get(
        &mut self,
        key: &str,
        source: &str,
        frame: u64,
        still: bool,
        out_frame: u64,
    ) -> Result<Arc<DecodedFrame>, RenderError> {
        if !still && let Some(r) = self.take_prerolled(key, frame) {
            return r.map_err(|e| decode_failed(e, source, frame, out_frame));
        }
        if still {
            let key = (source.to_owned(), frame);
            if let Some(f) = self.stills.get(&key) {
                return Ok(f.clone());
            }
            let r = self
                .still_readers
                .get_mut(source)
                .ok_or_else(|| failed("compose", Some(out_frame), "unknown source"))?;
            let idx = r.source().index();
            let s = *idx
                .pts_order
                .get(frame as usize)
                .ok_or_else(|| failed("decode", Some(out_frame), "hold frame past the source"))?;
            let pts = idx.dts[s as usize] + i64::from(idx.cts_off[s as usize]);
            let f = Arc::new(r.frame_at_pts(pts).map_err(|mut e| {
                e.source_id = Some(source.to_owned());
                RenderError::Media(e)
            })?);
            self.stills.insert(key, f.clone());
            return Ok(f);
        }
        let s = self
            .streams
            .get_mut(key)
            .ok_or_else(|| failed("compose", Some(out_frame), "unknown source"))?;
        s.frame(frame)
            .map_err(|e| decode_failed(e, source, frame, out_frame))
    }

    fn seeks(&self) -> u64 {
        self.streams.values().map(|s| s.seeks).sum()
    }
}

/// Every source the plan's mix hears, once each, in plan order: the parts' own audio,
/// the sources under them and the overlays' (decision 0051).
pub fn heard_sources(plan: &Plan) -> Vec<&str> {
    let mut v: Vec<&str> = Vec::new();
    let parts = plan.parts.iter().flat_map(|p| {
        std::iter::once(p.audio.source.as_str())
            .chain(p.audio.under.iter().flatten().map(|u| u.source.as_str()))
    });
    let overlays = plan
        .audio
        .overlays
        .iter()
        .flatten()
        .map(|o| o.source.as_str());
    for id in parts.chain(overlays) {
        if !v.contains(&id) {
            v.push(id);
        }
    }
    v
}

/// A source's decoder: AAC through the OS, or PCM read as stored (decision 0051).
enum Decoded {
    Aac(AacRandomReader<Src>),
    Pcm(PcmRandomReader<Src>),
}

impl Decoded {
    fn read(&mut self, first: u64, out: &mut [f32]) -> Result<(), String> {
        match self {
            Self::Aac(r) => r.read(first, out),
            Self::Pcm(r) => r.read(first, out),
        }
        .map_err(|e| e.to_string())
    }
}

/// One heard source as the mix reads it: 48 kHz stereo, a mono source spread to both
/// channels, a source at another rate converted on the way (decision 0051).
struct SourceAudio {
    dec: Decoded,
    channels: usize,
    resample: Option<Resampler>,
    scratch: Vec<f32>,
}

impl SourceAudio {
    fn read(&mut self, first: u64, out: &mut [f32]) -> Result<(), String> {
        let ch = self.channels;
        if ch == 2 && self.resample.is_none() {
            return self.dec.read(first, out);
        }
        self.scratch.resize(out.len() / 2 * ch, 0.0);
        let dec = &mut self.dec;
        match &mut self.resample {
            None => dec.read(first, &mut self.scratch)?,
            Some(r) => r.read(ch, first, &mut self.scratch, &mut |a, b| dec.read(a, b))?,
        }
        if ch == 2 {
            out.copy_from_slice(&self.scratch);
        } else {
            for (o, &m) in out.chunks_exact_mut(2).zip(&self.scratch) {
                o[0] = m;
                o[1] = m;
            }
        }
        Ok(())
    }
}

/// The mix graph's sample source, shared by the export and playback (so the two mixes
/// are one): one random-access reader per heard source.
pub struct PlanPcm {
    /// `None`: a source playback could not open, read as silence.
    readers: HashMap<String, Option<SourceAudio>>,
}

impl std::fmt::Debug for PlanPcm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanPcm")
            .field("sources", &self.readers.len())
            .finish_non_exhaustive()
    }
}

impl PlanPcm {
    /// Opens every source the plan's mix hears. The mix is 48 kHz stereo: mono sources
    /// are spread to both channels, other rates converted (decision 0051), other layouts
    /// refused.
    pub fn open(plan: &Plan, inputs: &mut dyn Inputs) -> Result<Self, RenderError> {
        let mut readers = HashMap::new();
        for id in heard_sources(plan) {
            readers.insert(id.to_owned(), Some(Self::reader(plan, inputs, id)?));
        }
        Ok(Self { readers })
    }

    /// [`Self::open`] for playback: a source this build cannot read plays as silence.
    pub fn open_lenient(plan: &Plan, inputs: &mut dyn Inputs) -> Self {
        let readers = heard_sources(plan)
            .into_iter()
            .map(|id| (id.to_owned(), Self::reader(plan, inputs, id).ok()))
            .collect();
        Self { readers }
    }

    fn reader(plan: &Plan, inputs: &mut dyn Inputs, id: &str) -> Result<SourceAudio, RenderError> {
        let a = inputs.audio(plan, id)?;
        if !(a.channels == 1 || a.channels == 2) || a.sample_rate == 0 {
            return Err(RenderError::Unsupported {
                step: "audio",
                reason: format!(
                    "source {id}: {} Hz × {} ch (the mix takes mono or stereo)",
                    a.sample_rate, a.channels
                ),
            });
        }
        let dec = match a.pcm {
            Some(f) => Decoded::Pcm(
                PcmRandomReader::new(a.source, f, a.channels, a.skip).with_lead(a.lead),
            ),
            None => Decoded::Aac(
                AacRandomReader::new(a.source, &a.asc, a.sample_rate, a.channels, a.skip)?
                    .with_lead(a.lead),
            ),
        };
        Ok(SourceAudio {
            dec,
            channels: a.channels as usize,
            resample: (a.sample_rate != 48_000).then(|| Resampler::new(a.sample_rate)),
            scratch: Vec::new(),
        })
    }
}

impl PcmSource for PlanPcm {
    fn read(&mut self, source: &str, first_sample: u64, out: &mut [f32]) -> Result<(), String> {
        match self.readers.get_mut(source) {
            Some(Some(r)) => r.read(first_sample, out),
            Some(None) => {
                out.fill(0.0);
                Ok(())
            }
            None => Err("audio source not opened".to_owned()),
        }
    }
}

/// The plan's voice mix read in ranges (captions heard from the cut's own audio, SPEC
/// §5.10): part gain, fades and envelopes as the export mixes them, before the delivery
/// loudness pass.
pub struct VoiceMix {
    ap: AudioPlan,
    pcm: PlanPcm,
}

impl std::fmt::Debug for VoiceMix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceMix")
            .field("total", &self.ap.total)
            .finish_non_exhaustive()
    }
}

impl VoiceMix {
    pub fn open(plan: &Plan, inputs: &mut dyn Inputs) -> Result<Self, RenderError> {
        let ap = AudioPlan::from_plan(plan)?;
        let pcm = PlanPcm::open(plan, inputs)?;
        Ok(Self { ap, pcm })
    }

    /// Output samples in the plan.
    pub fn total(&self) -> u64 {
        self.ap.total
    }

    /// Mixes output samples `[from, from + out.len() / 2)` (interleaved stereo, 48 kHz).
    pub fn read(&mut self, from: u64, out: &mut [f32]) -> Result<(), RenderError> {
        self.ap
            .mix_range(&mut self.pcm, from, out)
            .map_err(|e| failed("audio_mix", None, e))
    }
}

/// The encoded audio track: packets, durations, the ASC and the cut length.
struct AudioTrackOut {
    packets: Vec<Vec<u8>>,
    durations: Vec<u32>,
    asc: Vec<u8>,
    samples: u64,
    loudness: LoudnessRecord,
    /// The mix as encoded (after loudness), for verify's join checks.
    mix: Vec<f32>,
    plan: AudioPlan,
    /// The mix before loudness, hashed per block (when asked; `super::mix_block_hashes`).
    mix_hashes: Vec<String>,
}

fn encode_audio(
    plan: &Plan,
    inputs: &mut dyn Inputs,
    notes: &mut Vec<String>,
    hash: bool,
) -> Result<AudioTrackOut, RenderError> {
    let ap = AudioPlan::from_plan(plan)?;
    let mut pcm = PlanPcm::open(plan, inputs)?;
    let mut mix = ap.mix(&mut pcm).map_err(|e| failed("audio_mix", None, e))?;
    let mix_hashes = if hash {
        super::mix_block_hashes(&mix)
    } else {
        Vec::new()
    };
    let bitrate = plan.output.audio.bitrate.unwrap_or(192_000) as u32;
    let samples = ap.total;
    let durations = priming::packet_durations(samples);
    let raw_mix = mix.clone();
    // Static loudness, then a check of the *delivered* true peak: AAC encoding raises
    // it (measured +0.1 dB on h25, up to ~1 dB on hot material). When the decoded
    // stream exceeds the ceiling + 0.1 dB that verify allows, the limiter runs again
    // with a lower ceiling (audio only; the video pass has not started).
    let mut margin = 0.0f64;
    let mut attempt = 0;
    let (loudness, packets, asc) = loop {
        attempt += 1;
        mix.copy_from_slice(&raw_mix);
        let (loudness, ceiling) = match ap.loudness {
            LoudnessMode::Static {
                integrated_lufs,
                true_peak_dbtp,
            } => {
                let r = loudness::normalize_static(
                    &mut mix,
                    2,
                    48_000,
                    integrated_lufs,
                    true_peak_dbtp - margin,
                );
                let rec = LoudnessRecord {
                    integrated: r
                        .after
                        .integrated_lufs
                        .is_finite()
                        .then_some(r.after.integrated_lufs),
                    true_peak: r
                        .after
                        .true_peak_dbtp
                        .is_finite()
                        .then_some(r.after.true_peak_dbtp),
                    before: Some(LoudnessBefore {
                        integrated: r
                            .before
                            .integrated_lufs
                            .is_finite()
                            .then_some(r.before.integrated_lufs),
                        true_peak: r
                            .before
                            .true_peak_dbtp
                            .is_finite()
                            .then_some(r.before.true_peak_dbtp),
                    }),
                    gain_db: Some(r.gain_db),
                    target_reached: Some(r.reached),
                };
                (
                    rec,
                    Some((
                        integrated_lufs,
                        true_peak_dbtp,
                        r.reached,
                        r.after.integrated_lufs,
                    )),
                )
            }
            LoudnessMode::AsRecorded => {
                let m = loudness::measure(&mix, 2, 48_000);
                (
                    LoudnessRecord {
                        integrated: m.integrated_lufs.is_finite().then_some(m.integrated_lufs),
                        true_peak: m.true_peak_dbtp.is_finite().then_some(m.true_peak_dbtp),
                        before: None,
                        gain_db: None,
                        target_reached: None,
                    },
                    None,
                )
            }
        };
        let mut enc =
            AacEncoder::new(48_000, 2, bitrate).map_err(gpu("aac_encoder_create", None))?;
        let asc = os::aac_asc(&enc)
            .ok_or_else(|| failed("aac_encoder", None, "no AudioSpecificConfig"))?;
        let feed = priming::encoder_feed(&mix, 2, enc.priming() as usize, 48_000);
        enc.push(&feed);
        let mut packets = enc.pull(true).map_err(gpu("aac_encode", None))?;
        if packets.len() < durations.len() {
            return Err(failed(
                "aac_encode",
                None,
                format!("{} packets for {} needed", packets.len(), durations.len()),
            ));
        }
        packets.truncate(durations.len());
        let Some((target, ceiling_db, reached, after)) = ceiling else {
            break (loudness, packets, asc);
        };
        let mut dec = AacTrackDecoder::new(&asc, 48_000, 2, 0, Some(samples))?;
        let mut decoded = Vec::with_capacity(mix.len());
        for p in &packets {
            dec.decode(p, &mut decoded)?;
        }
        dec.finish(&mut decoded)?;
        let tp = loudness::true_peak(&decoded, 2);
        let excess = tp - (ceiling_db + 0.1);
        if excess <= -0.02 || attempt >= 4 {
            if !reached {
                notes.push(format!(
                    "loudness target {target} LUFS not reached: {after:.2} LUFS"
                ));
            }
            if margin > 0.0 {
                notes.push(format!(
                    "true peak: limiter ceiling lowered by {margin:.2} dB so the delivered AAC reads {tp:.2} dBTP (ceiling {ceiling_db})"
                ));
            }
            if excess > -0.02 {
                notes.push(format!(
                    "delivered true peak {tp:.2} dBTP above the ceiling after {attempt} attempts"
                ));
            }
            break (loudness, packets, asc);
        }
        margin += excess + 0.05;
    };
    Ok(AudioTrackOut {
        packets,
        durations,
        asc,
        samples,
        loudness,
        mix,
        plan: ap,
        mix_hashes,
    })
}

/// Writes video samples as they come and interleaves the audio packets by time.
struct MuxSink<'a> {
    path: PathBuf,
    writer: Option<Mp4Writer<BufWriter<File>>>,
    audio: &'a AudioTrackOut,
    next_audio: usize,
    audio_pts: i64,
    width: u16,
    height: u16,
    /// The output rate `fps / fps_den` (1001 for an NTSC rate).
    fps: u32,
    fps_den: u32,
    frames: u64,
    video_ts: i64,
    video_written: u64,
}

impl MuxSink<'_> {
    fn write_audio_until(&mut self, video_dts: Option<i64>) -> Result<(), MediaError> {
        let w = self.writer.as_mut().ok_or(mux_err("mux_no_writer"))?;
        while self.next_audio < self.audio.packets.len() {
            // Audio up to the video sample's decode time (48 kHz vs video ticks).
            if let Some(d) = video_dts
                && i128::from(self.audio_pts) * i128::from(self.video_ts)
                    > i128::from(d.max(0)) * 48_000
            {
                break;
            }
            let k = self.next_audio;
            let dur = self.audio.durations[k];
            w.write_sample(
                1,
                &gmclip_media::mux::Sample {
                    data: &self.audio.packets[k],
                    dts: self.audio_pts,
                    pts: self.audio_pts,
                    duration: dur,
                    sync: true,
                },
            )
            .map_err(|_| mux_err("mux_audio"))?;
            self.audio_pts += i64::from(dur);
            self.next_audio += 1;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<PathBuf, MediaError> {
        self.write_audio_until(None)?;
        let w = self.writer.take().ok_or(mux_err("mux_no_video"))?;
        w.finish().map_err(|_| mux_err("mux_finish"))?;
        Ok(self.path)
    }
}

fn mux_err(step: &'static str) -> MediaError {
    MediaError {
        step,
        frame: None,
        source_id: None,
        os_status: None,
    }
}

impl VideoSampleSink for MuxSink<'_> {
    fn video_config(&mut self, avcc: &[u8]) -> Result<(), MediaError> {
        let file = File::create(&self.path).map_err(|_| mux_err("mux_create"))?;
        let tracks = vec![
            TrackConfig::h264(
                avcc.to_vec(),
                self.width,
                self.height,
                self.fps,
                self.fps_den,
                Some(ColorTags::BT709_LIMITED),
            ),
            TrackConfig::aac(self.audio.asc.clone(), 2, 48_000),
        ];
        let plan = MuxPlan {
            sample_counts: vec![self.frames as u32, self.audio.packets.len() as u32],
            max_payload_bytes: None,
            gapless: Some(priming::gapless(self.audio.samples)),
        };
        self.writer = Some(
            Mp4Writer::create(BufWriter::new(file), tracks, &plan)
                .map_err(|_| mux_err("mux_create"))?,
        );
        Ok(())
    }

    fn video_sample(&mut self, s: &EncodedVideoSample<'_>) -> Result<(), MediaError> {
        self.write_audio_until(Some(s.dts))?;
        let w = self.writer.as_mut().ok_or(mux_err("mux_no_writer"))?;
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
        .map_err(|_| MediaError {
            step: "mux_video",
            frame: Some(self.video_written),
            source_id: None,
            os_status: None,
        })?;
        self.video_written += 1;
        Ok(())
    }
}

/// The 1-s peak an export may not pass (SPEC §6.5.1): 25 Mbit/s for the social presets
/// (a portrait or square canvas); an upload preset only its level's MaxBR (High profile,
/// Table A-1 × 1.25, [`gmclip_plan::validate::level_max_bps`]).
fn peak_cap(plan: &Plan) -> u64 {
    let (w, h) = (plan.output.canvas.w.get(), plan.output.canvas.h.get());
    if w <= h {
        return SOCIAL_PEAK_BPS;
    }
    plan.output
        .video
        .level
        .as_deref()
        .and_then(gmclip_plan::validate::level_max_bps)
        .unwrap_or(SOCIAL_PEAK_BPS)
        .max(SOCIAL_PEAK_BPS)
}

/// Opens the H.264 encoder at `cfg`. An upload rate above the social 25 Mbit/s that the
/// OS encoder refuses (a Media Foundation encoder may cap its bitrate) is retried once at
/// the social rate, 24 Mbit/s average and 25 peak at the same level, and the export
/// record says so; a refusal there, or of a rate that was not lifted, is an error that
/// names the size, rate and level (decision 0027).
pub(crate) fn open_encoder<E>(
    cfg: &EncoderConfig,
    mut open: impl FnMut(&EncoderConfig) -> Result<E, MediaError>,
) -> Result<(E, Option<String>), RenderError> {
    let what = |c: &EncoderConfig| {
        format!(
            "{}×{} at {} fps, {:.0} Mbit/s (peak {:.0}), level {:?}",
            c.width,
            c.height,
            c.fps,
            f64::from(c.average_bitrate) / 1e6,
            f64::from(c.max_bitrate) / 1e6,
            c.level
        )
    };
    let refused = |c: &EncoderConfig, e: &MediaError| {
        failed(
            "encoder_open",
            None,
            format!(
                "this computer's H.264 encoder refused {} ({}{}); export at a lower preset (YouTube 1080p) instead",
                what(c),
                e.step,
                e.os_status
                    .map(|s| format!(", status {s}"))
                    .unwrap_or_default()
            ),
        )
    };
    match open(cfg) {
        Ok(e) => Ok((e, None)),
        Err(e) if u64::from(cfg.max_bitrate) > SOCIAL_PEAK_BPS => {
            let mut lower = cfg.clone();
            lower.average_bitrate = lower.average_bitrate.min(24_000_000);
            lower.max_bitrate = SOCIAL_PEAK_BPS as u32;
            match open(&lower) {
                Ok(enc) => Ok((
                    enc,
                    Some(format!(
                        "encoder: refused {} ({}); encoded at {:.0} Mbit/s (peak {:.0})",
                        what(cfg),
                        e.step,
                        f64::from(lower.average_bitrate) / 1e6,
                        f64::from(lower.max_bitrate) / 1e6
                    )),
                )),
                Err(e2) => Err(refused(&lower, &e2)),
            }
        }
        Err(e) => Err(refused(cfg, &e)),
    }
}

fn encoder_config(plan: &Plan, min_qp: Option<u8>) -> EncoderConfig {
    let (w, h) = (
        plan.output.canvas.w.get() as u32,
        plan.output.canvas.h.get() as u32,
    );
    let (num, den) = plan.output.fps.rate();
    // the session's rate hints are whole frames per second (30 for 29.97); timestamps
    // are exact on the output's own timescale (1001 ticks a frame at NTSC rates)
    let fps = plan.output.fps.rounded() as u32;
    let mut c = EncoderConfig::reels(w, h);
    c.fps = fps;
    c.timescale = video_timescale(num as u32, den as u32) as i32;
    let asked = match plan.output.video.level.as_deref() {
        Some("5.1") => H264Level::High51,
        Some("4.2") => H264Level::High42,
        _ if fps > 30 => H264Level::High42,
        _ => H264Level::High40,
    };
    // a canvas past the asked level's MaxFS or MaxMBPS (a 2160-tall portrait) is
    // refused by the encoder, so the level rises to the first one that holds it
    let fits = fitting_level(w, h, fps);
    c.level = if level_rank(fits) > level_rank(asked) {
        fits
    } else {
        asked
    };
    if let Some(r) = &plan.output.video.rate {
        if let Some(t) = r.target_bps {
            c.average_bitrate = t as u32;
        }
        if let Some(m) = r.max_bps {
            c.max_bitrate = m as u32;
        }
    }
    if let Some(g) = plan.output.video.gop_frames {
        c.gop = g as u32;
    } else {
        c.gop = fps; // closed GOP of 1 s (SPEC §6.5.1)
    }
    c.b_frames = plan.output.video.bframes != Some(0);
    c.min_qp = min_qp;
    c
}

fn level_rank(l: H264Level) -> u8 {
    match l {
        H264Level::High40 => 0,
        H264Level::High42 => 1,
        H264Level::High51 => 2,
    }
}

/// [`gmclip_plan::validate::fitting_level`] as the encoder's level.
fn fitting_level(w: u32, h: u32, fps: u32) -> H264Level {
    match gmclip_plan::validate::fitting_level(w.into(), h.into(), fps.into()) {
        "4.0" => H264Level::High40,
        "4.2" => H264Level::High42,
        _ => H264Level::High51,
    }
}

struct VideoPass {
    peak: u64,
    /// The encoder took a lower rate than the plan's (decision 0027).
    encoder_note: Option<String>,
    seeks: u64,
    path: PathBuf,
    hashes: Vec<String>,
    draft_mattes: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
fn video_pass(
    plan: &Plan,
    inputs: &mut dyn Inputs,
    ctx: &GpuContext,
    audio: &AudioTrackOut,
    partial: &Path,
    min_qp: Option<u8>,
    opts: &ExportOptions,
    store: &Arc<crate::matte::store::MatteStore>,
) -> Result<VideoPass, RenderError> {
    let (w, h) = (
        plan.output.canvas.w.get() as u32,
        plan.output.canvas.h.get() as u32,
    );
    let frames = plan.output.frames;
    let cfg = encoder_config(plan, min_qp);
    let (fnum, fden) = plan.output.fps.rate();
    let ticks = i64::from(cfg.timescale) * fden as i64 / fnum as i64;
    let mut pictures = Pictures::open(plan, inputs)?;
    let mut composer = FrameComposer::new(ctx, plan, (w, h))?;
    composer.set_mattes(mattes::Mattes::new(
        store.clone(),
        if opts.draft_mattes {
            mattes::DraftPolicy::Allow
        } else {
            mattes::DraftPolicy::Refuse
        },
    ));
    // VideoToolbox holds up to a GOP of reference / lookahead buffers.
    // VideoToolbox holds its reorder/lookahead window of input buffers (measured: a
    // few frames at B = 2); 24 leaves room without sizing the pool from the GOP.
    let mut nv12 = Nv12Output::new(ctx, w, h, 3, 24).map_err(gpu("nv12_output", None))?;
    let canvas_view = composer.canvas().create_view(&Default::default());
    let (mut enc, encoder_note) = open_encoder(&cfg, H264Encoder::new)?;
    let mut sink = MuxSink {
        path: partial.to_path_buf(),
        writer: None,
        audio,
        next_audio: 0,
        audio_pts: 0,
        width: w as u16,
        height: h as u16,
        fps: fnum as u32,
        fps_den: fden as u32,
        frames,
        video_ts: i64::from(cfg.timescale),
        video_written: 0,
    };
    let mut encoded = 0u64;
    let mut hashes: Vec<String> = Vec::new();
    let hash = opts.hash_encoder_input;
    let mut encode_ready =
        |pb: OutFrame, enc: &mut H264Encoder, sink: &mut MuxSink<'_>| -> Result<(), RenderError> {
            if hash {
                let p = pb
                    .read_planes()
                    .map_err(gpu("nv12_readback", Some(encoded)))?;
                let mut all = p.y;
                all.extend_from_slice(&p.uv);
                hashes.push(crate::sha256::hex(&all));
            }
            enc.encode(&pb, encoded as i64 * ticks, ticks, sink)?;
            encoded += 1;
            Ok(())
        };
    for f in 0..frames {
        if opts.cancelled() {
            return Err(failed("cancelled", Some(f), "export cancelled"));
        }
        if let Some(p) = &opts.progress {
            p.store(f, std::sync::atomic::Ordering::Relaxed);
        }
        composer.compose(ctx, plan, f, &[], &mut ExportPictures(&mut pictures))?;
        if let Some(pb) = nv12
            .render(&canvas_view)
            .map_err(gpu("nv12_output", Some(f)))?
        {
            encode_ready(pb, &mut enc, &mut sink)?;
        }
    }
    for pb in nv12.finish().map_err(gpu("nv12_output", None))? {
        encode_ready(pb, &mut enc, &mut sink)?;
    }
    enc.finish(&mut sink)?;
    if sink.video_written != frames {
        return Err(failed(
            "encode",
            None,
            format!("{} of {frames} frames encoded", sink.video_written),
        ));
    }
    let peak = enc.peak_bits_per_second();
    let seeks = pictures.seeks();
    let path = sink.finish()?;
    let draft_mattes = composer.mattes().used_draft.iter().cloned().collect();
    Ok(VideoPass {
        peak,
        encoder_note,
        seeks,
        path,
        hashes,
        draft_mattes,
    })
}

/// B-15's verify on the written bytes: re-demux, decode the audio track with the OS's
/// decoder, compare with the encoded mix.
fn verify_file(
    plan: &Plan,
    bytes: &[u8],
    audio: &AudioTrackOut,
) -> Result<gmclip_media::verify::VerifyReport, RenderError> {
    use gmclip_media::demux::io::SliceSource;
    let mut src = SliceSource(bytes);
    let d = gmclip_media::demux::index(&mut src).map_err(|e| failed("verify_demux", None, e))?;
    let t = d
        .tracks
        .iter()
        .find(|t| t.kind == gmclip_media::demux::TrackKind::Audio)
        .ok_or_else(|| failed("verify_demux", None, "no audio track"))?;
    let gmclip_media::demux::Codec::Aac { asc } = &t.codec else {
        return Err(failed("verify_demux", None, "audio track is not AAC"));
    };
    // No edit list in our files; the gapless tag says priming 0 and length N.
    let mut dec = AacTrackDecoder::new(asc, 48_000, 2, 0, Some(audio.samples))?;
    let mut decoded = Vec::with_capacity(audio.mix.len());
    let s = &t.samples;
    for i in 0..s.len() {
        let (off, size) = (s.offset[i] as usize, s.size[i] as usize);
        let pkt = bytes
            .get(off..off + size)
            .ok_or_else(|| failed("verify_demux", Some(i as u64), "sample past the end"))?;
        dec.decode(pkt, &mut decoded)?;
    }
    dec.finish(&mut decoded)?;
    let expect = gmclip_media::verify::Expect::from_plan(plan);
    Ok(gmclip_media::verify::verify(
        bytes,
        &expect,
        Some((&decoded, &audio.mix, &audio.plan)),
    ))
}

/// Removes the partial file unless the export installed it.
struct PartialGuard<'a>(&'a Path);

impl Drop for PartialGuard<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0);
    }
}

pub(super) fn export(
    plan: &Plan,
    inputs: &mut dyn Inputs,
    out: &Path,
    opts: &ExportOptions,
) -> Result<ExportReport, RenderError> {
    let errors = gmclip_plan::check_plan(plan);
    if let Some(e) = errors.into_iter().next() {
        return Err(RenderError::Plan(e));
    }
    // Everything that can be refused is refused before any work.
    if let Some(l) = plan.output.video.level.as_deref()
        && !matches!(l, "4.0" | "4.2" | "5.1")
    {
        return Err(RenderError::Unsupported {
            step: "encode",
            reason: format!("H.264 level {l} (presets: 4.0, 4.2, 5.1)"),
        });
    }
    if opts.keep_superseded == Some(0) {
        return Err(RenderError::Invalid {
            step: "install",
            reason: "keep_superseded must be at least 1".into(),
        });
    }
    // Fonts and text styles: a missing font is refused here, not after the audio pass.
    crate::compositor::text_layer::TextLayer::new(plan).map_err(compose_err(0))?;
    let cancelled = |at: &'static str| -> Result<(), RenderError> {
        if opts.cancelled() {
            Err(failed(
                "cancelled",
                None,
                format!("export cancelled ({at})"),
            ))
        } else {
            Ok(())
        }
    };
    let t0 = Instant::now();
    let mut notes = Vec::new();
    notes.push("not verified: the automatic verify (R-EXP-6, B-15) is not wired in yet".to_owned());
    // a recorded cursor that cannot be drawn is a note, never a failed export (decision 0045)
    notes.extend(
        crate::compositor::cursor::CursorLayer::new(plan)
            .notes()
            .iter()
            .cloned(),
    );
    cancelled("before audio")?;
    let audio = encode_audio(plan, inputs, &mut notes, opts.hash_encoder_input)?;
    cancelled("after audio")?;
    // Roto mattes (SPEC §5.8): export waits for accurate mask tracks unless the user
    // confirmed draft mattes.
    let store = opts
        .mattes
        .clone()
        .unwrap_or_else(|| Arc::new(crate::matte::store::MatteStore::in_memory()));
    if !opts.draft_mattes {
        let b = mattes::build_tracks(
            plan,
            inputs,
            &store,
            os::SegQuality::Accurate,
            opts.matte_progress.as_deref(),
            opts.cancel.as_deref(),
        )?;
        if !b.needs.is_empty() {
            notes.push(format!(
                "mattes: {} mask tracks ({} built, {} frames segmented in {:.1} s)",
                b.needs.len(),
                b.built,
                b.frames,
                b.seconds
            ));
        }
    }
    cancelled("after mattes")?;
    let ctx = GpuContext::new(&GpuOptions::default()).map_err(gpu("gpu_device", None))?;
    let partial = out.with_extension("partial.mp4");
    let _guard = PartialGuard(&partial);
    let mut accepted = None;
    let mut seeks = 0;
    for (rung, min_qp) in RATE_CAP_LADDER.iter().enumerate() {
        let pass = video_pass(plan, inputs, &ctx, &audio, &partial, *min_qp, opts, &store)?;
        seeks += pass.seeks;
        if pass.peak <= peak_cap(plan) {
            accepted = Some((rung, pass));
            break;
        }
        notes.push(format!(
            "rate cap: 1-s peak {:.2} Mbps at rung {rung}; re-encoding",
            pass.peak as f64 / 1e6
        ));
    }
    let Some((rung, pass)) = accepted else {
        return Err(failed(
            "rate_cap",
            None,
            "no rung of the ladder held the 1-s peak cap",
        ));
    };
    let bytes = std::fs::read(&pass.path).map_err(|e| RenderError::Io {
        step: "record",
        what: "rendered file",
        kind: e.kind(),
    })?;
    // R-EXP-6: verify the written file before it is installed (B-15): container facts,
    // then the audio as the OS decodes it against the mix that was encoded.
    let report = verify_file(plan, &bytes, &audio)?;
    if let Some(c) = report.checks.iter().find(|c| !c.ok) {
        return Err(failed(c.step, None, format!("verify: {}", c.detail)));
    }
    let worst_join = report
        .joins
        .iter()
        .map(|j| j.lag_ms.abs())
        .fold(0.0f64, f64::max);
    notes.push(format!(
        "verified: {} checks passed (B-15 verify v0); {} discontinuous joins, worst A/V offset {worst_join:.2} ms, drift {:?} ms/join",
        report.checks.len(),
        report.joins.len(),
        report.drift_ms_per_join.map(|d| (d * 1000.0).round() / 1000.0)
    ));
    notes.retain(|n| !n.starts_with("not verified"));
    if let Some(n) = &pass.encoder_note {
        notes.push(n.clone());
    }
    if !pass.draft_mattes.is_empty() {
        notes.push(format!(
            "draft mattes (confirmed): {}",
            pass.draft_mattes.join(", ")
        ));
    }
    let mut record = ExportRecord {
        render_sig: None,
        render_sig_absent: None,
        framing: None,
        preset: match (plan.output.canvas.w.get(), plan.output.canvas.h.get()) {
            (1080, 1920) => "reels-1080x1920".to_owned(),
            (1920, 1080) if plan.output.video.gop_frames == Some(plan.output.fps.rounded() / 2) => {
                "youtube-1080p".to_owned()
            }
            (2560, 1440) => "youtube-1440p".to_owned(),
            (3840, 2160) => "youtube-2160p".to_owned(),
            (w, h) => format!("custom-{w}x{h}"),
        },
        input_rev: None,
        duration_expected: DurationExpected {
            frames: plan.output.frames,
            fps_num: plan.output.fps.num.get(),
            fps_den: plan.output.fps.den.get(),
            audio_samples: audio.samples,
        },
        // Decision 0005: reproducibility is checked on the composited frames handed to
        // the encoder (the hardware encoder is not bit-reproducible).
        frames_hash: (!pass.hashes.is_empty())
            .then(|| crate::sha256::hex(pass.hashes.concat().as_bytes())),
        file: out
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        sha256: crate::sha256::hex(&bytes),
        loudness: Some(audio.loudness.clone()),
        draft_mattes: pass.draft_mattes.clone(),
        notes: notes.clone(),
        blocked: Vec::new(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        renderer_version: format!("gmclip-render {}", env!("CARGO_PKG_VERSION")),
    };
    record.set_sig(render_sig_of(plan));
    install_export(&pass.path, out, &record, opts.keep_superseded.unwrap_or(3)).map_err(|e| {
        RenderError::Io {
            step: "install",
            what: "output",
            kind: e.kind(),
        }
    })?;
    Ok(ExportReport {
        out: out.to_path_buf(),
        frames: plan.output.frames,
        audio_samples: audio.samples,
        seconds: t0.elapsed().as_secs_f64(),
        video_peak_bps: pass.peak,
        rate_rung: rung,
        video_seeks: seeks,
        notes,
        encoder_input_hashes: pass.hashes.clone(),
        mix_hashes: audio.mix_hashes.clone(),
        draft_mattes: pass.draft_mattes.clone(),
    })
}

/// Output frames `at` rendered exactly as the export renders them (compositor + NV12
/// output), as RGB for contact sheets (H-05b `gmclip still`).
pub(super) fn render_frames(
    plan: &Plan,
    inputs: &mut dyn Inputs,
    at: &[u64],
) -> Result<Vec<(u64, super::sheet::RgbImage)>, RenderError> {
    let (w, h) = (
        plan.output.canvas.w.get() as u32,
        plan.output.canvas.h.get() as u32,
    );
    let ctx = GpuContext::new(&GpuOptions::default()).map_err(gpu("gpu_device", None))?;
    let mut pictures = Pictures::open(plan, inputs)?;
    let mut composer = FrameComposer::new(&ctx, plan, (w, h))?;
    let mut nv12 = Nv12Output::new(&ctx, w, h, 1, 1).map_err(gpu("nv12_output", None))?;
    let canvas_view = composer.canvas().create_view(&Default::default());
    let mut out = Vec::with_capacity(at.len());
    for &f in at {
        composer.compose(&ctx, plan, f, &[], &mut ExportPictures(&mut pictures))?;
        let pb = nv12
            .render_now(&canvas_view)
            .map_err(gpu("nv12_output", Some(f)))?;
        let p = pb.read_planes().map_err(gpu("nv12_readback", Some(f)))?;
        out.push((f, super::sheet::nv12_to_rgb(&p.y, &p.uv, w, h)));
    }
    Ok(out)
}

#[cfg(test)]
mod encoder_open_tests {
    use super::*;

    fn cfg(avg: u32, max: u32) -> EncoderConfig {
        let mut c = EncoderConfig::reels(2560, 1440);
        c.fps = 60;
        c.level = H264Level::High51;
        c.average_bitrate = avg;
        c.max_bitrate = max;
        c
    }

    fn refuse(step: &'static str) -> MediaError {
        MediaError {
            step,
            frame: None,
            source_id: None,
            os_status: Some(-1),
        }
    }

    /// A 2160-tall portrait passes level 4.2's MaxFS at any rate, so it opens at 5.1.
    #[test]
    fn a_2160_portrait_opens_at_5_1() {
        assert_eq!(fitting_level(1080, 1920, 30), H264Level::High40);
        assert_eq!(fitting_level(1080, 1920, 60), H264Level::High42);
        assert_eq!(fitting_level(1214, 2160, 30), H264Level::High51);
        assert_eq!(fitting_level(1214, 2160, 60), H264Level::High51);
    }

    /// Decision 0027: the plan's rate when the encoder takes it, with no note.
    #[test]
    fn the_plans_rate_when_taken() {
        let (got, note) = open_encoder(&cfg(36_000_000, 54_000_000), |c| {
            Ok::<_, MediaError>(c.max_bitrate)
        })
        .unwrap();
        assert_eq!((got, note), (54_000_000, None));
    }

    /// An upload rate the encoder refuses is retried once at the social rate, and said.
    #[test]
    fn a_refused_upload_rate_falls_back_and_says_so() {
        let mut tried = Vec::new();
        let (got, note) = open_encoder(&cfg(36_000_000, 54_000_000), |c| {
            tried.push((c.average_bitrate, c.max_bitrate));
            if c.max_bitrate > 25_000_000 {
                Err(refuse("mf_bitrate"))
            } else {
                Ok(c.max_bitrate)
            }
        })
        .unwrap();
        assert_eq!(got, 25_000_000);
        assert_eq!(
            tried,
            vec![(36_000_000, 54_000_000), (24_000_000, 25_000_000)]
        );
        let note = note.unwrap();
        assert!(
            note.contains("refused 2560×1440 at 60 fps, 36 Mbit/s (peak 54)")
                && note.contains("encoded at 24 Mbit/s (peak 25)"),
            "{note}"
        );
    }

    /// A refusal of the social rate (or of the fallback) is an error in words.
    #[test]
    fn a_refusal_is_an_error_in_words() {
        for c in [cfg(16_000_000, 24_000_000), cfg(36_000_000, 54_000_000)] {
            let e = open_encoder(&c, |_| Err::<(), _>(refuse("mf_level"))).unwrap_err();
            let s = e.to_string();
            assert!(
                s.contains("refused 2560×1440 at 60 fps") && s.contains("YouTube 1080p"),
                "{s}"
            );
        }
    }
}
