//! [`FrameReader`]: frame-exact random access and sequential decode over a
//! [`SampleSource`], for any OS decoder behind [`VideoDecoder`] (VideoToolbox on macOS,
//! Media Foundation on Windows). Safe, platform-neutral code.

use crate::backend::reorder::ReorderStage;
use crate::backend::seek::{is_leading_of, sample_at_pts, seek_start};
use crate::backend::source::SampleSource;
use crate::decode::{DecodedFrame, EncodedSample, MediaError, SubmitFlags, VideoDecoder};
use crate::index::SampleIdx;

/// Frame-exact random access and sequential decode over one source track.
///
/// Random access keeps the decoder's chain alive: a target reachable by continuing the
/// current decode (same or later GOP chain) is decoded forward from where the last
/// request stopped; otherwise a new session starts at the target's seek point.
pub struct FrameReader<S: SampleSource, D: VideoDecoder> {
    source: S,
    make_decoder: Box<dyn FnMut() -> Result<D, MediaError> + Send>,
    dec: Option<D>,
    /// Decode index where the current continuous chain started (a sync sample).
    chain_start: usize,
    /// Next decode index to submit on the current chain.
    next: usize,
    buf: Vec<u8>,
    /// Sessions created (one per non-continuable seek); for seek statistics.
    pub sessions_created: u64,
    /// Samples submitted (pre-roll included).
    pub samples_submitted: u64,
}

impl<S: SampleSource, D: VideoDecoder> std::fmt::Debug for FrameReader<S, D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameReader")
            .field("chain_start", &self.chain_start)
            .field("next", &self.next)
            .finish_non_exhaustive()
    }
}

impl<S: SampleSource, D: VideoDecoder> FrameReader<S, D> {
    pub fn new(
        source: S,
        make_decoder: impl FnMut() -> Result<D, MediaError> + Send + 'static,
    ) -> Self {
        Self {
            source,
            make_decoder: Box::new(make_decoder),
            dec: None,
            chain_start: 0,
            next: 0,
            buf: Vec::new(),
            sessions_created: 0,
            samples_submitted: 0,
        }
    }

    pub fn source(&self) -> &S {
        &self.source
    }

    /// The current session's capabilities (whether the OS picked hardware).
    pub fn decoder_caps(&self) -> Option<&crate::decode::DecoderCaps> {
        self.dec.as_ref().map(|d| d.caps())
    }

    fn submit(&mut self, i: usize, output: bool) -> Result<(), MediaError> {
        let s = SampleIdx(i as u32);
        self.source.read(s, &mut self.buf)?;
        let idx = self.source.index();
        let sample = EncodedSample {
            sample: s,
            dts: idx.dts[i],
            pts: idx.dts[i] + i64::from(idx.cts_off[i]),
            sync: idx.sync[i],
            data: &self.buf,
        };
        let dec = self.dec.as_mut().ok_or(MediaError {
            step: "frame_reader_no_session",
            frame: Some(i as u64),
            source_id: None,
            os_status: None,
        })?;
        dec.submit(&sample, SubmitFlags { no_output: !output })?;
        self.samples_submitted += 1;
        Ok(())
    }

    fn restart(&mut self, start: usize) -> Result<(), MediaError> {
        self.dec = None; // tear the old session down first (bounded sessions per source)
        self.dec = Some((self.make_decoder)()?);
        self.sessions_created += 1;
        self.chain_start = start;
        self.next = start;
        Ok(())
    }

    /// The picture whose PTS is `pts` (index timescale), decoded frame-exactly.
    ///
    /// Any error tears the session down, so the next request starts clean instead of
    /// continuing a chain whose state is unknown.
    pub fn frame_at_pts(&mut self, pts: i64) -> Result<DecodedFrame, MediaError> {
        let r = self.frame_at_pts_inner(pts);
        if r.is_err() {
            self.dec = None;
        }
        r
    }

    fn frame_at_pts_inner(&mut self, pts: i64) -> Result<DecodedFrame, MediaError> {
        let not_found = MediaError {
            step: "frame_at_pts_not_in_index",
            frame: None,
            source_id: None,
            os_status: None,
        };
        let target = sample_at_pts(self.source.index(), pts).ok_or(not_found.clone())?;
        let t = target.0 as usize;
        let start = seek_start(self.source.index(), target).ok_or(MediaError {
            step: "undecodable_leading_picture",
            frame: Some(t as u64),
            source_id: None,
            os_status: None,
        })?;
        // Continue the current chain only if it began at or before the target's seek
        // point and has not yet passed the target; a chain that began later started at
        // a sync sample the target is a leading picture of.
        let continuable =
            self.dec.is_some() && self.chain_start <= start && start <= self.next && self.next <= t;
        if !continuable {
            self.restart(start)?;
        }
        for i in self.next..=t {
            // Leading pictures of the chain's sync sample are undecodable (open GOP)
            // and never referenced by what follows; see `seek`.
            if i != t && is_leading_of(self.source.index(), self.chain_start, i) {
                continue;
            }
            self.submit(i, i == t)?;
        }
        self.next = t + 1;
        let tag = t as u64;
        let dec = self.dec.as_mut().ok_or(MediaError {
            step: "frame_reader_no_session",
            frame: Some(tag),
            source_id: None,
            os_status: None,
        })?;
        if let Some(f) = dec.poll()? {
            return check_target(f, tag);
        }
        dec.flush()?;
        match dec.poll()? {
            Some(f) => check_target(f, tag),
            None => Err(MediaError {
                step: "frame_at_pts_no_output",
                frame: Some(tag),
                source_id: None,
                os_status: None,
            }),
        }
    }

    /// Decodes samples `[from, to)` (decode order) from the sync sample at or before
    /// `from`, handing every picture of the range to `f` in presentation order.
    ///
    /// Leading pictures of the file's first sync sample (open GOP with nothing before
    /// it) are undecodable; they are neither expected nor submitted. Any error tears
    /// the session down.
    pub fn decode_range(
        &mut self,
        from: usize,
        to: usize,
        f: impl FnMut(DecodedFrame) -> Result<(), MediaError>,
    ) -> Result<u64, MediaError> {
        let r = self.decode_range_inner(from, to, f);
        if r.is_err() {
            self.dec = None;
        }
        r
    }

    fn decode_range_inner(
        &mut self,
        from: usize,
        to: usize,
        mut f: impl FnMut(DecodedFrame) -> Result<(), MediaError>,
    ) -> Result<u64, MediaError> {
        // Start early enough for every picture of the range: a leading picture near
        // `from` may need the GOP before `from`'s sync sample.
        let idx = self.source.index();
        let mut start = usize::MAX;
        for j in from..to {
            if j > from && idx.sync[j] {
                break;
            }
            if let Some(k) = seek_start(idx, SampleIdx(j as u32)) {
                start = start.min(k);
            }
        }
        if start == usize::MAX {
            return Err(MediaError {
                step: "decode_range_no_sync",
                frame: Some(from as u64),
                source_id: None,
                os_status: None,
            });
        }
        self.restart(start)?;
        let mut reorder: ReorderStage<DecodedFrame> = ReorderStage::new();
        let mut emitted = 0u64;
        let first_sync = self.source.index().sync.iter().position(|&s| s);
        let undecodable = |idx: &crate::index::SampleIndex, i: usize| {
            first_sync.is_some_and(|k| start == k && is_leading_of(idx, k, i))
        };
        // Every picture of the range is announced before any is decoded: a frame is
        // released only when no announced earlier PTS is outstanding.
        for i in from..to {
            if !undecodable(self.source.index(), i) {
                reorder.expect(self.source.pts(SampleIdx(i as u32)));
            }
        }
        for i in start..to {
            let idx = self.source.index();
            if undecodable(idx, i) || (i < from && is_leading_of(idx, start, i)) {
                continue;
            }
            self.submit(i, i >= from)?;
            while let Some(fr) = self.poll_dec()? {
                reorder.push(fr.pts, fr);
            }
            while let Some((_, fr)) = reorder.pop() {
                f(fr)?;
                emitted += 1;
            }
        }
        self.next = to;
        if let Some(dec) = self.dec.as_mut() {
            dec.flush()?;
        }
        while let Some(fr) = self.poll_dec()? {
            reorder.push(fr.pts, fr);
        }
        let missing = reorder.outstanding() as i64 - reorder.len() as i64;
        for (_, fr) in reorder.drain() {
            f(fr)?;
            emitted += 1;
        }
        if missing > 0 {
            return Err(MediaError {
                step: "decode_range_missing_frames",
                frame: None,
                source_id: None,
                os_status: None,
            });
        }
        Ok(emitted)
    }

    fn poll_dec(&mut self) -> Result<Option<DecodedFrame>, MediaError> {
        match self.dec.as_mut() {
            Some(d) => d.poll(),
            None => Ok(None),
        }
    }
}

fn check_target(f: DecodedFrame, tag: u64) -> Result<DecodedFrame, MediaError> {
    if u64::from(f.sample.0) == tag {
        Ok(f)
    } else {
        Err(MediaError {
            step: "frame_at_pts_wrong_frame",
            frame: Some(u64::from(f.sample.0)),
            source_id: None,
            os_status: None,
        })
    }
}
