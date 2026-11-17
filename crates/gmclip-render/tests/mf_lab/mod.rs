//! The Windows lab tests' shared inputs: the lab pack's 1080p30 H.264 source and a
//! three-part plan over it, derived from `schemas/examples/native-mode.renderplan.json`
//! (a seek, a held frame, two layers, static loudness).
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use gmclip_plan::Plan;

/// Output frames of [`plan`].
pub const FRAMES: u64 = 120;

/// The lab source, or `None` (skipped) without `$GMCLIP_LAB_MEDIA`.
pub fn lab() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("GMCLIP_LAB_MEDIA")?);
    let p = d.join("h264_high_aac.mp4");
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("skipped: no {} (set GMCLIP_LAB_MEDIA)", p.display());
        None
    }
}

/// native-mode's three parts over the lab source: part 1 from frame 0, part 2 a seek
/// to frame 150 with the screen held on 160, part 3 back to frame 30 with a frozen
/// screen; the cam crop moved inside 1920×1080.
pub fn plan(source: &Path) -> Plan {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text =
        std::fs::read_to_string(root.join("schemas/examples/native-mode.renderplan.json")).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["output"]["frames"] = FRAMES.into();
    let s = &mut v["sources"][0];
    s["path"] = source.to_str().unwrap().into();
    s["expect"]["video"]["w"] = 1920.into();
    s["expect"]["video"]["h"] = 1080.into();
    let ranges = [(0u64, 45u64, 0u64), (45, 90, 150), (90, 120, 30)];
    for (k, (a, b, first)) in ranges.into_iter().enumerate() {
        let p = &mut v["parts"][k];
        p["out"] = serde_json::json!([a, b]);
        p["picture"]["first_frame"] = first.into();
        p["picture"]["expect_pts"]["ticks"] = (first * 512).into();
        p["audio"]["first_sample"] = (first * 1600).into();
        for l in p["layers"].as_array_mut().unwrap() {
            if l["id"] == "cam" {
                l["crop"] = serde_json::json!({"x": 1400, "y": 600, "w": 496, "h": 340});
            }
            if l["time_map"]["mode"] == "hold" {
                l["time_map"]["frame"] = 160.into();
            }
        }
    }
    // a colour background (the plate is M2 on every platform)
    v["background"] = serde_json::json!({"kind": "colour", "rgb": "#0c1118"});
    v["audio"]["loudness"] =
        serde_json::json!({"mode": "static", "integrated_lufs": -16, "true_peak_dbtp": -1.5});
    let plan = gmclip_plan::parse(serde_json::to_string(&v).unwrap().as_bytes()).unwrap();
    let errors = gmclip_plan::check_plan(&plan);
    assert!(errors.is_empty(), "{errors:?}");
    plan
}
