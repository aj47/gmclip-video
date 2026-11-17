//! What import needs to know about a source before any decoding: its tracks and picture
//! size (to normalise crops), from gmclip's own demuxers (B-01 MP4, B-02 MKV). Export
//! and preview open sources through lane H's `gmclip_render::export::inputs::DemuxInputs`.

use std::path::Path;

use gmclip_media::demux::{self, Track, TrackKind};
use gmclip_render::ops::RenderError;

/// What one source file holds, indexed once.
#[derive(Debug)]
pub struct Indexed {
    pub video: Option<Track>,
    /// The first audio track.
    pub audio: Option<Track>,
    /// Every audio track, in file order (the import sheet asks which is the mic).
    pub audio_all: Vec<Track>,
}

/// Indexes `path` (no sample data is read).
pub fn index_file(path: &Path) -> Result<Indexed, RenderError> {
    let mut src = demux::io::FileSource::open(path).map_err(|e| RenderError::Invalid {
        step: "open_source",
        reason: e.to_string(),
    })?;
    let d = demux::index(&mut src).map_err(|e| RenderError::Invalid {
        step: "index_source",
        reason: e.to_string(),
    })?;
    let mut video = None;
    let mut audio_all = Vec::new();
    for t in d.tracks {
        match t.kind {
            TrackKind::Video if video.is_none() => video = Some(t),
            TrackKind::Audio => audio_all.push(t),
            _ => {}
        }
    }
    Ok(Indexed {
        video,
        audio: audio_all.first().cloned(),
        audio_all,
    })
}
