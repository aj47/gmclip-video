//! R-EXP-5 muxer conformance, CPU (B-05). Synthetic streams: H.264-shaped video with
//! B-frames at reorder depth 2 (DTS rebased below 0 by the reorder delay, as VT emits
//! them) and AAC-shaped audio from an encoder model with priming D ∈ {1024, 2112}, fed
//! from cut sample D (SPEC §6.5 step 3). Asserts, on the file our own parser reads back:
//! first video PTS 0, signed `ctts` (v1) where needed, no `edts` anywhere, `cslg` present,
//! audio sample 0 at t = 0, and cut sample q at decoded position q for every q ≥ D. The
//! whole box tree is compared with a golden dump (MP4Box is not installed; `inspect::dump`
//! stands in for `MP4Box -diso`). The lab half (real encoders, OS decode) is in
//! `mux_priming_lab.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::io::Cursor;

use gmclip_media::audio::priming;
use gmclip_media::mux::{Ctts, Mp4Writer, MuxPlan, MuxReport, Sample, TrackConfig, inspect};

const FRAMES: u32 = 60;
const N: u64 = 96_000; // 60 frames × 1600 samples
const FRAME_TICKS: i64 = 512; // 15360 / 30

/// avcC for High@4.0 (content is never parsed by the writer).
const AVCC: &[u8] = &[
    0x01, 0x64, 0x00, 0x28, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x64, 0x00, 0x28, 0x01, 0x00, 0x02, 0x68,
    0xee,
];
const ASC: &[u8] = &[0x11, 0x90];

/// Decode order I0 P3 B2 b1 P6 B5 b4 … (b-pyramid, reorder depth 2) with DTS rebased
/// by two frames: (dts, pts, sync).
fn video_samples() -> Vec<(i64, i64, bool)> {
    let mut order = vec![0i64];
    let mut p = 0;
    while p + 3 < i64::from(FRAMES) {
        order.extend([p + 3, p + 2, p + 1]);
        p += 3;
    }
    order.extend(p + 1..i64::from(FRAMES));
    order
        .iter()
        .enumerate()
        .map(|(j, &f)| ((j as i64 - 2) * FRAME_TICKS, f * FRAME_TICKS, f % 30 == 0))
        .collect()
}

/// H.264's max_num_reorder_frames for a decode-order PTS sequence.
fn reorder_depth(pts: &[i64]) -> usize {
    (0..pts.len())
        .map(|j| pts[..j].iter().filter(|&&p| p > pts[j]).count())
        .max()
        .unwrap_or(0)
}

/// Marks decoded positions that hold no input sample (warm-up and flush).
const WARMUP: f32 = -9.0;

/// An AAC encoder model with priming `d`: packet p decodes to positions
/// [1024p, 1024p + 1024) of its output, which hold input samples `1024p − d + k`. The
/// payload *is* that decoded PCM (left channel, 1024 f32), so the test can "decode" the
/// muxed file and see which cut sample sits at each position.
fn encode(feed: &[f32], d: u64) -> Vec<Vec<u8>> {
    let frames = feed.len() as u64 / 2;
    let packets = (frames + d).div_ceil(1024) + 1; // encoders flush one extra frame
    (0..packets)
        .map(|p| {
            (0..1024u64)
                .flat_map(|k| {
                    let i = (p * 1024 + k) as i64 - d as i64;
                    let v = if i >= 0 && (i as u64) < frames {
                        feed[2 * i as usize]
                    } else {
                        WARMUP
                    };
                    v.to_le_bytes()
                })
                .collect()
        })
        .collect()
}

/// The cut: distinct values, so a misplaced sample cannot match.
fn cut() -> Vec<f32> {
    (0..N)
        .flat_map(|i| {
            let v = 0.25 + i as f32 * 1e-6;
            [v, -v]
        })
        .collect()
}

fn mux(d: u64) -> (Vec<u8>, MuxReport) {
    let cut = cut();
    let feed = priming::encoder_feed(&cut, 2, d as usize, 48_000);
    let packets = encode(&feed, d);
    let durs = priming::packet_durations(N);
    let video = video_samples();
    let tracks = vec![
        TrackConfig::h264(AVCC.to_vec(), 1080, 1920, 30, 1, None),
        TrackConfig::aac(ASC.to_vec(), 2, 48_000),
    ];
    let plan = MuxPlan {
        sample_counts: vec![FRAMES, durs.len() as u32],
        max_payload_bytes: Some(1 << 20),
        gapless: Some(priming::gapless(N)),
    };
    let mut w = Mp4Writer::create(Cursor::new(Vec::new()), tracks, &plan).unwrap();
    let (mut i, mut j, mut adts) = (0usize, 0usize, 0i64);
    let frame = [0x00, 0x00, 0x00, 0x02, 0x09, 0xf0]; // an AUD NAL, length-prefixed
    while i < video.len() || j < durs.len() {
        let vt = video.get(i).map(|v| v.0 as f64 / 15_360.0);
        let at = (j < durs.len()).then(|| adts as f64 / 48_000.0);
        if matches!((vt, at), (Some(x), Some(y)) if x <= y) || at.is_none() {
            let (dts, pts, sync) = video[i];
            w.write_sample(
                0,
                &Sample {
                    data: &frame,
                    dts,
                    pts,
                    duration: FRAME_TICKS as u32,
                    sync,
                },
            )
            .unwrap();
            i += 1;
        } else {
            w.write_sample(
                1,
                &Sample {
                    data: &packets[j],
                    dts: adts,
                    pts: adts,
                    duration: durs[j],
                    sync: true,
                },
            )
            .unwrap();
            adts += i64::from(durs[j]);
            j += 1;
        }
    }
    let (c, r) = w.finish().unwrap();
    (c.into_inner(), r)
}

fn check(d: u64) {
    let (file, report) = mux(d);
    let tracks = inspect::tracks(&file).unwrap();
    let (v, a) = (&tracks[0], &tracks[1]);

    // No edit lists anywhere in the file.
    let dump = inspect::dump(&file).unwrap();
    assert!(!dump.contains("edts") && !dump.contains("elst"), "{dump}");
    assert!(!v.has_edts && !a.has_edts);

    // Video: first PTS 0, depth-2 reordering, signed ctts (v1), cslg present.
    let pts: Vec<i64> = v.samples.iter().map(|s| s.pts).collect();
    assert_eq!(pts.iter().min(), Some(&0), "first video PTS 0");
    assert_eq!(v.samples[0].pts, 0, "the first decoded frame presents at 0");
    assert_eq!(reorder_depth(&pts), 2);
    assert_eq!(v.timescale, 15_360);
    assert_eq!(v.ctts_version, Some(1), "negative offsets need ctts v1");
    assert_eq!(report.tracks[0].ctts, Ctts::V1);
    assert!(
        v.samples.iter().any(|s| s.pts < s.dts),
        "some sample presents before it decodes"
    );
    let c = v.cslg.expect("cslg present");
    assert_eq!(
        (c.shift, c.least, c.greatest),
        (2 * FRAME_TICKS, -2 * FRAME_TICKS, 2 * FRAME_TICKS)
    );
    assert_eq!((c.start, c.end), (0, i64::from(FRAMES) * FRAME_TICKS));
    let mut sorted = pts.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        (0..i64::from(FRAMES))
            .map(|f| f * FRAME_TICKS)
            .collect::<Vec<_>>(),
        "every frame once"
    );
    assert_eq!(v.media_duration, u64::from(FRAMES) * 512);

    // Audio: sample 0 at t = 0, packets every 1024, exactly N samples (the cut's length).
    assert_eq!(a.timescale, 48_000);
    assert_eq!(a.ctts_version, None);
    assert_eq!(a.samples[0].pts, 0, "audio sample 0 at t = 0");
    assert_eq!(a.media_duration, N);
    assert_eq!(a.samples.len(), N.div_ceil(1024) as usize);
    // "Decode" the muxed audio as a player without priming info does: packet p's PCM at
    // positions [pts, pts + 1024). Cut sample q must sit at position q for q ≥ D (after
    // the 5 ms feed fade, exactly); positions below D are the encoder's warm-up.
    let mut decoded = vec![f32::NAN; a.samples.len() * 1024];
    for (p, s) in a.samples.iter().enumerate() {
        assert_eq!((s.dts, s.pts), (p as i64 * 1024, p as i64 * 1024));
        let at = s.offset as usize;
        for k in 0..1024 {
            let b = &file[at + 4 * k..at + 4 * k + 4];
            decoded[s.pts as usize + k] = f32::from_le_bytes(b.try_into().unwrap());
        }
    }
    let cut = cut();
    let d = d as usize;
    assert!(
        decoded[..d].iter().all(|&v| v == WARMUP),
        "positions below D are warm-up (D = {d})"
    );
    for q in d..d + 240 {
        let want = (f64::from(cut[2 * q]) * ((q - d) as f64 / 240.0)) as f32;
        assert_eq!(decoded[q], want, "fade at {q} (D = {d})");
    }
    for q in d + 240..N as usize {
        assert_eq!(
            decoded[q],
            cut[2 * q],
            "cut sample {q} at decoded position {q} (D = {d})"
        );
    }
    // The gapless tag says: no priming to skip, exactly N samples.
    let smpb = gmclip_media::audio::priming::gapless(N).itunsmpb();
    assert!(
        file.windows(smpb.len()).any(|w| w == smpb.as_bytes()),
        "iTunSMPB present"
    );
    // A/V end together: both tracks last exactly 2 s.
    assert_eq!(v.media_duration * 48_000, a.media_duration * 15_360);
    assert_eq!(dump, golden_tree(), "box tree (D = {d})");
}

#[test]
fn conformance_priming_1024() {
    check(1024);
}

#[test]
fn conformance_priming_2112() {
    check(2112);
}

/// The full box tree of the synthetic file, as `inspect::dump` prints it. Identical for
/// both D (priming only changes which input samples the packets carry).
fn golden_tree() -> String {
    GOLDEN.trim_start().to_owned()
}

const GOLDEN: &str = "
ftyp size=36 major=isom minor=512 compat=isom,iso2,iso4,avc1,mp41,
moov size=2988
  mvhd size=108 v=0 timescale=1000 duration=2000
  trak size=1296
    tkhd size=92 v=0 flags=3 id=1 duration=2000 1080x1920
    mdia size=1196
      mdhd size=32 v=0 timescale=15360 duration=30720
      hdlr size=45 type=vide
      minf size=1111
        vmhd size=20
        dinf size=36
          dref size=28
            url  size=12
        stbl size=1047
          stsd size=163
            avc1 size=147 1080x1920
              avcC size=25 bytes=17
              pasp size=16 1:1
              btrt size=20
          stts size=24 v=0 entries=1
          stss size=24 v=0 entries=2
          ctts size=488 v=1 entries=59
          cslg size=32 v=0 shift=1024 least=-1024 greatest=1024 start=0 end=30720
          stsc size=40 v=0 entries=2
          stsz size=20 v=0 sample_size=6 count=60
          stco size=248 v=0 entries=58
  trak size=1326
    tkhd size=92 v=0 flags=3 id=2 duration=2000 0x0
    mdia size=1226
      mdhd size=32 v=0 timescale=48000 duration=96000
      hdlr size=45 type=soun
      minf size=1141
        smhd size=16
        dinf size=36
          dref size=28
            url  size=12
        stbl size=1081
          stsd size=103
            mp4a size=87 channels=2 bits=16 rate=48000
              esds size=51
          stts size=32 v=0 entries=2
          stsc size=616 v=0 entries=50
          stsz size=20 v=0 sample_size=4096 count=94
          stco size=248 v=0 entries=58
          sgpd size=26
          sbgp size=28
  udta size=250
    meta size=242
      hdlr size=34 type=mdir
      ilst size=196
        ---- size=188
          mean size=28
          name size=20
          data size=132
free size=5356
mdat size=385400 largesize=true
";
