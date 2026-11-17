//! `Mp4Writer`: plan counts → choose `stco`/`co64` → reserve `moov` → write `mdat`
//! (largesize) → build and patch `moov` in place (implementer Part C, SPEC §6.5 step 4).

use std::io::{Seek, SeekFrom, Write};

use super::moov::{self, Rates, TrackTables};
use super::{Ctts, Gapless, MuxError, MuxPlan, MuxReport, OffsetBoxes, TrackConfig, TrackReport};

/// Bytes of the `mdat` largesize header.
const MDAT_HEADER: u64 = 16;
/// Bytes of the smallest `free` box (the padding after `moov`).
const FREE_HEADER: u64 = 8;

/// One coded sample handed to the writer, in decode order per track.
///
/// `dts` and `pts` are in the track timescale. DTS may start anywhere (an encoder's DTS
/// rebased by its reorder delay starts below 0); the writer subtracts the first DTS so the
/// file's decode timeline starts at 0, and keeps PTS as given, so the first presentation
/// time must already be 0 (no edit lists). `duration` must equal the gap to the next DTS.
#[derive(Clone, Copy, Debug)]
pub struct Sample<'a> {
    pub data: &'a [u8],
    pub dts: i64,
    pub pts: i64,
    pub duration: u32,
    pub sync: bool,
}

/// A sample described by its size only: input to [`plan`] (FZ4) and the writer's core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlannedSample {
    pub track: usize,
    pub size: u32,
    pub dts: i64,
    pub pts: i64,
    pub duration: u32,
    pub sync: bool,
}

#[derive(Debug)]
struct TrackState {
    cfg: TrackConfig,
    planned: u32,
    sizes: Vec<u32>,
    durations: Vec<u32>,
    pts: Vec<i64>,
    sync: Vec<bool>,
    first_dts: Option<i64>,
    next_dts: i64,
    chunk_offsets: Vec<u64>,
    chunk_counts: Vec<u32>,
    payload: u64,
}

/// Writes one MP4 to a seekable sink. See the module docs for the layout rules.
#[derive(Debug)]
pub struct Mp4Writer<W: Write + Seek> {
    w: W,
    tracks: Vec<TrackState>,
    planned_offsets: OffsetBoxes,
    gapless: Option<Gapless>,
    reserve_at: u64,
    reserved: u64,
    mdat_at: u64,
    pos: u64,
    /// Track of the chunk being extended, if the last write was contiguous.
    open_chunk: Option<usize>,
    /// Set when a write to the sink failed: the file no longer matches the tables.
    poisoned: bool,
}

impl<W: Write + Seek> Mp4Writer<W> {
    /// Writes `ftyp`, the `moov` reservation and the `mdat` header.
    pub fn create(mut w: W, tracks: Vec<TrackConfig>, plan: &MuxPlan) -> Result<Self, MuxError> {
        if tracks.is_empty() {
            return Err(MuxError::InvalidTrack {
                track: 0,
                reason: "no tracks".into(),
            });
        }
        if plan.sample_counts.len() != tracks.len() {
            return Err(MuxError::InvalidTrack {
                track: 0,
                reason: format!(
                    "{} sample counts for {} tracks",
                    plan.sample_counts.len(),
                    tracks.len()
                ),
            });
        }
        if tracks.len() > 0xffff {
            return Err(MuxError::InvalidTrack {
                track: 0,
                reason: "more than 65535 tracks".into(),
            });
        }
        for (i, t) in tracks.iter().enumerate() {
            t.validate(i)?;
        }
        let reserved = reservation(&tracks, &plan.sample_counts, plan.gapless.as_ref())?;
        let start = w.stream_position()?;
        let ftyp = moov::ftyp()?;
        let reserve_at = start
            .checked_add(ftyp.len() as u64)
            .ok_or(MuxError::Overflow)?;
        let mdat_at = reserve_at.checked_add(reserved).ok_or(MuxError::Overflow)?;
        let first_sample_at = mdat_at.checked_add(MDAT_HEADER).ok_or(MuxError::Overflow)?;
        let planned_offsets = match plan.max_payload_bytes {
            Some(bound)
                if first_sample_at
                    .checked_add(bound)
                    .is_some_and(|end| end <= u64::from(u32::MAX)) =>
            {
                OffsetBoxes::Stco
            }
            _ => OffsetBoxes::Co64,
        };
        w.write_all(&ftyp)?;
        write_free(&mut w, reserved)?;
        w.write_all(&1u32.to_be_bytes())?;
        w.write_all(b"mdat")?;
        w.write_all(&0u64.to_be_bytes())?;
        let tracks = tracks
            .into_iter()
            .zip(&plan.sample_counts)
            .map(|(cfg, &planned)| TrackState {
                cfg,
                planned,
                sizes: Vec::new(),
                durations: Vec::new(),
                pts: Vec::new(),
                sync: Vec::new(),
                first_dts: None,
                next_dts: 0,
                chunk_offsets: Vec::new(),
                chunk_counts: Vec::new(),
                payload: 0,
            })
            .collect();
        Ok(Self {
            w,
            tracks,
            planned_offsets,
            gapless: plan.gapless,
            reserve_at,
            reserved,
            mdat_at,
            pos: first_sample_at,
            open_chunk: None,
            poisoned: false,
        })
    }

    /// Appends one sample to `mdat`.
    pub fn write_sample(&mut self, track: usize, s: &Sample<'_>) -> Result<(), MuxError> {
        let size = u32::try_from(s.data.len()).map_err(|_| MuxError::InvalidSample {
            track,
            sample: self.tracks.get(track).map_or(0, |t| t.count()),
            reason: "sample larger than 4 GiB".into(),
        })?;
        if self.poisoned {
            return Err(MuxError::Internal("an earlier write failed"));
        }
        let meta = PlannedSample {
            track,
            size,
            dts: s.dts,
            pts: s.pts,
            duration: s.duration,
            sync: s.sync,
        };
        self.account(&meta)?;
        if let Err(e) = self.w.write_all(s.data) {
            self.poisoned = true;
            return Err(e.into());
        }
        Ok(())
    }

    /// Records a sample's metadata and advances the file position; the caller writes
    /// exactly `size` bytes next (or, for [`plan`], seeks past them). Validates before it
    /// mutates anything, so a refused sample leaves the writer usable.
    fn account(&mut self, s: &PlannedSample) -> Result<(), MuxError> {
        let at = self.pos;
        let open = self.open_chunk;
        let t = self
            .tracks
            .get_mut(s.track)
            .ok_or_else(|| MuxError::InvalidTrack {
                track: s.track,
                reason: "no such track".into(),
            })?;
        let n = t.count();
        if n >= t.planned {
            return Err(MuxError::OverPlan {
                track: s.track,
                planned: t.planned,
            });
        }
        let bad = |reason: String| MuxError::InvalidSample {
            track: s.track,
            sample: n,
            reason,
        };
        if s.duration == 0 {
            return Err(bad("duration 0".into()));
        }
        if n == 0 && t.cfg.is_video() && !s.sync {
            return Err(bad("a video track must start on a sync sample".into()));
        }
        if t.first_dts.is_some() && s.dts != t.next_dts {
            return Err(bad(format!(
                "dts {} but the previous sample ends at {}",
                s.dts, t.next_dts
            )));
        }
        let first_dts = t.first_dts.unwrap_or(s.dts);
        let next_dts = s
            .dts
            .checked_add(i64::from(s.duration))
            .ok_or(MuxError::Overflow)?;
        // File DTS = dts − first dts; the composition offset must fit `ctts` (32 bits).
        let file_dts = s.dts.checked_sub(first_dts).ok_or(MuxError::Overflow)?;
        let off = s.pts.checked_sub(file_dts).ok_or(MuxError::Overflow)?;
        if i32::try_from(off).is_err() {
            return Err(bad(format!(
                "composition offset {off} does not fit 32 bits"
            )));
        }
        let payload = t
            .payload
            .checked_add(u64::from(s.size))
            .ok_or(MuxError::Overflow)?;
        let pos = at
            .checked_add(u64::from(s.size))
            .ok_or(MuxError::Overflow)?;
        let extend = open == Some(s.track) && !t.chunk_counts.is_empty();
        if extend && t.chunk_counts.last().is_some_and(|&c| c == u32::MAX) {
            return Err(MuxError::Overflow);
        }
        t.first_dts = Some(first_dts);
        t.next_dts = next_dts;
        t.sizes.push(s.size);
        t.durations.push(s.duration);
        t.pts.push(s.pts);
        t.sync.push(s.sync);
        t.payload = payload;
        match t.chunk_counts.last_mut() {
            Some(c) if extend => *c += 1,
            _ => {
                t.chunk_offsets.push(at);
                t.chunk_counts.push(1);
            }
        }
        self.open_chunk = Some(s.track);
        self.pos = pos;
        Ok(())
    }

    /// Builds `moov`, writes it into the reservation and patches the `mdat` size.
    pub fn finish(mut self) -> Result<(W, MuxReport), MuxError> {
        if self.poisoned {
            return Err(MuxError::Internal("an earlier write failed"));
        }
        let (moov_bytes, report) = self.build()?;
        let moov_len = moov_bytes.len() as u64;
        let pad = self
            .reserved
            .checked_sub(moov_len)
            .ok_or(MuxError::Internal("moov outgrew its reservation"))?;
        if pad < FREE_HEADER {
            return Err(MuxError::Internal("no room for the free box"));
        }
        let end = self.pos;
        self.w.seek(SeekFrom::Start(self.reserve_at))?;
        self.w.write_all(&moov_bytes)?;
        write_free_header(&mut self.w, pad)?;
        self.w.seek(SeekFrom::Start(self.mdat_at + 8))?;
        let mdat_size = end.checked_sub(self.mdat_at).ok_or(MuxError::Overflow)?;
        self.w.write_all(&mdat_size.to_be_bytes())?;
        self.w.seek(SeekFrom::Start(end))?;
        self.w.flush()?;
        Ok((self.w, report))
    }

    fn build(&self) -> Result<(Vec<u8>, MuxReport), MuxError> {
        let mut offsets_per_track: Vec<Vec<i64>> = Vec::with_capacity(self.tracks.len());
        let mut durations: Vec<u64> = Vec::with_capacity(self.tracks.len());
        for (i, t) in self.tracks.iter().enumerate() {
            if t.sizes.is_empty() {
                return Err(MuxError::EmptyTrack { track: i });
            }
            let first_pts = t.pts.iter().copied().min().unwrap_or(0);
            if first_pts != 0 {
                return Err(MuxError::FirstPtsNotZero {
                    track: i,
                    first_pts,
                });
            }
            let mut file_dts = 0i64;
            let mut offs = Vec::with_capacity(t.pts.len());
            for (pts, d) in t.pts.iter().zip(&t.durations) {
                offs.push(pts.checked_sub(file_dts).ok_or(MuxError::Overflow)?);
                file_dts = file_dts
                    .checked_add(i64::from(*d))
                    .ok_or(MuxError::Overflow)?;
            }
            offsets_per_track.push(offs);
            durations.push(u64::try_from(file_dts).map_err(|_| MuxError::Overflow)?);
        }
        let max_off = self
            .tracks
            .iter()
            .flat_map(|t| t.chunk_offsets.iter().copied())
            .max()
            .unwrap_or(0);
        let offsets = if self.planned_offsets == OffsetBoxes::Stco && max_off <= u64::from(u32::MAX)
        {
            OffsetBoxes::Stco
        } else {
            OffsetBoxes::Co64
        };
        let tables: Vec<TrackTables<'_>> = self
            .tracks
            .iter()
            .zip(&offsets_per_track)
            .zip(&durations)
            .enumerate()
            .map(|(i, ((t, offs), &dur))| TrackTables {
                id: u32::try_from(i + 1).unwrap_or(u32::MAX),
                cfg: &t.cfg,
                sizes: &t.sizes,
                durations: &t.durations,
                cts_offsets: offs,
                sync: &t.sync,
                chunk_offsets: &t.chunk_offsets,
                chunk_counts: &t.chunk_counts,
                media_duration: dur,
                rates: rates(t, dur),
            })
            .collect();
        let bytes = moov::moov(
            &tables,
            self.gapless.as_ref(),
            offsets == OffsetBoxes::Co64,
            false,
        )?;
        let reports = tables
            .iter()
            .zip(&self.tracks)
            .map(|(tt, t)| {
                let ctts = tt.ctts_kind();
                Ok(TrackReport {
                    samples: t.count(),
                    chunks: u32::try_from(t.chunk_counts.len()).map_err(|_| MuxError::Overflow)?,
                    duration: tt.media_duration,
                    timescale: t.cfg.timescale,
                    ctts,
                    cslg: ctts != Ctts::None,
                    stss: tt.needs_stss(),
                    min_cts_offset: tt.cts_offsets.iter().copied().min().unwrap_or(0),
                    max_cts_offset: tt.cts_offsets.iter().copied().max().unwrap_or(0),
                    payload_bytes: t.payload,
                })
            })
            .collect::<Result<Vec<_>, MuxError>>()?;
        let report = MuxReport {
            planned_offsets: self.planned_offsets,
            offsets,
            reserved: self.reserved,
            moov_size: bytes.len() as u64,
            mdat_offset: self.mdat_at,
            mdat_size: self
                .pos
                .checked_sub(self.mdat_at)
                .ok_or(MuxError::Overflow)?,
            tracks: reports,
        };
        Ok((bytes, report))
    }
}

impl TrackState {
    fn count(&self) -> u32 {
        // `planned` is a u32 and pushes stop there, so this never saturates.
        u32::try_from(self.sizes.len()).unwrap_or(u32::MAX)
    }
}

/// `btrt`/`esds` rates: average over the track, maximum over any 1-second window of
/// decode time, buffer = largest sample.
fn rates(t: &TrackState, duration: u64) -> Rates {
    let ts = u64::from(t.cfg.timescale.max(1));
    let avg = if duration == 0 {
        0
    } else {
        u128::from(t.payload) * 8 * u128::from(ts) / u128::from(duration)
    };
    // Max over sliding windows of ≤ 1 s of decode time. A window shorter than 1 s counts
    // its bits per second as they are (a 1 s window holding it carries at least that); a
    // single sample longer than 1 s is scaled to its own duration.
    let mut max_bits = 0u128;
    let (mut lo, mut window_bytes, mut dts_lo, mut dts_hi) = (0usize, 0u128, 0u64, 0u64);
    for (i, (&size, &d)) in t.sizes.iter().zip(&t.durations).enumerate() {
        window_bytes += u128::from(size);
        dts_hi += u64::from(d);
        while dts_hi.saturating_sub(dts_lo) > ts && lo < i {
            window_bytes -= t.sizes.get(lo).copied().map_or(0, u128::from);
            dts_lo += t.durations.get(lo).copied().map_or(0, u64::from);
            lo += 1;
        }
        let span = u128::from(dts_hi.saturating_sub(dts_lo).max(ts));
        max_bits = max_bits.max(window_bytes * 8 * u128::from(ts) / span);
    }
    Rates {
        buffer_size_db: t.sizes.iter().copied().max().unwrap_or(0),
        max_bitrate: u32::try_from(max_bits.max(avg)).unwrap_or(u32::MAX),
        avg_bitrate: u32::try_from(avg).unwrap_or(u32::MAX),
    }
}

/// Bytes reserved for `moov` + `free`: the empty-table `moov` with 64-bit headers, plus
/// every table at its worst case for the planned counts, plus one `free` header.
fn reservation(
    tracks: &[TrackConfig],
    counts: &[u32],
    gapless: Option<&Gapless>,
) -> Result<u64, MuxError> {
    let empty: Vec<TrackTables<'_>> = tracks
        .iter()
        .enumerate()
        .map(|(i, cfg)| TrackTables {
            id: u32::try_from(i + 1).unwrap_or(u32::MAX),
            cfg,
            sizes: &[],
            durations: &[],
            cts_offsets: &[],
            sync: &[],
            chunk_offsets: &[],
            chunk_counts: &[],
            media_duration: 0,
            rates: Rates::default(),
        })
        .collect();
    let base = moov::moov(&empty, gapless, true, true)?.len() as u64;
    let mut total = base.checked_add(FREE_HEADER).ok_or(MuxError::Overflow)?;
    for &n in counts {
        let worst = moov::worst_table_bytes(u64::from(n)).ok_or(MuxError::Overflow)?;
        total = total.checked_add(worst).ok_or(MuxError::Overflow)?;
    }
    if total > u64::from(u32::MAX) {
        return Err(MuxError::BoxTooLarge);
    }
    Ok(total)
}

/// A `free` box of `size` bytes (header + zeros), written in 64 KiB pieces.
fn write_free<W: Write>(w: &mut W, size: u64) -> Result<(), MuxError> {
    write_free_header(w, size)?;
    let zeros = [0u8; 65_536];
    let mut left = size - FREE_HEADER;
    while left > 0 {
        let n = usize::try_from(left.min(zeros.len() as u64)).unwrap_or(zeros.len());
        w.write_all(zeros.get(..n).unwrap_or(&zeros))?;
        left -= n as u64;
    }
    Ok(())
}

fn write_free_header<W: Write>(w: &mut W, size: u64) -> Result<(), MuxError> {
    let s = u32::try_from(size).map_err(|_| MuxError::BoxTooLarge)?;
    w.write_all(&s.to_be_bytes())?;
    w.write_all(b"free")?;
    Ok(())
}

/// A seekable sink that keeps no bytes: [`plan`] lays a file out on it.
#[derive(Debug, Default)]
pub struct NullSink {
    pos: u64,
    len: u64,
}

impl Write for NullSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.pos = self.pos.saturating_add(buf.len() as u64);
        self.len = self.len.max(self.pos);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for NullSink {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let p = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos = p.ok_or_else(|| std::io::Error::other("seek out of range"))?;
        self.len = self.len.max(self.pos);
        Ok(self.pos)
    }
}

/// A file laid out by [`plan`]: the real `moov` bytes and where everything lands.
#[derive(Clone, Debug)]
pub struct PlannedFile {
    pub ftyp: Vec<u8>,
    pub moov: Vec<u8>,
    pub report: MuxReport,
    /// Absolute offset of each input sample's bytes, in input order.
    pub sample_offsets: Vec<u64>,
    pub file_size: u64,
}

/// Lays out an MP4 from sample metadata alone (no payload bytes): the same reservation,
/// chunking, `stco`/`co64` choice and `moov` the writer would produce. FZ4's target.
/// `max_payload_bytes` defaults to the exact sum of the sizes.
pub fn plan(
    tracks: &[TrackConfig],
    samples: &[PlannedSample],
    max_payload_bytes: Option<u64>,
) -> Result<PlannedFile, MuxError> {
    let mut counts = vec![0u32; tracks.len()];
    let mut sum = 0u64;
    for s in samples {
        let c = counts
            .get_mut(s.track)
            .ok_or_else(|| MuxError::InvalidTrack {
                track: s.track,
                reason: "no such track".into(),
            })?;
        *c = c.checked_add(1).ok_or(MuxError::Overflow)?;
        sum = sum
            .checked_add(u64::from(s.size))
            .ok_or(MuxError::Overflow)?;
    }
    let mplan = MuxPlan {
        gapless: None,
        sample_counts: counts,
        max_payload_bytes: Some(max_payload_bytes.unwrap_or(sum)),
    };
    let mut w = Mp4Writer::create(NullSink::default(), tracks.to_vec(), &mplan)?;
    let mut sample_offsets = Vec::with_capacity(samples.len());
    for s in samples {
        sample_offsets.push(w.pos);
        w.account(s)?;
        let to = w.pos;
        w.w.seek(SeekFrom::Start(to))?;
    }
    let (moov_bytes, _) = w.build()?;
    let ftyp = moov::ftyp()?;
    let (sink, report) = w.finish()?;
    Ok(PlannedFile {
        ftyp,
        moov: moov_bytes,
        report,
        sample_offsets,
        file_size: sink.len,
    })
}
