//! Lab input shared by `vt_perf_lab` and `rcomp1_lab`: (a′), the 3-minute 1440p30 H.264
//! trim with B-frames (`a-prime-mmo-1440p-180s.mp4` in `$GMCLIP_LAB_MEDIA`, default
//! `target/lab-media`; the media crate's `vt_h264` lab cuts it), and `native-mode`
//! stretched to 60 s over it.
#![allow(dead_code)]

use std::path::PathBuf;

use gmclip_plan::Plan;

pub fn lab_dir() -> PathBuf {
    std::env::var_os("GMCLIP_LAB_MEDIA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/lab-media"))
}

pub fn a_prime() -> Option<PathBuf> {
    let p = lab_dir().join("a-prime-mmo-1440p-180s.mp4");
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("skipped: no {}", p.display());
        None
    }
}

/// native-mode over (a′), 1 800 frames: [0, 750) live from 300, [750, 1050) from 2000
/// with the screen held on 2005, [1050, 1800) live from 3600 (a seek forward); a colour
/// background.
pub fn plan(src: &std::path::Path) -> Plan {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text =
        std::fs::read_to_string(root.join("schemas/examples/native-mode.renderplan.json")).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["output"]["frames"] = 1800.into();
    let s = &mut v["sources"][0];
    s["path"] = src.to_str().unwrap().into();
    s["expect"]["video"]["timescale"] = 10_000.into();
    let ranges = [
        (0u64, 750u64, 300u64),
        (750, 1050, 2000),
        (1050, 1800, 3600),
    ];
    for (k, (a, b, first)) in ranges.into_iter().enumerate() {
        let p = &mut v["parts"][k];
        p["out"] = serde_json::json!([a, b]);
        p["picture"]["first_frame"] = first.into();
        p["picture"].as_object_mut().unwrap().remove("expect_pts");
        p["audio"]["first_sample"] = (first * 1600).into();
        for l in p["layers"].as_array_mut().unwrap() {
            let o = l.as_object_mut().unwrap();
            if k == 1 && o.get("time_map").is_some() {
                o["time_map"]["frame"] = (first + 5).into();
            } else {
                o.remove("time_map");
            }
        }
    }
    // a colour background (the plate is M2)
    v["background"] = serde_json::json!({"kind": "colour", "rgb": "#0c1118"});
    let plan = gmclip_plan::parse(serde_json::to_string(&v).unwrap().as_bytes()).unwrap();
    let errors = gmclip_plan::check_plan(&plan);
    assert!(errors.is_empty(), "{errors:?}");
    plan
}
