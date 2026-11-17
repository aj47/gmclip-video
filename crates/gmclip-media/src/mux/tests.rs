//! Unit tests per box and layout (B-04). The R-EXP-5 conformance suite is in
//! `tests/mux_conformance.rs` (B-05).
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used)]

use std::io::Cursor;

use super::inspect::{self, BoxNode};
use super::*;

/// A syntactically valid avcC (High@4.0, one tiny SPS/PPS); content is never parsed.
pub(crate) const AVCC: &[u8] = &[
    0x01, 0x64, 0x00, 0x28, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x64, 0x00, 0x28, 0x01, 0x00, 0x02, 0x68,
    0xee,
];
/// AAC-LC, 48 kHz, stereo.
pub(crate) const ASC: &[u8] = &[0x11, 0x90];

fn video() -> TrackConfig {
    TrackConfig::h264(
        AVCC.to_vec(),
        1080,
        1920,
        30,
        1,
        Some(ColorTags::BT709_LIMITED),
    )
}

fn audio() -> TrackConfig {
    TrackConfig::aac(ASC.to_vec(), 2, 48_000)
}

/// Writes `samples` (track, dts, pts, duration, sync, size) with a recognisable payload.
fn write(
    tracks: Vec<TrackConfig>,
    samples: &[(usize, i64, i64, u32, bool, usize)],
    bound: Option<u64>,
) -> (Vec<u8>, MuxReport) {
    let mut counts = vec![0u32; tracks.len()];
    for s in samples {
        counts[s.0] += 1;
    }
    let plan = MuxPlan {
        gapless: None,
        sample_counts: counts,
        max_payload_bytes: bound,
    };
    let mut w = Mp4Writer::create(Cursor::new(Vec::new()), tracks, &plan).unwrap();
    for (i, &(t, dts, pts, duration, sync, size)) in samples.iter().enumerate() {
        let data: Vec<u8> = (0..size).map(|k| (i * 31 + k) as u8).collect();
        w.write_sample(
            t,
            &Sample {
                data: &data,
                dts,
                pts,
                duration,
                sync,
            },
        )
        .unwrap();
    }
    let (c, r) = w.finish().unwrap();
    (c.into_inner(), r)
}

fn top(file: &[u8]) -> Vec<BoxNode> {
    inspect::parse(file).unwrap()
}

fn payload<'a>(file: &'a [u8], node: &BoxNode, path: &[&[u8; 4]]) -> &'a [u8] {
    let mut n = node;
    for p in path {
        n = n
            .find(p)
            .unwrap_or_else(|| panic!("no {}", String::from_utf8_lossy(*p)));
    }
    n.payload(file)
}

/// I0 P3 B2 b1 P6 B5 b4 … : reorder depth 2, DTS rebased by 2 frames so PTS starts at 0.
pub(crate) fn pyramid(frames: u32, d: u32) -> Vec<(i64, i64, bool)> {
    let mut order = vec![0u32];
    let mut p = 0;
    while p + 3 < frames {
        order.extend([p + 3, p + 2, p + 1]);
        p += 3;
    }
    for f in p + 1..frames {
        order.push(f);
    }
    order
        .iter()
        .enumerate()
        .map(|(j, &f)| {
            (
                (j as i64 - 2) * i64::from(d),
                i64::from(f) * i64::from(d),
                f % 30 == 0,
            )
        })
        .collect()
}

#[test]
fn video_timescale_rule() {
    assert_eq!(video_timescale(30, 1), 15_360);
    assert_eq!(video_timescale(60, 1), 30_720);
    assert_eq!(video_timescale(24, 1), 12_288);
    // NTSC: the numerator, 1001 ticks a frame (decision 0020; 90 kHz cannot hold
    // 23.976 or 59.94 exactly)
    assert_eq!(video_timescale(30_000, 1001), 30_000);
    assert_eq!(video_timescale(24_000, 1001), 24_000);
    assert_eq!(video_timescale(60_000, 1001), 60_000);
    assert_eq!(video_timescale(25, 3), 90_000);
    assert_eq!(video_timescale(60, 2), 15_360, "reduced first");
    assert_eq!(video_timescale(0, 1), 90_000);
}

#[test]
fn ftyp_bytes() {
    let f = moov::ftyp().unwrap();
    assert_eq!(&f[..8], &[0, 0, 0, 36, b'f', b't', b'y', b'p']);
    assert_eq!(&f[8..16], b"isom\0\0\x02\0");
    assert_eq!(&f[16..], b"isomiso2iso4avc1mp41");
}

#[test]
fn layout_is_ftyp_moov_free_mdat_largesize() {
    let s: Vec<_> = (0..10)
        .map(|i| (0, i * 512, i * 512, 512, i == 0, 100))
        .collect();
    let (file, r) = write(vec![video()], &s, Some(10_000));
    let t = top(&file);
    let names: Vec<String> = t.iter().map(BoxNode::name).collect();
    assert_eq!(names, ["ftyp", "moov", "free", "mdat"]);
    assert_eq!(t[3].header, 16, "mdat uses a largesize header");
    assert_eq!(t[3].size, 16 + 1000);
    assert_eq!(
        t[1].size + t[2].size,
        r.reserved,
        "moov + free fill the reservation"
    );
    assert_eq!(r.moov_size, t[1].size);
    assert_eq!(r.mdat_offset, t[3].offset);
    assert!(
        t[1].find(b"edts").is_none() && t[1].find(b"elst").is_none(),
        "never an edit list"
    );
    assert_eq!(file.len() as u64, t[3].offset + t[3].size);
}

#[test]
fn mvhd_tkhd_mdhd_durations() {
    // 10 frames at 15360 → 5120 ticks = 333.33 ms → 334 in movie ticks (rounded up).
    let s: Vec<_> = (0..10)
        .map(|i| (0, i * 512, i * 512, 512, true, 10))
        .collect();
    let (file, _) = write(vec![video()], &s, Some(1_000));
    let moov = top(&file)
        .into_iter()
        .find(|b| &b.fourcc == b"moov")
        .unwrap();
    let mvhd = payload(&file, &moov, &[b"mvhd"]);
    assert_eq!(mvhd[0], 0, "version 0");
    assert_eq!(u32::from_be_bytes(mvhd[12..16].try_into().unwrap()), 1000);
    assert_eq!(u32::from_be_bytes(mvhd[16..20].try_into().unwrap()), 334);
    assert_eq!(
        u32::from_be_bytes(mvhd[96..100].try_into().unwrap()),
        2,
        "next_track_ID"
    );
    let tr = &inspect::tracks(&file).unwrap()[0];
    assert_eq!(
        (tr.timescale, tr.media_duration, tr.tkhd_duration),
        (15_360, 5_120, 334)
    );
    let tkhd = payload(&file, &moov, &[b"trak", b"tkhd"]);
    assert_eq!(
        u32::from_be_bytes([0, tkhd[1], tkhd[2], tkhd[3]]),
        3,
        "enabled | in_movie"
    );
    assert_eq!(
        u32::from_be_bytes(tkhd[76..80].try_into().unwrap()) >> 16,
        1080
    );
    assert_eq!(
        u32::from_be_bytes(tkhd[80..84].try_into().unwrap()) >> 16,
        1920
    );
    let mdhd = payload(&file, &moov, &[b"trak", b"mdia", b"mdhd"]);
    assert_eq!(
        u16::from_be_bytes(mdhd[20..22].try_into().unwrap()),
        0x55c4,
        "und"
    );
}

#[test]
fn hdlr_vmhd_smhd_dref() {
    let s = [(0, 0, 0, 512, true, 5), (1, 0, 0, 1024, true, 5)];
    let (file, _) = write(vec![video(), audio()], &s, Some(100));
    let moov = top(&file)
        .into_iter()
        .find(|b| &b.fourcc == b"moov")
        .unwrap();
    let traks: Vec<&BoxNode> = moov
        .children
        .iter()
        .filter(|b| &b.fourcc == b"trak")
        .collect();
    assert_eq!(&payload(&file, traks[0], &[b"hdlr"])[8..12], b"vide");
    assert_eq!(&payload(&file, traks[1], &[b"hdlr"])[8..12], b"soun");
    assert!(traks[0].find(b"vmhd").is_some() && traks[0].find(b"smhd").is_none());
    assert!(traks[1].find(b"smhd").is_some() && traks[1].find(b"vmhd").is_none());
    let vmhd = payload(&file, traks[0], &[b"vmhd"]);
    assert_eq!(vmhd[3], 1, "vmhd flags = 1");
    let dref = traks[0].find(b"dref").unwrap();
    assert_eq!(dref.children.len(), 1);
    assert_eq!(&dref.children[0].fourcc, b"url ");
    assert_eq!(
        dref.children[0].payload(&file),
        &[0, 0, 0, 1],
        "self-contained"
    );
}

#[test]
fn avc1_entry_avcc_colr_pasp_btrt() {
    let s: Vec<_> = (0..60)
        .map(|i| (0, i * 512, i * 512, 512, i % 30 == 0, 1000 + i as usize))
        .collect();
    let (file, _) = write(vec![video()], &s, Some(1 << 20));
    let moov = top(&file)
        .into_iter()
        .find(|b| &b.fourcc == b"moov")
        .unwrap();
    let avc1 = moov.find(b"avc1").unwrap();
    let p = avc1.payload(&file);
    assert_eq!(u16::from_be_bytes([p[6], p[7]]), 1, "data_reference_index");
    assert_eq!(u16::from_be_bytes([p[24], p[25]]), 1080);
    assert_eq!(u16::from_be_bytes([p[26], p[27]]), 1920);
    assert_eq!(u16::from_be_bytes([p[74], p[75]]), 0x18, "depth");
    let kids: Vec<String> = avc1.children.iter().map(BoxNode::name).collect();
    assert_eq!(kids, ["avcC", "colr", "pasp", "btrt"]);
    assert_eq!(
        payload(&file, avc1, &[b"avcC"]),
        AVCC,
        "avcC copied verbatim"
    );
    assert_eq!(
        payload(&file, avc1, &[b"colr"]),
        b"nclx\0\x01\0\x01\0\x01\0"
    );
    assert_eq!(payload(&file, avc1, &[b"pasp"]), &[0, 0, 0, 1, 0, 0, 0, 1]);
    let btrt = payload(&file, avc1, &[b"btrt"]);
    let total: u64 = (0..60).map(|i| 1000 + i).sum();
    assert_eq!(
        u32::from_be_bytes(btrt[0..4].try_into().unwrap()),
        1059,
        "buffer = largest sample"
    );
    assert_eq!(
        u64::from(u32::from_be_bytes(btrt[8..12].try_into().unwrap())),
        total * 8 / 2,
        "avg over 2 s"
    );
    assert!(
        u32::from_be_bytes(btrt[4..8].try_into().unwrap())
            >= u32::from_be_bytes(btrt[8..12].try_into().unwrap())
    );
}

#[test]
fn mp4a_entry_and_esds() {
    let s: Vec<_> = (0..47)
        .map(|i| (0, i * 1024, i * 1024, 1024, true, 371))
        .collect();
    let (file, _) = write(vec![audio()], &s, Some(1 << 20));
    let moov = top(&file)
        .into_iter()
        .find(|b| &b.fourcc == b"moov")
        .unwrap();
    let mp4a = moov.find(b"mp4a").unwrap();
    let p = mp4a.payload(&file);
    assert_eq!(u16::from_be_bytes([p[16], p[17]]), 2, "channels");
    assert_eq!(u16::from_be_bytes([p[18], p[19]]), 16, "samplesize");
    assert_eq!(
        u32::from_be_bytes(p[24..28].try_into().unwrap()),
        48_000 << 16
    );
    let esds = payload(&file, mp4a, &[b"esds"]);
    // version/flags, then ES_Descriptor (tag 3) with 4-byte lengths.
    assert_eq!(&esds[..5], &[0, 0, 0, 0, 0x03]);
    let es_len = usize::from(esds[8]);
    assert_eq!(
        es_len,
        esds.len() - 9,
        "ES_Descriptor length covers the rest"
    );
    assert_eq!(&esds[9..12], &[0, 1, 0], "ES_ID = track id, flags 0");
    assert_eq!(esds[12], 0x04, "DecoderConfigDescriptor");
    assert_eq!(&esds[17..19], &[0x40, 0x15], "MPEG-4 audio stream");
    assert_eq!(
        u32::from_be_bytes([0, esds[19], esds[20], esds[21]]),
        371,
        "bufferSizeDB"
    );
    assert_eq!(esds[30], 0x05, "DecoderSpecificInfo");
    assert_eq!(usize::from(esds[34]), ASC.len());
    assert_eq!(&esds[35..37], ASC, "AudioSpecificConfig copied verbatim");
    assert_eq!(
        &esds[37..],
        &[0x06, 0x80, 0x80, 0x80, 0x01, 0x02],
        "SLConfig predefined 2"
    );
    // Constant sample size collapses into stsz.sample_size.
    let stsz = payload(&file, &moov, &[b"trak", b"stsz"]);
    assert_eq!(u32::from_be_bytes(stsz[4..8].try_into().unwrap()), 371);
    assert_eq!(stsz.len(), 12, "no per-sample table");
}

#[test]
fn stts_runs_and_last_sample_duration() {
    // 3 packets of 1024, a short last one of 600 (the cut's tail).
    let s = [
        (0, 0, 0, 1024, true, 9),
        (0, 1024, 1024, 1024, true, 9),
        (0, 2048, 2048, 1024, true, 9),
        (0, 3072, 3072, 600, true, 9),
    ];
    let (file, r) = write(vec![audio()], &s, Some(100));
    let moov = top(&file)
        .into_iter()
        .find(|b| &b.fourcc == b"moov")
        .unwrap();
    let stts = payload(&file, &moov, &[b"trak", b"stts"]);
    assert_eq!(
        stts,
        &[
            0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 4, 0, 0, 0, 0, 1, 0, 0, 2, 0x58
        ]
    );
    assert_eq!(r.tracks[0].duration, 3672);
}

#[test]
fn no_ctts_without_reordering() {
    let s: Vec<_> = (0..30)
        .map(|i| (0, i * 512, i * 512, 512, i == 0, 50))
        .collect();
    let (file, r) = write(vec![video()], &s, Some(10_000));
    let moov = top(&file)
        .into_iter()
        .find(|b| &b.fourcc == b"moov")
        .unwrap();
    assert!(moov.find(b"ctts").is_none() && moov.find(b"cslg").is_none());
    assert_eq!(r.tracks[0].ctts, Ctts::None);
    assert!(!r.tracks[0].cslg);
}

#[test]
fn ctts_v1_and_cslg_for_bframes() {
    let d = 512u32;
    let pat = pyramid(10, d);
    let s: Vec<_> = pat
        .iter()
        .map(|&(dts, pts, sync)| (0, dts, pts, d, sync, 20))
        .collect();
    let (file, r) = write(vec![video()], &s, Some(10_000));
    let tr = &inspect::tracks(&file).unwrap()[0];
    assert_eq!(tr.ctts_version, Some(1));
    assert_eq!(r.tracks[0].ctts, Ctts::V1);
    let pts: Vec<i64> = tr.samples.iter().map(|x| x.pts).collect();
    assert_eq!(pts.iter().min(), Some(&0), "first presentation time 0");
    let mut sorted = pts.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, (0..10).map(|f| f * 512).collect::<Vec<_>>());
    assert_eq!(
        tr.samples.iter().map(|x| x.dts).collect::<Vec<_>>(),
        (0..10).map(|j| j * 512).collect::<Vec<_>>()
    );
    let c = tr.cslg.unwrap();
    // offsets: I0 0, P3 +2, B2 0, b1 −2 (frames).
    assert_eq!(
        (c.version, c.shift, c.least, c.greatest, c.start, c.end),
        (0, 1024, -1024, 1024, 0, 5120)
    );
    // ctts entries run-length encoded.
    let moov = top(&file)
        .into_iter()
        .find(|b| &b.fourcc == b"moov")
        .unwrap();
    let ctts = payload(&file, &moov, &[b"trak", b"ctts"]);
    assert_eq!(ctts[0], 1);
    let n = u32::from_be_bytes(ctts[4..8].try_into().unwrap());
    assert_eq!(ctts.len(), 8 + 8 * n as usize);
}

#[test]
fn ctts_v0_when_offsets_are_non_negative() {
    let s = [
        (0, 0, 0, 512, true, 5),
        (0, 512, 1024, 512, false, 5),
        (0, 1024, 1536, 512, false, 5),
    ];
    let (file, r) = write(vec![video()], &s, Some(100));
    let tr = &inspect::tracks(&file).unwrap()[0];
    assert_eq!(tr.ctts_version, Some(0));
    assert_eq!(r.tracks[0].ctts, Ctts::V0);
    assert_eq!(
        tr.cslg.map(|c| (c.shift, c.least, c.greatest)),
        Some((0, 0, 512))
    );
}

#[test]
fn stss_only_when_some_sample_is_not_sync() {
    let s: Vec<_> = (0..65)
        .map(|i| (0, i * 512, i * 512, 512, i % 30 == 0, 5))
        .collect();
    let (file, r) = write(vec![video()], &s, Some(1000));
    let moov = top(&file)
        .into_iter()
        .find(|b| &b.fourcc == b"moov")
        .unwrap();
    let stss = payload(&file, &moov, &[b"trak", b"stss"]);
    assert_eq!(
        stss,
        &[0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 1, 0, 0, 0, 31, 0, 0, 0, 61]
    );
    assert!(r.tracks[0].stss);
    let s: Vec<_> = (0..5)
        .map(|i| (0, i * 1024, i * 1024, 1024, true, 5))
        .collect();
    let (file, r) = write(vec![audio()], &s, Some(1000));
    assert!(
        top(&file)
            .into_iter()
            .find(|b| &b.fourcc == b"moov")
            .unwrap()
            .find(b"stss")
            .is_none()
    );
    assert!(!r.tracks[0].stss);
}

#[test]
fn chunks_follow_write_order_and_stsc_compresses() {
    // V V A A A V V A A A V V … : video chunks of 2, audio chunks of 3.
    let mut s = Vec::new();
    for k in 0..4i64 {
        for j in 0..2 {
            let f = k * 2 + j;
            s.push((0, f * 512, f * 512, 512, f == 0, 10 + f as usize));
        }
        for j in 0..3 {
            let p = k * 3 + j;
            s.push((1, p * 1024, p * 1024, 1024, true, 7));
        }
    }
    let (file, r) = write(vec![video(), audio()], &s, Some(1000));
    assert_eq!((r.tracks[0].chunks, r.tracks[1].chunks), (4, 4));
    let moov = top(&file)
        .into_iter()
        .find(|b| &b.fourcc == b"moov")
        .unwrap();
    let traks: Vec<&BoxNode> = moov
        .children
        .iter()
        .filter(|b| &b.fourcc == b"trak")
        .collect();
    assert_eq!(
        payload(&file, traks[0], &[b"stsc"]),
        &[0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1]
    );
    assert_eq!(
        payload(&file, traks[1], &[b"stsc"]),
        &[0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 3, 0, 0, 0, 1]
    );
    // Every sample's bytes are where the tables say.
    let tracks = inspect::tracks(&file).unwrap();
    let mut seen = 0;
    for (i, &(t, ..)) in s.iter().enumerate() {
        let info = tracks[t].samples[s[..i].iter().filter(|x| x.0 == t).count()];
        let at = info.offset as usize;
        assert_eq!(file[at], (i * 31) as u8, "sample {i} data");
        seen += 1;
    }
    assert_eq!(seen, s.len());
}

#[test]
fn stco_when_the_bound_fits_co64_otherwise() {
    let s: Vec<_> = (0..3)
        .map(|i| (0, i * 512, i * 512, 512, true, 5))
        .collect();
    let (file, r) = write(vec![video()], &s, Some(1000));
    assert_eq!(
        (r.planned_offsets, r.offsets),
        (OffsetBoxes::Stco, OffsetBoxes::Stco)
    );
    assert!(!inspect::tracks(&file).unwrap()[0].co64);
    let (file, r) = write(vec![video()], &s, None);
    assert_eq!(
        (r.planned_offsets, r.offsets),
        (OffsetBoxes::Co64, OffsetBoxes::Co64)
    );
    let tr = &inspect::tracks(&file).unwrap()[0];
    assert!(tr.co64);
    assert_eq!(file[tr.samples[1].offset as usize], 31);
}

#[test]
fn plan_switches_to_co64_past_4_gib() {
    // 1 GiB video samples interleaved with audio: chunks start past 2^32.
    let t = [video(), audio()];
    let mut samples = Vec::new();
    for i in 0..5 {
        samples.push(PlannedSample {
            track: 0,
            size: 1 << 30,
            dts: i * 512,
            pts: i * 512,
            duration: 512,
            sync: true,
        });
        samples.push(PlannedSample {
            track: 1,
            size: 10,
            dts: i * 1024,
            pts: i * 1024,
            duration: 1024,
            sync: true,
        });
    }
    let p = plan(&t, &samples, None).unwrap();
    assert_eq!(p.report.offsets, OffsetBoxes::Co64);
    assert!(p.sample_offsets[8] > u64::from(u32::MAX));
    assert_eq!(p.file_size, p.sample_offsets[9] + 10);
    // A bound that claimed it would fit, then the payload outgrew it: upgraded, still fits.
    let p = plan(&t, &samples, Some(1000)).unwrap();
    assert_eq!(
        (p.report.planned_offsets, p.report.offsets),
        (OffsetBoxes::Stco, OffsetBoxes::Co64)
    );
    // The planned moov parses and lists the real offsets.
    let mut file = p.ftyp.clone();
    file.extend_from_slice(&p.moov);
    let moov = inspect::parse(&file).unwrap().pop().unwrap();
    let co64 = moov.find(b"co64").unwrap().payload(&file);
    assert_eq!(
        u64::from_be_bytes(co64[24..32].try_into().unwrap()),
        p.sample_offsets[4]
    );
    // One track only: a single chunk starting below 2^32 keeps stco even past 4 GiB of data.
    let one: Vec<PlannedSample> = samples.iter().filter(|s| s.track == 0).copied().collect();
    let p = plan(&t[..1], &one, Some(1000)).unwrap();
    assert_eq!(
        (p.report.offsets, p.report.tracks[0].chunks),
        (OffsetBoxes::Stco, 1)
    );
}

#[test]
fn refuses_first_pts_not_zero() {
    // ctts-v0 style (DTS 0, PTS starting at 2 frames) needs an edit list: refused.
    let plan = MuxPlan {
        gapless: None,
        sample_counts: vec![2],
        max_payload_bytes: Some(10),
    };
    let mut w = Mp4Writer::create(Cursor::new(Vec::new()), vec![video()], &plan).unwrap();
    w.write_sample(
        0,
        &Sample {
            data: &[1],
            dts: 0,
            pts: 1024,
            duration: 512,
            sync: true,
        },
    )
    .unwrap();
    w.write_sample(
        0,
        &Sample {
            data: &[1],
            dts: 512,
            pts: 1536,
            duration: 512,
            sync: false,
        },
    )
    .unwrap();
    assert!(matches!(
        w.finish(),
        Err(MuxError::FirstPtsNotZero {
            track: 0,
            first_pts: 1024
        })
    ));
}

#[test]
fn refuses_bad_samples_and_keeps_going() {
    let plan = MuxPlan {
        gapless: None,
        sample_counts: vec![2, 1],
        max_payload_bytes: Some(10),
    };
    let mut w = Mp4Writer::create(Cursor::new(Vec::new()), vec![video(), audio()], &plan).unwrap();
    let ok = |dts| Sample {
        data: &[1],
        dts,
        pts: dts,
        duration: 512,
        sync: true,
    };
    assert!(matches!(
        w.write_sample(2, &ok(0)),
        Err(MuxError::InvalidTrack { .. })
    ));
    assert!(matches!(
        w.write_sample(
            0,
            &Sample {
                duration: 0,
                ..ok(0)
            }
        ),
        Err(MuxError::InvalidSample { .. })
    ));
    w.write_sample(0, &ok(0)).unwrap();
    assert!(
        matches!(
            w.write_sample(0, &ok(600)),
            Err(MuxError::InvalidSample { .. })
        ),
        "dts gap"
    );
    w.write_sample(0, &ok(512)).unwrap();
    assert!(matches!(
        w.write_sample(0, &ok(1024)),
        Err(MuxError::OverPlan {
            track: 0,
            planned: 2
        })
    ));
    // The audio track never got a sample.
    assert!(matches!(w.finish(), Err(MuxError::EmptyTrack { track: 1 })));
}

#[test]
fn rejects_bad_track_configs() {
    let plan = MuxPlan {
        gapless: None,
        sample_counts: vec![1],
        max_payload_bytes: None,
    };
    let bad = TrackConfig::h264(vec![0, 1, 2, 3, 4, 5, 6], 2, 2, 30, 1, None);
    assert!(Mp4Writer::create(Cursor::new(Vec::new()), vec![bad], &plan).is_err());
    let bad = TrackConfig {
        timescale: 44_100,
        ..audio()
    };
    assert!(Mp4Writer::create(Cursor::new(Vec::new()), vec![bad], &plan).is_err());
    let plan2 = MuxPlan {
        gapless: None,
        sample_counts: vec![1, 1],
        max_payload_bytes: None,
    };
    assert!(Mp4Writer::create(Cursor::new(Vec::new()), vec![audio()], &plan2).is_err());
}

/// xorshift64*: deterministic tables for the round-trip property.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Random tables (interleave, sizes, durations, signed offsets, sync) round-trip through
/// the writer and our reader exactly, and the worst-case reservation always holds the
/// moov — including when every table is at its worst.
#[test]
fn random_tables_round_trip_and_fit_the_reservation() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for case in 0..300 {
        let ntracks = 1 + rng.below(2) as usize;
        let tracks: Vec<TrackConfig> = (0..ntracks)
            .map(|t| if t == 0 { video() } else { audio() })
            .collect();
        let n = 1 + rng.below(300) as usize;
        let mut next_dts = vec![0i64; ntracks];
        let mut per_track: Vec<Vec<(i64, u32, bool, usize)>> = vec![Vec::new(); ntracks];
        let mut order = Vec::new();
        let zero_sizes = case % 7 == 0; // whole tracks of empty samples (stsz table kept)
        for _ in 0..n {
            let t = rng.below(ntracks as u64) as usize;
            let d = 1 + rng.below(if case % 2 == 0 { 3 } else { 2000 }) as u32;
            // Video starts on a sync sample (refused otherwise).
            let sync = per_track[t].is_empty() || rng.below(3) != 0;
            let size = if zero_sizes {
                0
            } else {
                rng.below(64) as usize
            };
            per_track[t].push((next_dts[t], d, sync, size));
            next_dts[t] += i64::from(d);
            order.push(t);
        }
        if per_track.iter().any(Vec::is_empty) {
            continue;
        }
        let mut pts_by_track: Vec<Vec<i64>> = per_track
            .iter()
            .map(|v| {
                v.iter()
                    .map(|&(dts, ..)| dts + rng.below(5000) as i64 - 2500)
                    .collect()
            })
            .collect();
        for p in &mut pts_by_track {
            let m = p.iter().copied().min().unwrap_or(0);
            p.iter_mut().for_each(|x| *x -= m);
        }
        let dts_shift: Vec<i64> = (0..ntracks)
            .map(|_| rng.below(10_000) as i64 - 5000)
            .collect();
        let mut samples = Vec::new();
        let mut idx = vec![0usize; ntracks];
        let mut input_index: Vec<Vec<usize>> = vec![Vec::new(); ntracks];
        for &t in &order {
            let i = idx[t];
            idx[t] += 1;
            let (dts, d, sync, size) = per_track[t][i];
            input_index[t].push(samples.len());
            samples.push((t, dts + dts_shift[t], pts_by_track[t][i], d, sync, size));
        }
        let (file, r) = write(
            tracks,
            &samples,
            if case % 3 == 0 { None } else { Some(1 << 20) },
        );
        let t = top(&file);
        assert_eq!(t[1].size + t[2].size, r.reserved, "case {case}");
        let got = inspect::tracks(&file).unwrap();
        for (k, tr) in got.iter().enumerate() {
            assert!(!tr.has_edts);
            let want: Vec<(i64, i64, u32, bool, u32)> = per_track[k]
                .iter()
                .zip(&pts_by_track[k])
                .map(|(&(dts, d, sync, size), &pts)| (dts, pts, d, sync, size as u32))
                .collect();
            let have: Vec<(i64, i64, u32, bool, u32)> = tr
                .samples
                .iter()
                .map(|s| (s.dts, s.pts, s.duration, s.sync, s.size))
                .collect();
            assert_eq!(have, want, "case {case} track {k}");
            // Payload bytes: every sample's first byte is where the tables say.
            for (s, &i) in tr.samples.iter().zip(&input_index[k]) {
                if s.size > 0 {
                    assert_eq!(
                        file[s.offset as usize],
                        (i * 31) as u8,
                        "case {case} track {k} sample {i}"
                    );
                }
            }
        }
    }
}

/// Payload bytes beyond an empty table of each sample table box, from the file.
fn table_bytes(trak: &BoxNode) -> u64 {
    let size = |f: &[u8; 4], empty: u64| trak.find(f).map_or(0, |b| b.size - empty);
    size(b"stts", 16)
        + size(b"stsc", 16)
        + size(b"stsz", 20)
        + size(b"stco", 16)
        + size(b"co64", 16)
        + size(b"ctts", 0)
        + size(b"cslg", 0)
        + size(b"stss", 0)
}

/// The reservation is exactly the forced-v1 empty moov + worst-case tables + a free
/// header: what the real moov leaves over must equal the slack of each table against its
/// worst case plus the 64-bit header savings. A box the reservation forgot to count
/// shows up as a mismatch here (not only as a failure once it overflows).
#[test]
fn reservation_accounts_for_every_box() {
    let mut rng = Rng(7);
    for case in 0..40 {
        let m = 1 + rng.below(400) as i64;
        let mut s = Vec::new();
        let mut dts = [0i64; 2];
        for i in 0..m * 2 {
            let t = (i % 2) as usize;
            let d = 1 + rng.below(5000) as u32;
            let pts = if i < 2 {
                0
            } else {
                dts[t] + rng.below(3000) as i64
            };
            s.push((
                t,
                dts[t],
                pts,
                d,
                i < 2 || rng.below(2) == 0,
                1 + rng.below(9) as usize,
            ));
            dts[t] += i64::from(d);
        }
        let (file, r) = write(
            vec![video(), audio()],
            &s,
            if case % 2 == 0 { None } else { Some(1 << 20) },
        );
        let moov = top(&file)
            .into_iter()
            .find(|b| &b.fourcc == b"moov")
            .unwrap();
        let mut slack = 12; // mvhd v0 vs v1
        for trak in moov.children.iter().filter(|b| &b.fourcc == b"trak") {
            let n = inspect::tracks(&file).unwrap()
                [usize::from(trak.find(b"hdlr").unwrap().payload(&file)[8] == b's')]
            .samples
            .len() as u64;
            slack += 24; // tkhd + mdhd v0 vs v1
            // (The worst case assumes co64; a written stco counts its 4-byte entries.)
            slack += moov::worst_table_bytes(n).unwrap() - table_bytes(trak);
        }
        assert_eq!(r.reserved - r.moov_size - 8, slack, "case {case}");
    }
}

#[test]
fn zero_size_samples_keep_a_stsz_table() {
    let s = [(0, 0, 0, 1024, true, 0), (0, 1024, 1024, 1024, true, 0)];
    let (file, _) = write(vec![audio()], &s, Some(10));
    let tr = &inspect::tracks(&file).unwrap()[0];
    assert_eq!(
        tr.samples.iter().map(|x| x.size).collect::<Vec<_>>(),
        [0, 0]
    );
    let p = plan(
        &[audio()],
        &[PlannedSample {
            track: 0,
            size: 0,
            dts: 0,
            pts: 0,
            duration: 1024,
            sync: true,
        }],
        None,
    )
    .unwrap();
    let mut f = p.ftyp.clone();
    f.extend_from_slice(&p.moov);
    let moov = inspect::parse(&f).unwrap().pop().unwrap();
    assert_eq!(
        moov.find(b"stsz").unwrap().size,
        24,
        "sample_size 0 + one entry"
    );
}

#[test]
fn video_must_start_on_a_sync_sample() {
    let plan = MuxPlan {
        gapless: None,
        sample_counts: vec![2],
        max_payload_bytes: Some(10),
    };
    let mut w = Mp4Writer::create(Cursor::new(Vec::new()), vec![video()], &plan).unwrap();
    let s = Sample {
        data: &[1],
        dts: 0,
        pts: 0,
        duration: 512,
        sync: false,
    };
    assert!(matches!(
        w.write_sample(0, &s),
        Err(MuxError::InvalidSample { sample: 0, .. })
    ));
    w.write_sample(0, &Sample { sync: true, ..s }).unwrap();
    w.write_sample(
        0,
        &Sample {
            dts: 512,
            pts: 512,
            ..s
        },
    )
    .unwrap();
}

#[test]
fn max_bitrate_scales_samples_longer_than_a_second() {
    // Five 1000-byte samples of 2 s each: 4000 bit/s, not 8000.
    let s: Vec<_> = (0..5)
        .map(|i| (0, i * 96_000, i * 96_000, 96_000, true, 1000))
        .collect();
    let (file, _) = write(vec![audio()], &s, Some(1 << 20));
    let moov = top(&file)
        .into_iter()
        .find(|b| &b.fourcc == b"moov")
        .unwrap();
    let esds = payload(&file, &moov, &[b"esds"]);
    assert_eq!(
        u32::from_be_bytes(esds[22..26].try_into().unwrap()),
        4000,
        "max"
    );
    assert_eq!(
        u32::from_be_bytes(esds[26..30].try_into().unwrap()),
        4000,
        "avg"
    );
}

#[test]
fn gapless_tag_is_itunsmpb_in_udta() {
    let g = Gapless {
        priming: 0,
        padding: 256,
        length: 96_000,
    };
    let plan = MuxPlan {
        sample_counts: vec![1],
        max_payload_bytes: Some(10),
        gapless: Some(g),
    };
    let mut w = Mp4Writer::create(Cursor::new(Vec::new()), vec![audio()], &plan).unwrap();
    w.write_sample(
        0,
        &Sample {
            data: &[1],
            dts: 0,
            pts: 0,
            duration: 1024,
            sync: true,
        },
    )
    .unwrap();
    let (c, r) = w.finish().unwrap();
    let file = c.into_inner();
    let t = top(&file);
    assert_eq!(
        t[1].size + t[2].size,
        r.reserved,
        "the tag is inside the reservation"
    );
    let moov = &t[1];
    let udta = moov.children.last().unwrap();
    assert_eq!(&udta.fourcc, b"udta", "after the traks");
    assert_eq!(&payload(&file, udta, &[b"meta", b"hdlr"])[8..12], b"mdir");
    assert_eq!(
        payload(&file, udta, &[b"mean"]),
        b"\0\0\0\0com.apple.iTunes"
    );
    assert_eq!(payload(&file, udta, &[b"name"]), b"\0\0\0\0iTunSMPB");
    let data = payload(&file, udta, &[b"data"]);
    assert_eq!(&data[..8], &[0, 0, 0, 1, 0, 0, 0, 0], "UTF-8, no locale");
    assert_eq!(&data[8..], g.itunsmpb().as_bytes());
    assert_eq!(udta.size, 250, "same size as AudioToolbox's own tag");
}

#[test]
fn an_explicit_edit_round_trips() {
    let mut a = audio();
    a.edit = Some(TrackEdit {
        media_time: 1024,
        presented: 4096,
        segment: None,
    });
    let s: Vec<_> = (0..6)
        .map(|i| (0, i * 1024, i * 1024, 1024, true, 3))
        .collect();
    let (file, r) = write(vec![a], &s, Some(1000));
    let t = top(&file);
    assert_eq!(
        t[1].size + t[2].size,
        r.reserved,
        "the edit is inside the reservation"
    );
    let tr = &inspect::tracks(&file).unwrap()[0];
    assert!(tr.has_edts);
    assert_eq!(
        tr.edits,
        vec![inspect::Edit {
            segment_duration: 85,
            media_time: 1024,
            rate: 0x0001_0000
        }]
    );
    assert_eq!(
        tr.tkhd_duration, 86,
        "tkhd says the presented length (4096/48000 s, rounded up)"
    );
}
