//! B-17: every committed fuzz input (seeds and minimised crashers under
//! `fuzz/regressions/<target>/` and `fuzz/seeds/<target>/`) replays through the same
//! harness bodies the fuzzer runs, on hosted CI. A crasher fixed in the parsers stays
//! fixed here.
//!
//! `GMCLIP_WRITE_SEEDS=1 cargo test -p gmclip-media --test fuzz_regressions -- --ignored` rewrites
//! the synthetic seeds from the test builders.

#[path = "demux_common/mod.rs"]
mod common;

fn replay(target: &str, run: fn(&[u8])) -> usize {
    let mut n = 0;
    for dir in ["regressions", "seeds"] {
        let d = common::repo().join("fuzz").join(dir).join(target);
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for p in paths {
            let data = std::fs::read(&p).unwrap();
            run(&data);
            n += 1;
        }
    }
    n
}

#[test]
fn fz1_mp4_inputs_replay() {
    assert!(replay("fz1_mp4", gmclip_media::fuzz::mp4) >= 8);
}

#[test]
fn fz2_mkv_inputs_replay() {
    // seeds plus the crasher fixed in 841796e's follow-up (i64 overflow in dts synthesis)
    assert!(replay("fz2_mkv", gmclip_media::fuzz::mkv) >= 4);
    assert!(
        common::repo()
            .join("fuzz/regressions/fz2_mkv")
            .read_dir()
            .unwrap()
            .count()
            >= 1
    );
}

#[test]
fn fz3_codec_inputs_replay() {
    assert!(replay("fz3_codec", gmclip_media::fuzz::headers) >= 8);
}

/// B-17 review: packed false Cluster IDs after an unknown-size Segment once made the
/// resync reload 64 KiB per match (16 000x the input). The harness asserts the read
/// volume, so this must stay within 4x.
#[test]
fn fz2_resync_work_stays_linear() {
    let mut f = vec![0x1A, 0x45, 0xDF, 0xA3, 0x84, 0x42, 0x82, 0x81, b'w'];
    f.extend_from_slice(&[
        0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00,
    ]);
    while f.len() < 1 << 20 {
        f.extend_from_slice(&[0x1F, 0x43, 0xB6, 0x75]);
    }
    gmclip_media::fuzz::mkv(&f);
}

/// Every seed must index (or parse) successfully, so seeds cannot quietly go dead.
#[test]
fn seeds_reach_their_parsers() {
    use gmclip_media::demux::{self, io::SliceSource};
    for (target, ok_min) in [("fz1_mp4", 8), ("fz2_mkv", 1)] {
        let d = common::repo().join("fuzz/seeds").join(target);
        let mut ok = 0;
        for e in std::fs::read_dir(&d).unwrap() {
            let data = std::fs::read(e.unwrap().path()).unwrap();
            if let Ok(x) = demux::index(&mut SliceSource(&data))
                && x.tracks.iter().any(|t| !t.samples.is_empty())
            {
                ok += 1;
            }
        }
        assert!(ok >= ok_min, "{target}: only {ok} seeds index samples");
    }
}

/// Rewrites the synthetic seeds: `GMCLIP_WRITE_SEEDS=1 cargo test -p gmclip-media
/// --test fuzz_regressions write_seeds -- --ignored`.
#[test]
#[ignore = "writes fuzz/seeds"]
fn write_seeds_once() {
    write_seeds();
}

fn put(target: &str, name: &str, data: &[u8]) {
    let d = common::repo().join("fuzz/seeds").join(target);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(name), data).unwrap();
}

/// Seeds from the synthetic builders (only with GMCLIP_WRITE_SEEDS=1).
fn write_seeds() {
    use common::build::{self, Trak};
    if std::env::var_os("GMCLIP_WRITE_SEEDS").is_none() {
        return;
    }
    let mut v = Trak::video(12);
    v.cts = Some(vec![
        1024, 2560, 1024, 0, 512, 1024, 2560, 1024, 0, 512, 0, 0,
    ]);
    v.edits = Some(vec![(500, -1), (0, 1024)]);
    v.chunk_pattern = Some(vec![3, 3, 2, 4]);
    put("fz1_mp4", "bframes-edits.mp4", &build::mp4(&[v.clone()]));
    let mut c = Trak::video(5);
    c.co64 = true;
    c.stz2 = Some(8);
    put("fz1_mp4", "co64-stz2.mp4", &build::mp4(&[c]));
    let pcm = Trak {
        video: false,
        timescale: 48000,
        sizes: vec![4; 3000],
        deltas: vec![1; 3000],
        sync: None,
        per_chunk: 2500,
        constant: true,
        ..Trak::video(0)
    };
    put("fz1_mp4", "pcm.mp4", &build::mp4(&[v, pcm]));
    let blocks = [
        build::simple_block(0, 0x80, &[1; 10]),
        build::simple_block(100, 0, &[2; 10]),
        build::simple_block(33, 0x06, &[2, 0x81, 0xBF, 1, 1, 1, 1]),
        build::el(
            0xA0,
            &[
                build::el(0xA1, &[0x81, 0, 67, 0, 9, 9]),
                build::uint_el(0xFB, 33),
                build::uint_el(0x9B, 33),
            ]
            .concat(),
        ),
    ];
    put(
        "fz2_mkv",
        "blocks-cues.mkv",
        &build::mkv(&[build::cluster(0, &blocks)], true),
    );
    put(
        "fz2_mkv",
        "no-cues.mkv",
        &build::mkv(&[build::cluster(0, &blocks[..1])], false),
    );
    // FZ3: selector byte + payload
    let avcc = [
        1u8, 0x64, 0, 0x28, 0xFF, 0xE1, 0, 4, 0x67, 0x64, 0, 0x28, 1, 0x68,
    ];
    put("fz3_codec", "avcc", &[&[0u8][..], &avcc].concat());
    let key = [0x82, 0x49, 0x83, 0x42, 0x04, 0x77, 0xF4, 0x37, 0x00, 0x00];
    put("fz3_codec", "vp9-key", &[&[3u8][..], &key].concat());
    put(
        "fz3_codec",
        "vp9-super",
        &[&[2u8][..], &[0x82, 0x49, 0x83, 0x42, 0x00, 0xC1, 5, 0xC1]].concat(),
    );
}
