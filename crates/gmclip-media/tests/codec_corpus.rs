//! B-03 on real media (lab): the H.264 reorder depth equals ffprobe's `has_b_frames`
//! (and profile/level/colour agree) on every H.264 stream of the corpus and the lab
//! sources; on VP9 the synthesised `vpcC` equals the `tools/`-made reference (ffmpeg's
//! MP4 muxer, `vpcc_ref` in the goldens) and every packet's superframe index and frame
//! header parse.

#[path = "demux_common/mod.rs"]
mod common;

use gmclip_media::codec::{h264, vp9};
use gmclip_media::demux::{self, Codec, TrackKind, io::ByteSource, io::FileSource};

const H264: &[&str] = &[
    "a-prime",
    "editlist-mov",
    "vfr",
    "zoo-v-midgop",
    "zoo-v-offframe",
    "zoo-a-straddle",
    "zoo-a-late",
    "zoo-empty",
    "zoo-neg-cts",
    "a",
    "proxy-loc_4fadb756",
    "proxy-loc_086da141",
    "rtw-2565-2615",
    "mmo-oct05",
];

fn profile_name(p: u8, constraints: u8) -> &'static str {
    match p {
        66 if constraints & 0x40 != 0 => "Constrained Baseline",
        66 => "Baseline",
        77 => "Main",
        88 => "Extended",
        100 => "High",
        110 => "High 10",
        122 => "High 4:2:2",
        244 => "High 4:4:4 Predictive",
        _ => "?",
    }
}

fn code_name(kind: &str, v: u8) -> &'static str {
    match (kind, v) {
        (_, 1) => "bt709",
        (_, 2) => "unknown",
        ("matrix", 6) | ("primaries", 6) => "smpte170m",
        ("transfer", 6) => "smpte170m",
        ("primaries", 5) => "bt470bg",
        ("matrix", 5) => "bt470bg",
        (_, 9) => "bt2020",
        _ => "?",
    }
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs test-media/local and the lab sources"
)]
fn h264_sps_agrees_with_ffprobe() {
    let here = common::on_this_machine(H264);
    println!("B-03 H.264 on {} of {} rows", here.len(), H264.len());
    for id in here {
        let golden = common::goldens_for(id);
        let path = common::manifest_path(id).unwrap();
        let d = demux::index(&mut FileSource::open(&path).unwrap()).unwrap();
        let (i, t) = d
            .tracks
            .iter()
            .enumerate()
            .find(|(_, t)| t.kind == TrackKind::Video)
            .unwrap();
        let Codec::H264 { avcc } = &t.codec else {
            panic!("{id}: not H.264")
        };
        let cfg = h264::parse_avcc(avcc).unwrap();
        let sps = h264::parse_sps(&cfg.sps[0]).unwrap();
        let facts = &golden["streams"][i]["ffprobe"];
        let has_b: u8 = facts["has_b_frames"].as_str().map_or_else(
            || facts["has_b_frames"].as_u64().unwrap() as u8,
            |s| s.parse().unwrap(),
        );
        let depth = sps.reorder_depth_with(Some(t.samples.reorder_depth));
        assert_eq!(depth, has_b, "{id}: reorder depth vs has_b_frames");
        assert_eq!(
            profile_name(sps.profile_idc, sps.constraints),
            facts["profile"].as_str().unwrap(),
            "{id}"
        );
        let level = facts["level"]
            .as_str()
            .map_or_else(|| facts["level"].as_i64().unwrap(), |s| s.parse().unwrap());
        assert_eq!(i64::from(sps.level_idc), level, "{id}: level");
        assert_eq!(sps.bit_depth_luma, 8, "{id}");
        assert_eq!(sps.chroma_format_idc, 1, "{id}: 4:2:0");
        if let Some(v) = &sps.vui {
            if let Some((p, tr, m)) = v.colour {
                // ffprobe omits "unknown" values; compare each field it does report.
                for (kind, ours, key) in [
                    ("primaries", p, "color_primaries"),
                    ("transfer", tr, "color_transfer"),
                    ("matrix", m, "color_space"),
                ] {
                    if let Some(theirs) = facts[key].as_str() {
                        assert_eq!(code_name(kind, ours), theirs, "{id}: {key}");
                    }
                }
            }
            if let (Some(full), Some(r)) = (v.video_full_range, facts["color_range"].as_str()) {
                assert_eq!(if full { "pc" } else { "tv" }, r, "{id}: range");
            }
        }
        println!(
            "{id}: {} level {} reorder {depth} (has_b_frames {has_b}; table {}, SPS alone {}), vui reorder {:?}",
            profile_name(sps.profile_idc, sps.constraints),
            sps.level_idc,
            t.samples.reorder_depth,
            sps.reorder_depth(),
            sps.vui.as_ref().and_then(|v| v.max_num_reorder_frames)
        );
    }
}

fn read(src: &mut FileSource, off: u64, size: u32) -> Vec<u8> {
    let mut b = vec![0u8; size as usize];
    src.read_at(off, &mut b).unwrap();
    b
}

fn vp9_case(id: &str, stride: usize) {
    if common::on_this_machine(&[id]).is_empty() {
        return;
    }
    let golden = common::goldens_for(id);
    let path = common::manifest_path(id).unwrap();
    let mut src = FileSource::open(&path).unwrap();
    let d = demux::index(&mut src).unwrap();
    let (i, t) = d
        .tracks
        .iter()
        .enumerate()
        .find(|(_, t)| t.kind == TrackKind::Video)
        .unwrap();
    assert!(matches!(t.codec, Codec::Vp9 { .. }));
    let s = &t.samples;
    let first = read(&mut src, s.offset[0], s.size[0]);
    let frames = vp9::superframe(&first).unwrap();
    let key = frames
        .iter()
        .map(|&(o, n)| vp9::parse_header(&first[o..o + n]).unwrap())
        .find(|h| h.key_frame)
        .unwrap();
    let fps = s.cfr.map_or(30.0, |r| r.num as f64 / f64::from(r.den));
    let ours = vp9::synthesize_vpcc(&key, fps, &t.colour).unwrap();
    let hex: String = ours.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        hex,
        golden["streams"][i]["vpcc_ref"].as_str().unwrap(),
        "{id}: vpcC"
    );
    vp9::parse_vpcc(&ours).unwrap();
    let (mut packets, mut supers, mut hidden, mut existing) = (0, 0, 0, 0);
    for k in (0..s.len()).step_by(stride) {
        let p = read(&mut src, s.offset[k], s.size[k]);
        let fr = vp9::superframe(&p).unwrap_or_else(|e| panic!("{id} packet {k}: {e}"));
        packets += 1;
        if fr.len() > 1 {
            supers += 1;
        }
        for (o, n) in fr {
            let h =
                vp9::parse_header(&p[o..o + n]).unwrap_or_else(|e| panic!("{id} packet {k}: {e}"));
            if !h.show_frame {
                hidden += 1;
            }
            if h.show_existing_frame {
                existing += 1;
            }
            if h.key_frame {
                assert!(s.sync[k], "{id}: key frame in a non-sync packet {k}");
                assert_eq!(
                    (h.width, h.height),
                    (Some(t.width), Some(t.height)),
                    "{id}: size"
                );
            }
        }
    }
    println!(
        "{id}: vpcC {hex} equals reference; {packets} packets parsed, {supers} superframes, {hidden} hidden frames, {existing} show_existing"
    );
}

#[test]
#[cfg_attr(not(feature = "lab"), ignore = "lab: needs test-media/local")]
fn vp9_b_prime_vpcc_and_every_packet() {
    vp9_case("b-prime", 1);
}

#[test]
#[cfg_attr(not(feature = "lab"), ignore = "lab: needs source (b)")]
fn vp9_b_vpcc_and_packets() {
    vp9_case("b", 7);
}
