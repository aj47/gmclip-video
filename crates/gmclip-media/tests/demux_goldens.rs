//! B-01 / B-02: our index equals the ffprobe packet goldens (W0-06) on corpus v0 and the
//! lab sources. Lab-only: needs test-media/local (tools/make_corpus.sh) and the
//! sources in test-media/manifest.toml. `cargo test -p gmclip-media --features lab
//! --release --test demux_goldens -- --nocapture` prints one line per stream and view.

#[path = "demux_common/mod.rs"]
mod common;

/// Checks the rows of `cases` on this machine (a lab-only row that is not here is skipped
/// by name; a corpus row that is not here fails).
fn run(cases: &[&str]) {
    let here = common::on_this_machine(cases);
    if here.len() < cases.len() {
        println!("checked {} of {} rows: {here:?}", here.len(), cases.len());
    }
    for id in here {
        for line in common::check(id) {
            println!("{line}");
        }
    }
}

#[test]
#[cfg_attr(not(feature = "lab"), ignore = "lab: needs test-media/local")]
fn mp4_corpus_equals_goldens() {
    run(&[
        "a-prime",
        "editlist-mov",
        "vfr",
        "zoo-v-midgop",
        "zoo-v-offframe",
        "zoo-a-straddle",
        "zoo-a-late",
        "zoo-empty",
        "zoo-neg-cts",
        "zoo-ipcm",
    ]);
}

#[test]
#[cfg_attr(not(feature = "lab"), ignore = "lab: needs the lab sources")]
fn mp4_lab_sources_equal_goldens() {
    run(&[
        "a",
        "proxy-loc_4fadb756",
        "proxy-loc_086da141",
        "rtw-2565-2615",
        "mmo-oct05",
    ]);
}

#[test]
#[cfg_attr(not(feature = "lab"), ignore = "lab: needs test-media/local")]
fn mkv_corpus_equals_goldens() {
    run(&["b-prime"]);
}

#[test]
#[cfg_attr(not(feature = "lab"), ignore = "lab: needs the lab sources")]
fn mkv_lab_source_equals_goldens() {
    run(&["b"]);
}

/// B-01: indexing the 2.9 GB MMO restream (a) reads box headers and `moov` only — no
/// byte of any `mdat` payload — and takes ≤ 1.5 s on the M4 Pro (release build).
#[test]
#[cfg_attr(not(feature = "lab"), ignore = "lab: needs source (a)")]
fn mmo_index_reads_no_mdat_and_is_fast() {
    use gmclip_media::demux::{io::FileSource, mp4};
    let path = common::manifest_path("a").unwrap();
    if gmclip_model::lab::skip_without([&path]) {
        return;
    }
    let mut src = FileSource::open(&path).unwrap().with_trace();
    let t0 = std::time::Instant::now();
    let d = gmclip_media::demux::index(&mut src).unwrap();
    let took = t0.elapsed();
    let mut plain = FileSource::open(&path).unwrap();
    let mdat: Vec<_> = mp4::top_level(&mut plain)
        .unwrap()
        .into_iter()
        .filter(|b| &b.kind == b"mdat")
        .map(|b| (b.offset + b.header, b.offset + b.size))
        .collect();
    assert!(!mdat.is_empty());
    let mut bytes = 0u64;
    for &(at, n) in src.trace() {
        bytes += n as u64;
        for &(a, z) in &mdat {
            assert!(
                at + n as u64 <= a || at >= z,
                "read [{at}, +{n}) touches mdat payload [{a}, {z})"
            );
        }
    }
    let samples: usize = d.tracks.iter().map(|t| t.samples.len()).sum();
    println!(
        "(a) {} bytes: {} reads, {bytes} bytes read, {samples} samples, indexed in {took:?}",
        gmclip_media::demux::io::ByteSource::len(&src),
        src.trace().len()
    );
    if !cfg!(debug_assertions) {
        assert!(
            took.as_secs_f64() <= 1.5,
            "index took {took:?} (budget 1.5 s)"
        );
    }
}

/// B-02: a cue lookup on (b) takes ≤ 5 ms and lands on a cluster that holds a keyframe
/// at or before the asked time.
#[test]
#[cfg_attr(not(feature = "lab"), ignore = "lab: needs source (b)")]
fn mkv_cue_lookup_is_fast() {
    use gmclip_media::demux::{io::FileSource, mkv};
    let path = common::manifest_path("b").unwrap();
    if gmclip_model::lab::skip_without([&path]) {
        return;
    }
    let mut src = FileSource::open(&path).unwrap().with_trace();
    let t0 = std::time::Instant::now();
    let d = gmclip_media::demux::index(&mut src).unwrap();
    let bytes: u64 = src.trace().iter().map(|&(_, n)| n as u64).sum();
    let len = gmclip_media::demux::io::ByteSource::len(&src);
    println!(
        "(b) indexed in {:?}: {} reads, {bytes} of {len} bytes ({:.1}%), {} cue points",
        t0.elapsed(),
        src.trace().len(),
        100.0 * bytes as f64 / len as f64,
        d.cues.len()
    );
    // The walk reads element and block headers, not frame data.
    assert!(bytes * 4 < len, "read {bytes} of {len} bytes");
    let video = d
        .tracks
        .iter()
        .find(|t| t.kind == gmclip_media::demux::TrackKind::Video)
        .unwrap();
    let t0 = std::time::Instant::now();
    let mut found = 0;
    for k in 0..1000u64 {
        let t = k * 4_962_000 / 1000;
        if let Some(c) = mkv::cue_for(&d.cues, video.id, t) {
            assert!(c.time <= t);
            found += 1;
        }
    }
    let per = t0.elapsed() / 1000;
    println!("(b) cue lookup: {per:?} each, {found}/1000 found");
    assert!(per.as_micros() <= 5_000);
    // Every cue points at a cluster whose first video frame is a keyframe at cue time.
    for c in d.cues.iter().filter(|c| c.track == video.id).take(50) {
        let i = video
            .samples
            .dts
            .iter()
            .position(|&t| t == c.time as i64)
            .unwrap();
        assert!(video.samples.sync[i], "cue at {} is not a keyframe", c.time);
    }
}
