//! AAC-LC decode with the timing rules of SPEC §6.5 step 3 (H-04, H-10), shared by the
//! OS backends: AudioToolbox on macOS, Media Foundation on Windows (M5). Edit lists /
//! priming in sources are honoured on decode. The FFI is `gmclip_gpu::{apple,windows}`;
//! this module is safe code with one behaviour on both.

#[cfg(target_os = "macos")]
use gmclip_gpu::apple::{AAC_FRAME, AacDecoder};
#[cfg(windows)]
use gmclip_gpu::windows::{AAC_FRAME, AacDecoder};

#[cfg(any(target_os = "macos", windows))]
use crate::backend::source::SampleSource;
#[cfg(any(target_os = "macos", windows))]
use crate::decode::MediaError;
#[cfg(any(target_os = "macos", windows))]
use crate::index::SampleIdx;

/// Maps a gmclip-gpu error to a media error at `step`.
#[cfg(any(target_os = "macos", windows))]
fn media_err(step: &'static str, frame: Option<u64>, e: gmclip_gpu::GpuError) -> MediaError {
    MediaError {
        step,
        frame,
        source_id: None,
        os_status: e.os_status(),
    }
}
/// The AudioSpecificConfig inside an MPEG-4 ES descriptor (an `esds` payload or
/// AudioToolbox's compression magic cookie): ES_Descriptor (0x03) →
/// DecoderConfigDescriptor (0x04) → DecoderSpecificInfo (0x05). Walked iteratively
/// along that one path (no recursion: file data cannot exhaust the stack), with the
/// ES_Descriptor's optional fields skipped; any malformed length is `None`.
pub fn asc_from_esds(es: &[u8]) -> Option<Vec<u8>> {
    /// `(tag, body)` of the descriptor starting at `b[0]`, plus the bytes after it.
    fn descriptor(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
        let tag = *b.first()?;
        let mut i = 1usize;
        let mut len = 0usize;
        for k in 0..4 {
            let c = *b.get(i)?;
            i += 1;
            len = (len << 7) | usize::from(c & 0x7f);
            if c & 0x80 == 0 {
                break;
            }
            if k == 3 {
                return None;
            }
        }
        let end = i.checked_add(len)?;
        Some((tag, b.get(i..end)?, b.get(end..)?))
    }
    /// The first descriptor with `tag` in a sequence.
    fn find(mut b: &[u8], want: u8) -> Option<&[u8]> {
        while !b.is_empty() {
            let (tag, body, rest) = descriptor(b)?;
            if tag == want {
                return Some(body);
            }
            b = rest;
        }
        None
    }
    let es_body = find(es, 0x03)?;
    // ES_ID (2), flags (1), then dependsOn_ES_ID (2), URL (1 + n), OCR_ES_ID (2).
    let flags = *es_body.get(2)?;
    let mut i = 3usize;
    if flags & 0x80 != 0 {
        i += 2;
    }
    if flags & 0x40 != 0 {
        i += 1 + usize::from(*es_body.get(i)?);
    }
    if flags & 0x20 != 0 {
        i += 2;
    }
    let dcd = find(es_body.get(i..)?, 0x04)?;
    find(dcd.get(13..)?, 0x05).map(<[u8]>::to_vec)
}

/// The sampling rate and channel count an AudioSpecificConfig declares (ISO 14496-3
/// 1.6.2.1): what the decoder must be opened with. A sample entry's own `channelcount`
/// is often 2 whatever the stream (AudioToolbox writes 2 for mono AAC). `None` for a
/// config too short, a reserved rate index, channels left to a program config (0), or
/// HE-AAC (SBR, PS), whose config gives the core's rate.
pub fn asc_rate_channels(asc: &[u8]) -> Option<(u32, u32)> {
    const RATES: [u32; 13] = [
        96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
        8_000, 7_350,
    ];
    let mut bits = 0u64;
    for (i, b) in asc.iter().take(8).enumerate() {
        bits |= u64::from(*b) << (56 - 8 * i);
    }
    let mut at = 0u32;
    let mut take = |n: u32| {
        let v = (bits << at) >> (64 - n);
        at += n;
        v as u32
    };
    let mut object = take(5);
    if object == 31 {
        object = 32 + take(6); // escaped object type
    }
    // SBR / PS (HE-AAC) declare the core's rate here, not the stream's
    if object == 5 || object == 29 {
        return None;
    }
    let rate = match take(4) {
        15 => take(24),
        i => *RATES.get(i as usize)?,
    };
    let ch = take(4);
    let need = (at as usize).div_ceil(8);
    (asc.len() >= need && rate > 0 && (1..=7).contains(&ch))
        .then_some((rate, if ch == 7 { 8 } else { ch }))
}

/// Decodes a source's AAC track to interleaved f32, dropping the leading samples the
/// container says to skip (edit-list `media_time`, i.e. the encoder's priming).
///
/// The decoder may answer late (Media Foundation's answers one packet behind); samples
/// are counted as they come, so the output is the same, and [`AacTrackDecoder::finish`]
/// collects what it still holds at the end of the track (nothing on macOS).
#[cfg(any(target_os = "macos", windows))]
#[derive(Debug)]
pub struct AacTrackDecoder {
    dec: AacDecoder,
    skip: u64,
    /// Samples still allowed out (the edit list's segment duration), if bounded.
    remaining: Option<u64>,
    channels: usize,
    packets: u64,
    pcm: Vec<f32>,
}

#[cfg(any(target_os = "macos", windows))]
impl AacTrackDecoder {
    /// `skip` = samples to drop from the start of the decoded stream (edit-list
    /// `media_time`, i.e. the encoder's priming; 0 without an edit list); `valid` =
    /// the edit list's segment duration in samples (trailing padding is cut), `None`
    /// for "to the end". An empty edit (a leading delay) is the caller's to reject or
    /// pad; until B-01 lands the lab tests take both values from ffprobe.
    pub fn new(
        asc: &[u8],
        sample_rate: u32,
        channels: u32,
        skip: u64,
        valid: Option<u64>,
    ) -> Result<Self, MediaError> {
        Ok(Self {
            dec: AacDecoder::new(asc, sample_rate, channels)
                .map_err(|e| media_err("aac_decoder_create", None, e))?,
            skip,
            remaining: valid,
            channels: channels as usize,
            packets: 0,
            pcm: Vec::with_capacity(AAC_FRAME * 2 * channels as usize),
        })
    }

    /// Decodes one access unit, appending the (post-skip) PCM the decoder hands out to
    /// `out`.
    pub fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<(), MediaError> {
        self.pcm.clear();
        self.dec
            .decode(packet, &mut self.pcm)
            .map_err(|e| media_err("aac_decode", Some(self.packets), e))?;
        self.packets += 1;
        self.keep(out);
        Ok(())
    }

    /// End of the track: appends what the decoder still holds (the last packet's
    /// samples on Windows; nothing on macOS).
    pub fn finish(&mut self, out: &mut Vec<f32>) -> Result<(), MediaError> {
        self.pcm.clear();
        self.dec
            .drain(&mut self.pcm)
            .map_err(|e| media_err("aac_drain", Some(self.packets), e))?;
        self.keep(out);
        Ok(())
    }

    /// Moves `self.pcm` to `out` after the skip, within the valid length.
    fn keep(&mut self, out: &mut Vec<f32>) {
        let frames = (self.pcm.len() / self.channels) as u64;
        let drop = self.skip.min(frames);
        self.skip -= drop;
        let mut keep = frames - drop;
        if let Some(r) = self.remaining.as_mut() {
            keep = keep.min(*r);
            *r -= keep;
        }
        let start = drop as usize * self.channels;
        out.extend_from_slice(&self.pcm[start..start + keep as usize * self.channels]);
    }
}

/// Random access to a source's decoded AAC by sample index (after the edit-list
/// skip): what the mix graph's `PcmSource` needs (H-10). Keeps a contiguous decoded
/// window so consecutive reads decode each packet once; a jump re-seeks, decoding two
/// packets of pre-roll (AAC frames overlap) and discarding them.
///
/// Packet `k` (decode order) yields decoded positions `[1024·k, 1024·k + 1024)`, in
/// order. Samples are placed by count, not by the packet that produced them: a decoder
/// that answers late (Media Foundation: one packet behind) fills the same positions. The
/// first packet after a reset decodes without its overlap, so its positions are
/// discarded; past the last packet the decoder is drained once. Positions past the end
/// read as silence.
#[cfg(any(target_os = "macos", windows))]
#[derive(Debug)]
pub struct AacRandomReader<S: SampleSource> {
    source: S,
    dec: AacDecoder,
    channels: usize,
    skip: u64,
    /// Samples of silence before the first decoded one kept ([`Self::with_lead`]).
    lead: u64,
    /// Decoded position of `win[0]`.
    win_start: u64,
    win: Vec<f32>,
    /// Next packet to decode to extend the window.
    next_packet: usize,
    /// Decoded position of the next sample the decoder hands out.
    out_pos: u64,
    /// Samples before this position are dropped (the overlap-less packet after a reset).
    discard_until: u64,
    /// The decoder has been drained at the end of the track.
    drained: bool,
    buf: Vec<u8>,
    pcm: Vec<f32>,
    /// Re-seeks done (export report).
    pub seeks: u64,
}

#[cfg(any(target_os = "macos", windows))]
const PREROLL_PACKETS: u64 = 2;

#[cfg(any(target_os = "macos", windows))]
impl<S: SampleSource> AacRandomReader<S> {
    pub fn new(
        source: S,
        asc: &[u8],
        sample_rate: u32,
        channels: u32,
        skip: u64,
    ) -> Result<Self, MediaError> {
        Ok(Self {
            source,
            dec: AacDecoder::new(asc, sample_rate, channels)
                .map_err(|e| media_err("aac_decoder_create", None, e))?,
            channels: channels as usize,
            skip,
            lead: 0,
            win_start: 0,
            win: Vec::new(),
            next_packet: 0,
            out_pos: 0,
            discard_until: 0,
            drained: false,
            buf: Vec::new(),
            pcm: Vec::new(),
            seeks: 0,
        })
    }

    /// `lead` samples of silence before the first one kept: an empty edit longer than
    /// the priming ([`crate::demux::Track::audio_start`]).
    #[must_use]
    pub fn with_lead(mut self, lead: u64) -> Self {
        self.lead = lead;
        self
    }

    fn win_end(&self) -> u64 {
        self.win_start + (self.win.len() / self.channels) as u64
    }

    /// Feeds the next packet (or, past the last, drains once) and keeps what comes out.
    /// `false` when there is nothing more to feed.
    fn decode_next(&mut self) -> Result<bool, MediaError> {
        self.pcm.clear();
        let n = if self.next_packet < self.source.index().len() {
            let k = self.next_packet;
            self.source.read(SampleIdx(k as u32), &mut self.buf)?;
            let n = self
                .dec
                .decode(&self.buf, &mut self.pcm)
                .map_err(|e| media_err("aac_decode", Some(k as u64), e))?;
            // one packet's worth, or nothing yet from a decoder that answers late
            if n != AAC_FRAME && n != 0 {
                return Err(MediaError {
                    step: "aac_decode_frame_count",
                    frame: Some(k as u64),
                    source_id: None,
                    os_status: None,
                });
            }
            self.next_packet += 1;
            n
        } else if !self.drained {
            self.drained = true;
            self.dec
                .drain(&mut self.pcm)
                .map_err(|e| media_err("aac_drain", None, e))?
        } else {
            return Ok(false);
        };
        let ch = self.channels;
        let first = self.out_pos;
        self.out_pos += n as u64;
        let skip = self.discard_until.saturating_sub(first).min(n as u64) as usize;
        self.win.extend_from_slice(&self.pcm[skip * ch..n * ch]);
        Ok(true)
    }

    fn seek(&mut self, pos: u64) -> Result<(), MediaError> {
        let p = pos / AAC_FRAME as u64;
        let start = p.saturating_sub(PREROLL_PACKETS);
        self.dec
            .reset()
            .map_err(|e| media_err("aac_decoder_reset", None, e))?;
        self.win.clear();
        self.next_packet = start as usize;
        self.out_pos = start * AAC_FRAME as u64;
        self.drained = false;
        // The first packet after a reset decodes without its overlap: discard it.
        self.discard_until = if start > 0 {
            self.out_pos + AAC_FRAME as u64
        } else {
            0
        };
        self.win_start = self.discard_until.max(self.out_pos);
        self.seeks += 1;
        Ok(())
    }

    /// Fills `out` (interleaved) from sample `first` (post-skip, after the lead) on.
    pub fn read(&mut self, first: u64, out: &mut [f32]) -> Result<(), MediaError> {
        let ch = self.channels;
        if first < self.lead {
            let quiet = (((self.lead - first) as usize) * ch).min(out.len());
            out[..quiet].fill(0.0);
            if quiet == out.len() {
                return Ok(());
            }
            return self.read_decoded(0, &mut out[quiet..]);
        }
        self.read_decoded(first - self.lead, out)
    }

    /// [`Self::read`] from decoded sample `first` (post-skip).
    fn read_decoded(&mut self, first: u64, out: &mut [f32]) -> Result<(), MediaError> {
        let ch = self.channels;
        let frames = (out.len() / ch) as u64;
        let a = first + self.skip;
        let b = a + frames;
        if a < self.win_start || a > self.win_end() + 4 * AAC_FRAME as u64 {
            self.seek(a)?;
        }
        while self.win_end() < b {
            if !self.decode_next()? {
                break;
            }
        }
        out.fill(0.0);
        let have_from = a.max(self.win_start);
        let have_to = b.min(self.win_end());
        if have_to > have_from {
            let src = ((have_from - self.win_start) as usize) * ch;
            let dst = ((have_from - a) as usize) * ch;
            let n = ((have_to - have_from) as usize) * ch;
            out[dst..dst + n].copy_from_slice(&self.win[src..src + n]);
        }
        // Keep the window bounded: drop what lies before this read.
        if a > self.win_start + 8 * AAC_FRAME as u64 {
            let cut = ((a - self.win_start) as usize).min(self.win.len() / ch);
            self.win.drain(..cut * ch);
            self.win_start += cut as u64;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AudioToolbox's mono AAC at 48 kHz (its sample entry says 2 channels), stereo at
    /// 44.1 kHz, an explicit 24-bit rate, and configs that say nothing usable.
    #[test]
    fn the_config_says_the_rate_and_channels() {
        assert_eq!(asc_rate_channels(&[0x11, 0x88]), Some((48_000, 1)));
        assert_eq!(asc_rate_channels(&[0x12, 0x10]), Some((44_100, 2)));
        // LC, index 15, 22 000 Hz in 24 bits, 2 channels: 5 + 4 + 24 + 4 bits
        let bits: u64 = (2 << 59) | (15 << 55) | (22_000 << 31) | (2 << 27);
        assert_eq!(
            asc_rate_channels(&bits.to_be_bytes()[..5]),
            Some((22_000, 2))
        );
        assert_eq!(asc_rate_channels(&[0x11, 0x80]), None, "a program config");
        assert_eq!(asc_rate_channels(&[0x11]), None, "too short");
        assert_eq!(
            asc_rate_channels(&[0x16, 0x88]),
            None,
            "reserved rate index"
        );
        assert_eq!(asc_rate_channels(&[0x2b, 0x92]), None, "HE-AAC (SBR)");
    }
}
