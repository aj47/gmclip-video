//! Fuzz harness bodies (B-17, SR §3.2), shared by the `fuzz/` targets and the
//! regression tests that replay committed crashers on hosted CI. A failed assertion
//! here is the crash signal the fuzzer looks for; everything they call must return
//! errors, never panic.
#![allow(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used
)]

use crate::codec::{ColourTags, h264, vp9};
use crate::demux::{
    self, DemuxError, Demuxed, MAX_SAMPLES, View,
    io::{ByteSource, SliceSource},
};

/// A slice source that counts the bytes the parser reads (SR §3.2: work ≤ 4× input).
struct Counting<'a> {
    inner: SliceSource<'a>,
    read: u64,
}

impl ByteSource for Counting<'_> {
    fn len(&self) -> u64 {
        self.inner.len()
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DemuxError> {
        self.read += buf.len() as u64;
        self.inner.read_at(offset, buf)
    }
}

/// Bytes a parse of `len` input bytes may read: 4× the input plus one resync window
/// and one metadata read of slack.
pub fn work_budget(len: usize) -> u64 {
    4 * len as u64 + 256 * 1024
}

fn counted<T>(data: &[u8], f: impl FnOnce(&mut Counting<'_>) -> T) -> T {
    let mut src = Counting {
        inner: SliceSource(data),
        read: 0,
    };
    let out = f(&mut src);
    assert!(
        src.read <= work_budget(data.len()),
        "read {} bytes of a {}-byte input",
        src.read,
        data.len()
    );
    out
}

/// The invariants every successful index must hold.
fn check(d: &Demuxed, input_len: usize) {
    let mut total = 0u64;
    for t in &d.tracks {
        let s = &t.samples;
        let n = s.len();
        total += n as u64;
        assert!(n as u64 <= MAX_SAMPLES, "track over 2^26 samples");
        for len in [
            s.cts_off.len(),
            s.offset.len(),
            s.size.len(),
            s.sync.len(),
            s.pts_order.len(),
            t.durations.len(),
        ] {
            assert_eq!(len, n, "ragged index");
        }
        // decode times never go backwards
        assert!(s.dts.windows(2).all(|w| w[0] <= w[1]), "dts not monotone");
        // presentation order is a permutation sorted by pts
        let mut seen = vec![false; n];
        for &i in &s.pts_order {
            assert!(
                !std::mem::replace(&mut seen[i as usize], true),
                "pts_order repeats"
            );
        }
        let pts = |i: u32| s.dts[i as usize].saturating_add(i64::from(s.cts_off[i as usize]));
        assert!(
            s.pts_order.windows(2).all(|w| pts(w[0]) <= pts(w[1])),
            "pts_order unsorted"
        );
        for len in [t.pts_unknown.len(), t.block_pos.len()] {
            assert!(len == 0 || len == n, "ragged Matroska columns");
        }
        for view in [View::Raw, View::Edited] {
            if let Ok(p) = t.packets(view) {
                assert!(p.len() <= n, "more packets than samples");
                assert!(
                    p.windows(2)
                        .all(|w| w[0].dts.unwrap_or(i64::MIN) <= w[1].dts.unwrap_or(i64::MAX)),
                    "{view:?} dts not monotone"
                );
            }
        }
    }
    assert!(
        total <= demux::entry_budget(input_len as u64),
        "entry budget exceeded"
    );
}

/// FZ1 body.
pub fn mp4(data: &[u8]) {
    if let Ok(d) = counted(data, |s| demux::mp4::index(s)) {
        check(&d, data.len());
    }
}

/// FZ2 body.
pub fn mkv(data: &[u8]) {
    if let Ok(d) = counted(data, |s| demux::mkv::index(s)) {
        check(&d, data.len());
        for c in &d.cues {
            assert!(c.cluster < data.len() as u64, "cue points past the file");
        }
    }
}

fn roundtrip(h: &vp9::Header, seed: u8) {
    if !h.key_frame {
        return;
    }
    // Vary the frame rate and container tags from the input too.
    let fps = [24.0, 30.0, 60.0, 120.0][usize::from(seed & 3)];
    let colour = ColourTags {
        primaries: (seed & 4 != 0).then_some(seed),
        transfer: (seed & 8 != 0).then_some(seed.wrapping_mul(3)),
        matrix: (seed & 16 != 0).then_some(seed.wrapping_mul(5)),
        full_range: (seed & 32 != 0).then_some(seed & 64 != 0),
        chroma_left: (seed & 128 != 0).then_some(seed & 1 != 0),
    };
    if let Ok(v) = vp9::synthesize_vpcc(h, fps, &colour) {
        let back = vp9::parse_vpcc(&v).expect("a synthesised vpcC parses");
        assert_eq!(back.profile, h.profile);
        let (w, ht) = (h.width.unwrap_or(0), h.height.unwrap_or(0));
        assert_eq!(back.level, vp9::level(w, ht, fps));
        let chroma = match h.subsampling {
            Some((1, 1)) if colour.chroma_left == Some(true) => 0,
            Some((1, 1)) => 1,
            Some((1, 0)) => 2,
            _ => 3,
        };
        assert_eq!(back.chroma, chroma);
        assert_eq!(back.primaries, colour.primaries.unwrap_or(2));
        assert_eq!(back.transfer, colour.transfer.unwrap_or(2));
        let matrix = colour
            .matrix
            .unwrap_or_else(|| vp9::matrix_for(h.color_space.unwrap_or(0)));
        assert_eq!(back.matrix, matrix);
        assert_eq!(&v[10..], &[0, 0]);
        assert_eq!(Some(back.bit_depth), h.bit_depth);
        assert_eq!(
            back.full_range,
            colour.full_range.or(h.full_range).unwrap_or(false)
        );
    }
}

/// FZ3 body: the first byte picks the reader.
pub fn headers(data: &[u8]) {
    let Some((&sel, rest)) = data.split_first() else {
        return;
    };
    match sel % 4 {
        0 => {
            if let Ok(c) = h264::parse_avcc(rest) {
                for s in &c.sps {
                    if let Ok(sps) = h264::parse_sps(s) {
                        assert!(sps.reorder_depth() <= 16);
                        assert!(sps.reorder_depth_with(Some(sel)) <= 16);
                    }
                }
            }
        }
        1 => {
            if let Ok(sps) = h264::parse_sps(rest) {
                assert!(sps.reorder_depth() <= 16);
                assert!(sps.reorder_depth_with(Some(sel)) <= 16);
            }
        }
        2 => {
            if let Ok(frames) = vp9::superframe(rest) {
                for (o, n) in frames {
                    assert!(o + n <= rest.len(), "superframe split out of bounds");
                    if let Ok(h) = vp9::parse_header(&rest[o..o + n]) {
                        roundtrip(&h, sel);
                    }
                }
            }
        }
        _ => {
            if let Ok(h) = vp9::parse_header(rest) {
                roundtrip(&h, sel);
            }
            let _ = vp9::parse_vpcc(rest);
        }
    }
}
