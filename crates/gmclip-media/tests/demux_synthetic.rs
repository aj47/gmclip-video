//! B-01 / B-02 / B-18 on synthetic files built in memory: runs on hosted CI (no media).

#[path = "demux_common/mod.rs"]
mod common;

use common::build::{self, Trak};
use gmclip_media::demux::{self, Codec, DemuxError, TrackKind, View, io::SliceSource};

fn index(bytes: &[u8]) -> Result<demux::Demuxed, DemuxError> {
    demux::index(&mut SliceSource(bytes))
}

fn invalid_budget(r: Result<demux::Demuxed, DemuxError>) -> String {
    match r {
        Err(DemuxError::Invalid { reason, .. }) => reason,
        Err(e) => panic!("expected Invalid, got {e}"),
        Ok(_) => panic!("expected Invalid, got an index"),
    }
}

#[test]
fn mp4_basic_index() {
    let f = build::mp4(&[Trak::video(10)]);
    let d = index(&f).unwrap();
    assert_eq!(d.tracks.len(), 1);
    let t = &d.tracks[0];
    assert_eq!(t.kind, TrackKind::Video);
    assert!(matches!(&t.codec, Codec::H264 { avcc } if avcc.len() == 7));
    assert_eq!((t.width, t.height, t.timescale), (64, 48, 15360));
    assert_eq!(t.samples.len(), 10);
    assert_eq!(t.samples.dts[3], 3 * 512);
    assert_eq!(t.samples.size[3], 103);
    assert_eq!(t.samples.offset[1], t.samples.offset[0] + 100);
    assert_eq!(t.samples.sync.iter().filter(|&&s| s).count(), 1);
    assert_eq!(t.samples.reorder_depth, 0);
    assert_eq!(t.samples.cfr.map(|r| (r.num, r.den)), Some((30, 1)));
}

#[test]
fn mp4_bframes_reorder_and_edit_list() {
    // decode order I0 P4 B2 b1 b3 (pyramid): cts so pts = 0,4,2,1,3 frames (+2 delay)
    let mut t = Trak::video(5);
    t.cts = Some(vec![1024, 2560, 1024, 0, 512]);
    t.edits = Some(vec![(0, 1024)]);
    let d = index(&build::mp4(&[t])).unwrap();
    let tr = &d.tracks[0];
    assert_eq!(tr.samples.reorder_depth, 2);
    assert_eq!(tr.samples.pts_order, vec![0, 3, 2, 4, 1]);
    let edited = tr.packets(View::Edited).unwrap();
    let pts: Vec<_> = edited.iter().map(|p| p.pts.unwrap()).collect();
    assert_eq!(pts, vec![0, 2048, 1024, 512, 1536]);
    assert!(edited.iter().all(|p| !p.discard));
    assert_eq!(edited[0].dts, Some(-1024));
}

#[test]
fn mp4_empty_edit_shifts_presentation() {
    let mut t = Trak::video(3);
    t.edits = Some(vec![(500, -1), (0, 0)]);
    let d = index(&build::mp4(&[t])).unwrap();
    let p = d.tracks[0].packets(View::Edited).unwrap();
    assert_eq!(p[0].pts, Some(7680));
}

#[test]
fn mp4_two_media_edits_are_unsupported() {
    let mut t = Trak::video(3);
    t.edits = Some(vec![(10, 0), (10, 512)]);
    let d = index(&build::mp4(&[t])).unwrap();
    assert!(matches!(
        d.tracks[0].packets(View::Edited),
        Err(DemuxError::Unsupported { .. })
    ));
    assert_eq!(d.tracks[0].packets(View::Raw).unwrap().len(), 3);
}

#[test]
fn mp4_pcm_is_grouped_per_chunk_in_1024_frame_packets() {
    let t = Trak {
        video: false,
        timescale: 48000,
        sizes: vec![4; 3000],
        deltas: vec![1; 3000],
        cts: None,
        sync: None,
        edits: None,
        per_chunk: 2500,
        constant: true,
        ..Trak::video(0)
    };
    let d = index(&build::mp4(&[t])).unwrap();
    let tr = &d.tracks[0];
    assert!(matches!(
        tr.codec,
        Codec::Pcm {
            bits: 16,
            little_endian: true,
            float: false
        }
    ));
    assert_eq!(tr.samples.size, vec![4096, 4096, 1808, 2000]);
    assert_eq!(tr.samples.dts, vec![0, 1024, 2048, 2500]);
    assert_eq!(tr.samples.offset[1], tr.samples.offset[0] + 4096);
}

#[test]
fn mp4_co64_stz2_and_multi_entry_stsc() {
    for (co64, stz2) in [
        (true, None),
        (false, Some(8)),
        (false, Some(16)),
        (true, Some(4)),
    ] {
        let mut t = Trak::video(7);
        t.sizes = vec![3, 1, 4, 1, 5, 9, 2];
        t.co64 = co64;
        t.stz2 = stz2;
        t.chunk_pattern = Some(vec![2, 2, 1, 2]);
        let f = build::mp4(&[t]);
        let d = index(&f).unwrap();
        let s = &d.tracks[0].samples;
        assert_eq!(
            s.size,
            vec![3, 1, 4, 1, 5, 9, 2],
            "co64 {co64} stz2 {stz2:?}"
        );
        // chunks: [3,1] [4,1] [5] [9,2], contiguous in mdat
        let base = s.offset[0];
        assert_eq!(
            s.offset,
            vec![
                base,
                base + 3,
                base + 4,
                base + 8,
                base + 9,
                base + 14,
                base + 23
            ]
        );
    }
}

#[test]
fn mp4_fragmented_is_unsupported() {
    let mut f = build::mp4(&[Trak::video(2)]);
    f.extend(build::bx(b"moof", &[]));
    assert!(matches!(index(&f), Err(DemuxError::Unsupported { .. })));
}

#[test]
fn mp4_every_truncation_fails_cleanly() {
    let f = build::mp4(&[Trak::video(6)]);
    for n in 0..f.len() {
        let _ = index(&f[..n]); // must not panic
    }
}

#[test]
fn mp4_every_corrupted_byte_fails_cleanly() {
    let f = build::mp4(&[Trak::video(6)]);
    let moov = f.windows(4).position(|w| w == b"moov").unwrap();
    for i in moov.saturating_sub(4)..f.len() {
        for v in [0x00, 0x7F, 0xFF] {
            let mut g = f.clone();
            g[i] = v;
            if let Ok(d) = index(&g) {
                for t in &d.tracks {
                    let _ = t.packets(View::Edited);
                }
            }
        }
    }
}

// ---------- Matroska ----------

fn three_frames(cues: bool) -> Vec<u8> {
    let c = build::cluster(
        0,
        &[
            build::simple_block(0, 0x80, &[1; 10]),
            build::simple_block(33, 0x00, &[2; 20]),
            build::simple_block(67, 0x00, &[3; 30]),
        ],
    );
    build::mkv(&[c], cues)
}

#[test]
fn mkv_basic_index() {
    let f = three_frames(true);
    let d = index(&f).unwrap();
    assert_eq!(d.tracks.len(), 1);
    let t = &d.tracks[0];
    assert!(matches!(t.codec, Codec::Vp9 { vpcc: None }));
    assert_eq!((t.width, t.height, t.timescale), (64, 48, 1000));
    assert_eq!(t.samples.dts, vec![0, 33, 67]);
    assert_eq!(t.samples.size, vec![10, 20, 30]);
    assert_eq!(t.samples.sync, vec![true, false, false]);
    let p = t.packets(View::Edited).unwrap();
    // pos = the block payload, 4 bytes (track, timecode, flags) before the frame
    assert_eq!(p[0].pos + 4, t.samples.offset[0]);
    assert_eq!(&f[t.samples.offset[1] as usize..][..20], &[2; 20]);
    assert_eq!(d.cues.len(), 1);
    assert_eq!(demux::mkv::cue_for(&d.cues, 1, 50).map(|c| c.time), Some(0));
}

#[test]
fn mkv_without_cues_is_unsupported_m2() {
    assert_eq!(
        index(&three_frames(false)).err(),
        Some(DemuxError::Unsupported {
            reason: "no cues (M2)".into()
        })
    );
}

fn laced(flags: u8, header: &[u8], frames: &[&[u8]]) -> Vec<u8> {
    let mut payload = header.to_vec();
    for f in frames {
        payload.extend_from_slice(f);
    }
    build::mkv(
        &[build::cluster(
            0,
            &[build::simple_block(0, 0x80 | flags, &payload)],
        )],
        true,
    )
}

#[test]
fn mkv_lacing_modes() {
    let (a, b, c) = (&[1u8; 300][..], &[2u8; 5][..], &[3u8; 7][..]);
    // Xiph: count-1 = 2, sizes 300 = 255+45, 5
    let xiph = laced(0x02, &[2, 255, 45, 5], &[a, b, c]);
    // EBML: first 300 as a 2-byte VINT (0x412C), then delta 5-300 = -295 as a signed
    // 2-byte VINT: raw = -295 + 8191 = 7896 = 0x1ED8 -> 0x5ED8
    let ebml = laced(0x06, &[2, 0x41, 0x2C, 0x5E, 0xD8], &[a, b, c]);
    for f in [xiph, ebml] {
        let d = index(&f).unwrap();
        assert_eq!(d.tracks[0].samples.size, vec![300, 5, 7]);
        let p = d.tracks[0].packets(View::Raw).unwrap();
        assert_eq!(
            p[1].pts, None,
            "a laced frame without DefaultDuration has no known pts"
        );
    }
    let fixed = laced(0x04, &[2], &[&[9; 4], &[8; 4], &[7; 4]]);
    let d = index(&fixed).unwrap();
    assert_eq!(d.tracks[0].samples.size, vec![4, 4, 4]);
    // Lace sizes that overrun the block: the cluster is damaged, so it is skipped by a
    // resync (or the file is refused), never indexed with bogus frames.
    match index(&laced(0x02, &[2, 255, 255, 5], &[b])) {
        Ok(d) => assert!(d.tracks[0].samples.is_empty()),
        Err(e) => assert!(matches!(e, DemuxError::Invalid { .. }), "{e}"),
    }
}

#[test]
fn mkv_lace_edge_cases() {
    // EBML lacing flag with a count byte of 0: one frame, no sizes to read.
    let one = laced(0x06, &[0], &[&[7; 10]]);
    assert_eq!(index(&one).unwrap().tracks[0].samples.size, vec![10]);
    // EBML lace size of all ones (unknown) and zero-size frames: damaged, not indexed.
    for f in [
        laced(0x06, &[2, 0xFF, 0xC1], &[&[1; 9]]),
        laced(0x02, &[2, 0, 0], &[&[1; 9]]),
    ] {
        match index(&f) {
            Ok(d) => assert!(d.tracks[0].samples.is_empty()),
            Err(e) => assert!(matches!(e, DemuxError::Invalid { .. }), "{e}"),
        }
    }
}

#[test]
fn mkv_default_duration_overflow_is_invalid() {
    let header = build::el(0x1A45_DFA3, &build::str_el(0x4282, "webm"));
    let entry = build::el(
        0xAE,
        &[
            build::uint_el(0xD7, 1),
            build::uint_el(0x83, 1),
            build::str_el(0x86, "V_VP9"),
            build::uint_el(0x23_E383, u64::MAX),
        ]
        .concat(),
    );
    let c = build::cluster(0, &[build::simple_block(0, 0x80, &[1; 4])]);
    let pos = build::el(
        0xB7,
        &[build::uint_el(0xF7, 1), build::uint_el(0xF1, 0)].concat(),
    );
    let cues = build::el(
        0x1C53_BB6B,
        &build::el(0xBB, &[build::uint_el(0xB3, 0), pos].concat()),
    );
    let seg = [build::el(0x1654_AE6B, &entry), c, cues].concat();
    let f = [header, build::el(0x1853_8067, &seg)].concat();
    assert!(matches!(index(&f), Err(DemuxError::Invalid { .. })));
}

#[test]
fn mkv_bframe_order_gets_decode_times() {
    // pts in decode order 0, 100, 33, 67 (an I P B B pattern at ~30 fps)
    let c = build::cluster(
        0,
        &[
            build::simple_block(0, 0x80, &[1; 4]),
            build::simple_block(100, 0, &[1; 4]),
            build::simple_block(33, 0, &[1; 4]),
            build::simple_block(67, 0, &[1; 4]),
        ],
    );
    let d = index(&build::mkv(&[c], true)).unwrap();
    let s = &d.tracks[0].samples;
    let pts: Vec<i64> = (0..4).map(|i| s.dts[i] + i64::from(s.cts_off[i])).collect();
    assert_eq!(pts, vec![0, 100, 33, 67]);
    assert!(
        s.dts.windows(2).all(|w| w[0] <= w[1]),
        "dts monotonic: {:?}",
        s.dts
    );
    assert!((0..4).all(|i| s.cts_off[i] >= 0));
    assert_eq!(s.reorder_depth, 1);
}

#[test]
fn mkv_block_group_key_from_reference() {
    let blk = |rel: i16| {
        let mut b = vec![0x81];
        b.extend_from_slice(&rel.to_be_bytes());
        b.push(0);
        b.extend_from_slice(&[0; 8]);
        build::el(0xA1, &b)
    };
    let g1 = build::el(0xA0, &blk(0));
    let g2 = build::el(0xA0, &[blk(33), build::uint_el(0xFB, 33)].concat());
    let d = index(&build::mkv(&[build::cluster(100, &[g1, g2])], true)).unwrap();
    assert_eq!(d.tracks[0].samples.sync, vec![true, false]);
    assert_eq!(d.tracks[0].samples.dts, vec![100, 133]);
}

#[test]
fn mkv_resyncs_past_a_damaged_cluster() {
    let good = build::cluster(0, &[build::simple_block(0, 0x80, &[1; 10])]);
    let mut bad = build::cluster(1000, &[build::simple_block(0, 0x80, &[1; 10])]);
    let n = bad.len();
    bad[n - 14] = 0xFF; // corrupt the block's size inside
    bad[n - 13] = 0xFF;
    let later = build::cluster(2000, &[build::simple_block(0, 0x80, &[5; 10])]);
    let d = index(&build::mkv(&[good, bad, later], true)).unwrap();
    assert!(
        d.tracks[0].samples.dts.contains(&2000),
        "the cluster after the damage is indexed"
    );
}

#[test]
fn mkv_every_truncation_and_corruption_fails_cleanly() {
    let f = three_frames(true);
    for n in 0..f.len() {
        let _ = index(&f[..n]);
    }
    for i in 0..f.len() {
        for v in [0x00, 0x01, 0x40, 0x7F, 0x80, 0xFF] {
            let mut g = f.clone();
            g[i] = v;
            let _ = index(&g);
        }
    }
}

// ---------- B-18 bomb fixtures (SR §3.5.7) ----------

#[test]
fn bomb_huge_largesize() {
    let mut f = build::bx(b"ftyp", b"isom");
    f.extend_from_slice(&1u32.to_be_bytes());
    f.extend_from_slice(b"moov");
    f.extend_from_slice(&(1u64 << 62).to_be_bytes());
    let r = invalid_budget(index(&f));
    assert!(r.contains("past the end"), "{r}");
}

#[test]
fn bomb_2_pow_32_entry_stsz() {
    let f = build::mp4(&[Trak::video(4)]);
    // rewrite the stsz count to 0xFFFFFFFF (per-sample sizes): claims 16 GB of entries
    let at = f.windows(4).position(|w| w == b"stsz").unwrap() + 12; // fourcc, version/flags, sample_size
    let mut g = f.clone();
    g[at..at + 4].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
    let r = invalid_budget(index(&g));
    assert!(r.starts_with("budget"), "{r}");
    // and the constant-size form, where the count costs nothing to claim
    let mut h = f.clone();
    h[at - 4..at].copy_from_slice(&7u32.to_be_bytes());
    h[at..at + 4].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
    let r = invalid_budget(index(&h));
    assert!(r.contains("stts") || r.starts_with("budget"), "{r}");
}

#[test]
fn bomb_64_deep_moov() {
    let mut b = build::bx(b"free", &[]);
    for _ in 0..64 {
        b = build::bx(b"moov", &b);
    }
    let f = [build::bx(b"ftyp", b"isom"), b].concat();
    let r = invalid_budget(index(&f));
    assert!(r.starts_with("budget"), "{r}");
}

#[test]
fn bomb_1000_deep_ebml() {
    let mut v: Vec<u8> = Vec::new();
    for _ in 0..1000 {
        v = build::el(0xAE, &v);
    }
    let header = build::el(0x1A45_DFA3, &build::str_el(0x4282, "webm"));
    let f = [header, build::el(0x1853_8067, &build::el(0x1654_AE6B, &v))].concat();
    let r = invalid_budget(index(&f));
    assert!(r.starts_with("budget"), "{r}");
}

#[test]
fn bomb_adversarial_resync() {
    // Plausible clusters (ID, size, Timestamp) whose next child is garbage: every one
    // forces a resync, and past MAX_RESYNCS the file is refused.
    let mut junk = Vec::new();
    for _ in 0..1000 {
        junk.extend(build::el(
            0x1F43_B675,
            &[build::uint_el(0xE7, 0), vec![0x00, 0x00, 0x00, 0x00]].concat(),
        ));
    }
    let f = build::mkv(&[junk], true);
    let r = invalid_budget(index(&f));
    assert!(r.starts_with("budget") && r.contains("resync"), "{r}");
}

#[test]
fn bomb_cheap_sample_count() {
    // 2^24 samples claimed by a constant stsz, one stts run and one stsc entry in a
    // file of a few hundred bytes: the byte budget refuses it before allocating.
    let mut t = Trak::video(1);
    t.constant = true;
    t.sizes = vec![1; 1];
    t.sync = None;
    let mut f = build::mp4(&[t]);
    let n = 1u32 << 24;
    let stsz = f.windows(4).position(|w| w == b"stsz").unwrap();
    f[stsz + 12..stsz + 16].copy_from_slice(&n.to_be_bytes());
    let stts = f.windows(4).position(|w| w == b"stts").unwrap();
    f[stts + 12..stts + 16].copy_from_slice(&n.to_be_bytes());
    let stsc = f.windows(4).position(|w| w == b"stsc").unwrap();
    f[stsc + 16..stsc + 20].copy_from_slice(&n.to_be_bytes());
    let r = invalid_budget(index(&f));
    assert!(r.starts_with("budget"), "{r}");
}

#[test]
fn bomb_overflowing_edit_list() {
    for edits in [
        vec![(u64::MAX, i64::MAX)],
        vec![(u64::MAX / 2, 0)],
        vec![(1, i64::MAX - 1)],
        vec![(u64::MAX, -1), (u64::MAX, 0)],
    ] {
        let mut t = Trak::video(3);
        t.edits64 = Some(edits.clone());
        let r = invalid_budget(index(&build::mp4(&[t])));
        assert!(r.contains("overflow"), "{edits:?}: {r}");
    }
}
