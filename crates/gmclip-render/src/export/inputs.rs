//! [`Inputs`] from lane B's demuxers (B-01 MP4/MOV, B-02 MKV/WebM): what `gmclip
//! render` and `gmclip still` open sources with. Each file is indexed once and shared
//! by every reader of it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gmclip_media::MediaError;
use gmclip_media::SampleIndex;
use gmclip_media::backend::SampleSource;
use gmclip_media::backend::pcm::PcmFormat;
use gmclip_media::demux::io::{ByteSource, FileSource};
use gmclip_media::demux::{self, Codec, Demuxed, TrackKind};
use gmclip_media::index::SampleIdx;
use gmclip_plan::Plan;

use super::{AudioInput, Inputs, VideoCodec, VideoInput};
use crate::compositor::ChromaSiting;
use crate::ops::RenderError;

/// One demuxed track's samples, read from its own file handle.
struct TrackSource {
    file: FileSource,
    index: Arc<SampleIndex>,
}

impl SampleSource for TrackSource {
    fn index(&self) -> &SampleIndex {
        &self.index
    }

    fn read(&mut self, s: SampleIdx, buf: &mut Vec<u8>) -> Result<(), MediaError> {
        let i = s.0 as usize;
        let (Some(&off), Some(&size)) = (self.index.offset.get(i), self.index.size.get(i)) else {
            return Err(MediaError {
                step: "sample_read",
                frame: Some(u64::from(s.0)),
                source_id: None,
                os_status: None,
            });
        };
        // a sample entry past the end of the file (a corrupt or still-growing
        // recording) or over 256 MB is an error before any buffer is sized from it
        if size > 256 * 1024 * 1024
            || off
                .checked_add(u64::from(size))
                .is_none_or(|end| end > self.file.len())
        {
            return Err(MediaError {
                step: "sample_read",
                frame: Some(u64::from(s.0)),
                source_id: None,
                os_status: None,
            });
        }
        buf.resize(size as usize, 0);
        self.file.read_at(off, buf).map_err(|_| MediaError {
            step: "sample_read",
            frame: Some(u64::from(s.0)),
            source_id: None,
            os_status: None,
        })
    }
}

/// A demuxed file and its tracks' sample indexes (shared by every reader).
type Indexed = Arc<(Demuxed, Vec<Arc<SampleIndex>>)>;

/// Opens plan sources through the demuxers.
#[derive(Default)]
pub struct DemuxInputs {
    /// Plan-relative paths resolve against this directory.
    base: Option<PathBuf>,
    files: HashMap<PathBuf, Indexed>,
}

impl std::fmt::Debug for DemuxInputs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DemuxInputs")
            .field("files", &self.files.len())
            .finish()
    }
}

fn open_failed(what: &'static str, source: &str, reason: impl ToString) -> RenderError {
    RenderError::Failed {
        step: what,
        frame: None,
        reason: format!("source {source}: {}", reason.to_string()),
    }
}

impl DemuxInputs {
    /// `base`: the directory relative source paths resolve against (the plan's).
    pub fn new(base: Option<&Path>) -> Self {
        Self {
            base: base.map(Path::to_path_buf),
            files: HashMap::new(),
        }
    }

    fn path_of(&self, plan: &Plan, source: &str) -> Result<PathBuf, RenderError> {
        let s = plan
            .sources
            .iter()
            .find(|s| s.id.as_str() == source)
            .ok_or_else(|| open_failed("open_source", source, "not in the plan"))?;
        let p = PathBuf::from(&s.path);
        Ok(match (&self.base, p.is_relative()) {
            (Some(b), true) => b.join(p),
            _ => p,
        })
    }

    fn demuxed(&mut self, path: &Path, source: &str) -> Result<Indexed, RenderError> {
        if let Some(d) = self.files.get(path) {
            return Ok(d.clone());
        }
        let mut f = FileSource::open(path).map_err(|e| open_failed("open_source", source, e))?;
        let d = demux::index(&mut f).map_err(|e| open_failed("demux", source, e))?;
        let idx = d
            .tracks
            .iter()
            .map(|t| Arc::new(t.samples.clone()))
            .collect();
        let entry = Arc::new((d, idx));
        self.files.insert(path.to_path_buf(), entry.clone());
        Ok(entry)
    }

    fn track(
        &mut self,
        plan: &Plan,
        source: &str,
        kind: TrackKind,
    ) -> Result<(Indexed, usize, TrackSource), RenderError> {
        let path = self.path_of(plan, source)?;
        // audio: the plan's `audio_track` (the mic, decision 0009), else the first
        let want = (kind == TrackKind::Audio)
            .then(|| {
                plan.sources
                    .iter()
                    .find(|s| s.id.as_str() == source)
                    .and_then(|s| s.audio_track)
            })
            .flatten();
        self.track_at(&path, source, kind, want)
    }

    fn track_at(
        &mut self,
        path: &Path,
        source: &str,
        kind: TrackKind,
        want: Option<u32>,
    ) -> Result<(Indexed, usize, TrackSource), RenderError> {
        let d = self.demuxed(path, source)?;
        let k = match want {
            Some(id) => d
                .0
                .tracks
                .iter()
                .position(|t| t.kind == kind && t.id == u64::from(id))
                .ok_or_else(|| RenderError::Invalid {
                    step: "open_source",
                    reason: format!("source {source}: no audio track {id} (/sources/audio_track)"),
                })?,
            None => d
                .0
                .tracks
                .iter()
                .position(|t| t.kind == kind)
                .ok_or_else(|| open_failed("open_source", source, format!("no {kind:?} track")))?,
        };
        let file = FileSource::open(path).map_err(|e| open_failed("open_source", source, e))?;
        let src = TrackSource {
            file,
            index: d.1[k].clone(),
        };
        Ok((d, k, src))
    }

    /// The first video track of the file at `path` (no plan: the scrub proxy's builder
    /// and reader, H.05). `label` names it in errors.
    pub fn video_file(&mut self, path: &Path, label: &str) -> Result<VideoInput, RenderError> {
        let (d, k, src) = self.track_at(path, label, TrackKind::Video, None)?;
        video_input(&d, k, src, label)
    }

    /// Forgets the indexed file at `path` (a proxy segment re-written in place).
    pub fn forget(&mut self, path: &Path) {
        self.files.remove(path);
    }
}

fn video_input(
    d: &Indexed,
    k: usize,
    src: TrackSource,
    source: &str,
) -> Result<VideoInput, RenderError> {
    let t = &d.0.tracks[k];
    let codec = match &t.codec {
        Codec::H264 { avcc } => VideoCodec::H264 { avcc: avcc.clone() },
        Codec::Vp9 { .. } => VideoCodec::Vp9,
        other => {
            return Err(RenderError::Unsupported {
                step: "decode",
                reason: format!("source {source}: video codec {other:?} (M0: H.264, VP9)"),
            });
        }
    };
    // Tagged siting wins; untagged H.264 is left-sited (chroma_loc_type 0), untagged
    // VP9 centred (as swscale assumes, and as the reference's renders were made).
    let siting = match (t.colour.chroma_left, &codec) {
        (Some(true), _) | (None, VideoCodec::H264 { .. }) => ChromaSiting::Left,
        _ => ChromaSiting::Center,
    };
    Ok(VideoInput {
        source: Box::new(src),
        codec,
        width: t.width,
        height: t.height,
        siting,
    })
}

impl Inputs for DemuxInputs {
    fn video(&mut self, plan: &Plan, source: &str) -> Result<VideoInput, RenderError> {
        let (d, k, src) = self.track(plan, source, TrackKind::Video)?;
        video_input(&d, k, src, source)
    }

    fn audio(&mut self, plan: &Plan, source: &str) -> Result<AudioInput, RenderError> {
        let (d, k, src) = self.track(plan, source, TrackKind::Audio)?;
        let t = &d.0.tracks[k];
        // AAC, or linear PCM read as stored (MOV LPCM, WAV; decision 0051)
        let (asc, pcm) = match &t.codec {
            Codec::Aac { asc } => (asc.clone(), None),
            &Codec::Pcm {
                bits,
                float,
                little_endian,
            } => {
                let f = PcmFormat {
                    bits,
                    float,
                    little_endian,
                };
                if !f.readable() {
                    return Err(RenderError::Unsupported {
                        step: "audio",
                        reason: format!(
                            "source {source}: {bits}-bit PCM (16, 24 or 32-bit, or float)"
                        ),
                    });
                }
                (Vec::new(), Some(f))
            }
            other => {
                return Err(RenderError::Unsupported {
                    step: "audio",
                    reason: format!("source {source}: audio codec {other:?} (AAC or PCM)"),
                });
            }
        };
        // The edit list's priming to drop, less an empty edit in front (a recording's
        // files), or silence first when that is the longer (media timescale = rate).
        let Some((skip, lead)) = t.audio_start() else {
            return Err(RenderError::Unsupported {
                step: "audio",
                reason: format!("source {source}: rescaled or multi-segment audio edit (M0)"),
            });
        };
        // AAC: the config's rate and channels, which the decoder needs, over the sample
        // entry's (AudioToolbox's mono AAC says 2 channels there)
        let (sample_rate, channels) = gmclip_media::backend::aac::asc_rate_channels(&asc)
            .filter(|_| pcm.is_none())
            .unwrap_or((t.sample_rate, u32::from(t.channels)));
        Ok(AudioInput {
            source: Box::new(src),
            asc,
            pcm,
            sample_rate,
            channels,
            skip,
            lead,
        })
    }
}
