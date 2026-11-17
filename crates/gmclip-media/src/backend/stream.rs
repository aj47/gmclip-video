//! Presentation-order frame access for export (H-10): "give me presentation frame
//! `n` of this source", mostly in increasing order (a part plays its frames in order,
//! a join jumps), with frame-exact seeks when the request is not reachable by decoding
//! forward.
//!
//! Frames are decoded in decode order with every picture output and kept by PTS; a
//! request returns its picture as soon as it is decoded (a picture is complete when
//! the decoder hands it out — ordering only matters for eviction, and pictures before
//! the last request are dropped). A request for a picture still in flight on the
//! current chain waits for it; one behind the last request, or far ahead, seeks.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::backend::seek::{is_leading_of, seek_start};
use crate::backend::source::SampleSource;
use crate::decode::{DecodedFrame, EncodedSample, MediaError, SubmitFlags, VideoDecoder};
use crate::index::SampleIdx;

fn err(step: &'static str, frame: Option<u64>) -> MediaError {
    MediaError {
        step,
        frame,
        source_id: None,
        os_status: None,
    }
}

/// Samples submitted past the requested one, so the hardware decodes the next pictures
/// while the caller composites this one.
pub const LOOKAHEAD: usize = 3;

/// Sequential, seekable presentation-order access to one video track.
pub struct FrameStream<S: SampleSource, D: VideoDecoder> {
    source: S,
    make_decoder: Box<dyn FnMut() -> Result<D, MediaError> + Send>,
    dec: Option<D>,
    /// The PTS of the last picture handed out (older ones are evicted).
    last_returned: Option<i64>,
    /// Next decode-order sample to submit.
    next: usize,
    /// Sync sample the current chain started at.
    chain_start: usize,
    /// Decoded pictures not yet final or not yet consumed, by PTS.
    pending: BTreeMap<i64, Arc<DecodedFrame>>,
    buf: Vec<u8>,
    /// Samples submitted past each request ([`LOOKAHEAD`] unless set).
    lookahead: usize,
    /// Seeks performed (new chains), for the export report.
    pub seeks: u64,
    /// Samples submitted (pre-roll included).
    pub submitted: u64,
    /// Decoder flushes: each one waits for every picture in flight (a sequential read
    /// that flushes has drained its look-ahead).
    pub flushes: u64,
}

impl<S: SampleSource, D: VideoDecoder> std::fmt::Debug for FrameStream<S, D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameStream")
            .field("next", &self.next)
            .field("pending", &self.pending.len())
            .field("seeks", &self.seeks)
            .finish_non_exhaustive()
    }
}

impl<S: SampleSource, D: VideoDecoder> FrameStream<S, D> {
    pub fn new(
        source: S,
        make_decoder: impl FnMut() -> Result<D, MediaError> + Send + 'static,
    ) -> Self {
        Self {
            source,
            make_decoder: Box::new(make_decoder),
            dec: None,
            last_returned: None,
            next: 0,
            chain_start: 0,
            pending: BTreeMap::new(),
            buf: Vec::new(),
            lookahead: LOOKAHEAD,
            seeks: 0,
            submitted: 0,
            flushes: 0,
        }
    }

    pub fn source(&self) -> &S {
        &self.source
    }

    /// Samples submitted past each request. A synchronous decoder (VP9) gains nothing
    /// from look-ahead — each submit blocks until decoded — and a seek pays for it, so
    /// its streams set 0 (H.05: ~9 ms of a 1440p VP9 seek).
    pub fn set_lookahead(&mut self, n: usize) {
        self.lookahead = n;
    }

    /// Number of presentation frames.
    pub fn len(&self) -> usize {
        self.source.index().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn pts_of(&self, i: usize) -> i64 {
        let idx = self.source.index();
        idx.dts[i] + i64::from(idx.cts_off[i])
    }

    fn restart(&mut self, start: usize) -> Result<(), MediaError> {
        self.dec = None;
        self.pending.clear();
        self.last_returned = None;
        self.dec = Some((self.make_decoder)()?);
        self.next = start;
        self.chain_start = start;
        self.seeks += 1;
        Ok(())
    }

    fn submit_next(&mut self, target_pts: i64) -> Result<(), MediaError> {
        let i = self.next;
        self.next += 1;
        // Pictures before the target are pre-roll: decoded for reference, never output
        // or held (requests go forward; a backward one re-seeks).
        let no_output = self.pts_of(i) < target_pts;
        // Leading pictures of the chain's sync sample are undecodable (open GOP) and
        // never referenced later; skip them unless they are the target.
        if self.pts_of(i) != target_pts && is_leading_of(self.source.index(), self.chain_start, i) {
            return Ok(());
        }
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
        let dec = self
            .dec
            .as_mut()
            .ok_or(err("stream_no_session", Some(i as u64)))?;
        dec.submit(&sample, SubmitFlags { no_output })?;
        self.submitted += 1;
        self.collect()
    }

    /// Presentation frame `n` (0-based, in PTS order), decoded frame-exactly.
    pub fn frame(&mut self, n: u64) -> Result<Arc<DecodedFrame>, MediaError> {
        let r = self.frame_inner(n);
        if r.is_err() {
            self.dec = None; // never continue a chain in an unknown state
            self.pending.clear();
        }
        r
    }

    fn collect(&mut self) -> Result<(), MediaError> {
        if let Some(d) = self.dec.as_mut() {
            while let Some(f) = d.poll()? {
                self.pending.insert(f.pts, Arc::new(f));
            }
        }
        Ok(())
    }

    fn frame_inner(&mut self, n: u64) -> Result<Arc<DecodedFrame>, MediaError> {
        let idx = self.source.index();
        let t = *idx
            .pts_order
            .get(n as usize)
            .ok_or(err("stream_frame_out_of_range", Some(n)))? as usize;
        let target_pts = self.pts_of(t);
        self.collect()?;
        if let Some(f) = self.pending.get(&target_pts).cloned() {
            // keep the look-ahead full (`VideoDecoder::needs_input_to_output`), so the
            // decoder works while the caller does and the next requests rarely flush
            if self.dec.as_ref().is_some_and(|d| d.needs_input_to_output()) {
                let ahead = (t + 1 + self.lookahead).min(self.len());
                while self.next < ahead {
                    self.submit_next(target_pts)?;
                }
                self.collect()?;
            }
            self.evict_before(target_pts);
            return Ok(f);
        }
        let idx = self.source.index();
        let start = seek_start(idx, SampleIdx(t as u32))
            .ok_or(err("undecodable_leading_picture", Some(n)))?;
        let in_chain = self.dec.is_some() && self.chain_start <= start && start <= self.next;
        let submitted = t < self.next;
        // Already submitted on this chain and not consumed: its picture is still in
        // flight (asynchronous decode) — wait for it instead of seeking.
        if in_chain && submitted && self.last_returned.is_none_or(|l| target_pts > l) {
            self.wait_for(target_pts)?;
            if let Some(f) = self.pending.get(&target_pts).cloned() {
                self.evict_before(target_pts);
                return Ok(f);
            }
        }
        // Decoding forward from `next` never costs more than a restart from `start`
        // (`start <= next`), so any target ahead on the chain is reached forward.
        let reachable = in_chain && !submitted;
        if !reachable {
            self.restart(start)?;
        }
        while self.next <= t {
            self.submit_next(target_pts)?;
        }
        let ahead = (t + 1 + self.lookahead).min(self.len());
        while self.next < ahead {
            self.submit_next(target_pts)?;
        }
        self.wait_for(target_pts)?;
        let f = self
            .pending
            .get(&target_pts)
            .cloned()
            .ok_or(err("stream_frame_not_decoded", Some(n)))?;
        self.evict_before(target_pts);
        Ok(f)
    }

    /// Collects until the picture at `pts` is in: one decoder output at a time where the
    /// decoder can wait for one (the look-ahead keeps decoding meanwhile), else a flush.
    fn wait_for(&mut self, pts: i64) -> Result<(), MediaError> {
        loop {
            self.collect()?;
            if self.pending.contains_key(&pts) {
                return Ok(());
            }
            let waited = match self.dec.as_mut() {
                Some(d) => d.wait_output()?,
                None => false,
            };
            if !waited {
                break;
            }
        }
        self.flush()?;
        self.collect()
    }

    fn flush(&mut self) -> Result<(), MediaError> {
        if let Some(d) = self.dec.as_mut() {
            d.flush()?;
            self.flushes += 1;
        }
        Ok(())
    }

    /// Drops pictures before `pts` (requests mostly go forward; a backward request
    /// re-seeks) and remembers `pts` as the last one handed out.
    fn evict_before(&mut self, pts: i64) {
        self.pending = self.pending.split_off(&pts);
        self.last_returned = Some(pts);
    }
}
