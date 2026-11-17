//! RenderPlan `/1` (decision 0014): image and shape events, the blurred-source
//! background and text animation are accepted under `/1` and refused under `/0`, and the
//! `/1` rules (references, ranges, unique ids, run indices) hold.

mod common;

use serde_json::{Value, json};

/// One change to a plan's JSON.
type Mutation = dyn Fn(&mut Value);

/// The RP §2.3 example with every `/1` addition used once.
fn v1() -> Value {
    let mut v = common::h79();
    v["schema"] = json!("gmclip.renderplan/1");
    v["background"] = json!({"kind": "blurred_source", "rgb": "#0c1118", "blur": 24, "dim": 0.3});
    v["text_events"][0]["anim"] = json!({"kind": "bounce", "pop_runs": [0]});
    v["images"] = json!([{"id": "logo", "path": "/tmp/logo.qoi", "sha256": "ab".repeat(32), "w": 400, "h": 200}]);
    v["image_events"] = json!([{"id": "i1", "out": [0, 30], "layer": 10, "image": "logo", "place": {"x": 40, "y": 40, "w": 200, "h": 100}, "opacity": 0.9}]);
    v["shape_events"] = json!([
        {"id": "b1", "out": [0, 30], "shape": {"kind": "box", "rect": {"x": 100, "y": 300, "w": 400, "h": 200}, "width": 8, "stroke": "#FF3B30FF", "radius": 12}},
        {"id": "a1", "out": [10, 40], "layer": 25, "shape": {"kind": "arrow", "from": {"x": 700, "y": 900}, "to": {"x": 500, "y": 500}, "width": 10, "stroke": "#FFD400FF"}}
    ]);
    v
}

#[test]
fn a_v1_plan_parses() {
    let p = common::validate(&v1()).unwrap();
    assert_eq!(p.schema, gmclip_plan::consts::SchemaVersion::V1);
    assert_eq!(p.image_events.as_ref().unwrap().len(), 1);
    assert_eq!(p.shape_events.as_ref().unwrap().len(), 2);
    // round trip: the typed plan serialises to a plan that parses to itself
    let back = serde_json::to_value(&p).unwrap();
    assert_eq!(common::validate(&back).unwrap(), p);
    assert_eq!(back["shape_events"][1]["shape"]["kind"], "arrow");
    assert_eq!(back["background"]["kind"], "blurred_source");
}

#[test]
fn v1_additions_are_refused_under_v0() {
    let rows: [(&str, &Mutation, &str); 4] = [
        (
            "background",
            &|v| v["background"] = v1()["background"].clone(),
            "/background",
        ),
        (
            "anim",
            &|v| v["text_events"][0]["anim"] = json!({"kind": "fade"}),
            "/text_events/0/anim",
        ),
        (
            "images",
            &|v| v["images"] = v1()["images"].clone(),
            "/images",
        ),
        (
            "shape_events",
            &|v| v["shape_events"] = v1()["shape_events"].clone(),
            "/shape_events",
        ),
    ];
    for (what, f, at) in rows {
        let mut v = common::h79();
        f(&mut v);
        let e = common::validate(&v).expect_err(what);
        assert_eq!(e.code(), "Invalid", "{what}: {e}");
        assert!(e.path().starts_with(at), "{what}: {e}");
    }
}

#[test]
fn v1_rules() {
    let bad: [(&Mutation, &str); 6] = [
        (
            &|v| v["image_events"][0]["image"] = json!("nope"),
            "/image_events/0/image",
        ),
        (
            &|v| v["image_events"][0]["out"] = json!([0, 99999]),
            "/image_events/0/out",
        ),
        (
            &|v| v["shape_events"][1]["id"] = json!("b1"),
            "/shape_events/1/id",
        ),
        (
            &|v| v["shape_events"][0]["out"] = json!([5, 5]),
            "/shape_events/0/out",
        ),
        (
            &|v| v["text_events"][0]["anim"]["pop_runs"] = json!([7]),
            "/text_events/0/anim/pop_runs/0",
        ),
        (
            &|v| {
                v.as_object_mut().unwrap().remove("images");
            },
            "/images",
        ),
    ];
    for (f, at) in bad {
        let mut v = v1();
        f(&mut v);
        let e = common::validate(&v).expect_err(at);
        assert_eq!(e.path(), at, "{e}");
    }
    // the blur and dim bounds are the schema's
    let mut v = v1();
    v["background"]["blur"] = json!(1);
    assert!(common::validate(&v).is_err());
    let mut v = v1();
    v["background"]["dim"] = json!(1.5);
    assert!(common::validate(&v).is_err());
}

/// Decision 0016: a layer's `ease` is `/1` only; its frames and curve are the schema's.
#[test]
fn layer_ease() {
    let ease = json!({"from_crop": {"x": 0, "y": 0, "w": 1280, "h": 720}, "from_place": {"x": 0, "y": 0, "w": 1080, "h": 607.5}, "frames": 12, "curve": "ease_in_out"});
    let mut v = v1();
    v["parts"][0]["layers"][0]["ease"] = ease.clone();
    let p = common::validate(&v).unwrap();
    let e = p.parts[0].layers[0].ease.as_ref().unwrap();
    assert_eq!(e.frames, 12);
    // halfway through an ease-in-out is halfway between the two values
    let (c, _, _) = e.at(
        6,
        gmclip_plan::types::Rect {
            x: 1280.0,
            y: 0.0,
            w: 1280.0,
            h: 720.0,
        },
        e.from_place,
        1.0,
    );
    assert!((c.x - 640.0).abs() < 1e-9, "{c:?}");
    // from `frames` on: the layer's own values
    let own = gmclip_plan::types::Rect {
        x: 9.0,
        y: 9.0,
        w: 9.0,
        h: 9.0,
    };
    assert_eq!(e.at(12, own, own, 1.0).0, own);
    let mut v0 = common::h79();
    v0["parts"][0]["layers"][0]["ease"] = ease.clone();
    let err = common::validate(&v0).expect_err("ease under /0");
    assert!(err.path().starts_with("/parts/0/layers/0"), "{err}");
    for bad in [json!(0), json!("x")] {
        let mut v = v1();
        v["parts"][0]["layers"][0]["ease"] = ease.clone();
        v["parts"][0]["layers"][0]["ease"]["frames"] = bad;
        assert!(common::validate(&v).is_err());
    }
    let mut v = v1();
    v["parts"][0]["layers"][0]["ease"] = ease;
    v["parts"][0]["layers"][0]["ease"]["curve"] = json!("bounce");
    assert!(common::validate(&v).is_err());
}

/// Decision 0017: a picture source at another nominal rate than `output.fps` renders
/// through `parts[].picture.fps` (`/1` only); without it, it stays `Unsupported`.
#[test]
fn picture_rate_conversion() {
    let with_rate = |src: Value, part: Option<Value>, version: &str| {
        let mut v = common::h79();
        v["schema"] = json!(version);
        v["sources"][0]["expect"]["video"]["fps"] = src;
        let n = v["parts"].as_array().unwrap().len();
        for i in 0..n {
            if let Some(r) = &part {
                v["parts"][i]["picture"]["fps"] = r.clone();
            }
        }
        v
    };
    let ntsc = json!({"num": 30000, "den": 1001});
    // every part converts: valid, and round-trips
    for (src, rate) in [
        (ntsc.clone(), ntsc.clone()),
        (json!({"num": 60, "den": 1}), json!({"num": 60, "den": 1})),
        (
            json!({"num": 24000, "den": 1001}),
            json!({"num": 24000, "den": 1001}),
        ),
        (json!({"num": 25, "den": 1}), json!({"num": 25, "den": 1})),
        (
            json!({"num": 60000, "den": 1001}),
            json!({"num": 60000, "den": 1001}),
        ),
    ] {
        let v = with_rate(src, Some(rate), "gmclip.renderplan/1");
        let p = common::validate(&v).unwrap();
        assert!(p.parts.iter().all(|p| p.picture.fps.is_some()));
        assert_eq!(
            common::validate(&serde_json::to_value(&p).unwrap()).unwrap(),
            p
        );
    }
    // AJ's 29.97 file in a 30 fps cut, no conversion: still Unsupported, at the source
    let e = common::validate(&with_rate(ntsc.clone(), None, "gmclip.renderplan/0")).unwrap_err();
    assert_eq!(
        (e.code(), e.path()),
        ("Unsupported", "/sources/0/expect/video/fps")
    );
    // `/0` cannot say it
    let e = common::validate(&with_rate(
        ntsc.clone(),
        Some(ntsc.clone()),
        "gmclip.renderplan/0",
    ))
    .unwrap_err();
    assert_eq!(e.code(), "Invalid");
    assert!(e.path().starts_with("/parts/0/picture"), "{e}");
    // equal to the output rate, or not the source's declared rate: Invalid
    let e = common::validate(&with_rate(
        json!({"num": 30, "den": 1}),
        Some(json!({"num": 60, "den": 2})),
        "gmclip.renderplan/1",
    ))
    .unwrap_err();
    assert_eq!(e.path(), "/parts/0/picture/fps", "{e}");
    let e = common::validate(&with_rate(
        json!({"num": 60, "den": 1}),
        Some(json!({"num": 50, "den": 1})),
        "gmclip.renderplan/1",
    ))
    .unwrap_err();
    assert_eq!(e.path(), "/parts/0/picture/fps", "{e}");
    // one part that does not convert keeps the source unsupported
    let mut v = with_rate(ntsc.clone(), Some(ntsc), "gmclip.renderplan/1");
    v["parts"][1]["picture"]
        .as_object_mut()
        .unwrap()
        .remove("fps");
    let e = common::validate(&v).unwrap_err();
    assert_eq!(
        (e.code(), e.path()),
        ("Unsupported", "/sources/0/expect/video/fps")
    );
}

/// Decision 0020: NTSC output rates are `/1` only; their sample clock rounds down and
/// telescopes.
#[test]
fn ntsc_output_rates() {
    use gmclip_plan::validate::{out_samples, part_samples};
    for (num, spf) in [(24000u64, 2002u64), (30000, 1601), (60000, 800)] {
        let mut v = common::h79();
        v["schema"] = json!("gmclip.renderplan/1");
        v["output"]["fps"] = json!({"num": num, "den": 1001});
        // the source declares the output's rate, so no conversion is needed
        v["sources"][0]["expect"]["video"]["fps"] = json!({"num": num, "den": 1001});
        let p = common::validate(&v).unwrap_or_else(|e| panic!("{num}/1001: {e}"));
        assert!(p.output.fps.is_ntsc());
        // frames carry ⌊48000·1001/num⌋ or one more, and the total is S(frames)
        let total: u64 = p
            .parts
            .iter()
            .map(|x| part_samples(x.out, p.output.fps).unwrap())
            .sum();
        assert_eq!(Some(total), out_samples(p.output.frames, p.output.fps));
        let per: Vec<u64> = (0..1001)
            .map(|f| {
                out_samples(f + 1, p.output.fps).unwrap() - out_samples(f, p.output.fps).unwrap()
            })
            .collect();
        assert!(per.iter().all(|&n| n == spf || n == spf + 1), "{num}");
        assert_eq!(
            out_samples(1001, p.output.fps),
            Some(48_000 * 1001 * 1001 / num)
        );
        // `/0` cannot say it
        let mut v0 = v.clone();
        v0["schema"] = json!("gmclip.renderplan/0");
        assert!(common::validate(&v0).is_err());
    }
    // pairs: 30000/1 and 30/1001 are not rates
    for bad in [
        json!({"num": 30000, "den": 1}),
        json!({"num": 30, "den": 1001}),
    ] {
        let mut v = common::h79();
        v["schema"] = json!("gmclip.renderplan/1");
        v["output"]["fps"] = bad;
        let e = common::validate(&v).unwrap_err();
        assert_eq!(e.path(), "/output/fps", "{e}");
    }
}

/// Decision 0021: a layer's `cover` (blur, pixelate, solid) is `/1` only; its bounds are
/// the schema's, and a cover layer carries no matte.
#[test]
fn layer_cover() {
    for cover in [
        json!({"kind": "blur", "radius": 24.5}),
        json!({"kind": "pixelate", "block": 16.5}),
        json!({"kind": "solid", "rgb": "#0c1118"}),
    ] {
        let mut v = v1();
        v["parts"][0]["layers"][0]["cover"] = cover.clone();
        let p = common::validate(&v).unwrap();
        assert!(p.parts[0].layers[0].cover.is_some());
        let back = serde_json::to_value(&p).unwrap();
        assert_eq!(back["parts"][0]["layers"][0]["cover"], cover);
        let mut v0 = common::h79();
        v0["parts"][0]["layers"][0]["cover"] = cover;
        let e = common::validate(&v0).expect_err("cover under /0");
        assert!(e.path().starts_with("/parts/0/layers/0"), "{e}");
    }
    for bad in [
        json!({"kind": "blur", "radius": 0}),
        json!({"kind": "blur", "radius": 401}),
        json!({"kind": "pixelate"}),
        json!({"kind": "solid", "rgb": "red"}),
        json!({"kind": "smudge", "radius": 4}),
    ] {
        let mut v = v1();
        v["parts"][0]["layers"][0]["cover"] = bad.clone();
        assert!(common::validate(&v).is_err(), "{bad}");
    }
    let mut v = v1();
    v["parts"][0]["layers"][0]["cover"] = json!({"kind": "blur", "radius": 8});
    v["parts"][0]["layers"][0]["matte"] = json!({"kind": "roto", "model": "apple_vision"});
    let e = common::validate(&v).unwrap_err();
    assert_eq!(e.path(), "/parts/0/layers/0/cover", "{e}");
    // the small picture a cover resamples through
    use gmclip_plan::types::LayerCover;
    assert_eq!(
        LayerCover::Blur { radius: 24.0 }.grid(480.0, 100.0),
        (20, 4)
    );
    assert_eq!(
        LayerCover::Pixelate { block: 1000.0 }.grid(480.0, 100.0),
        (1, 1)
    );
    let solid = LayerCover::Solid {
        rgb: serde_json::from_value(json!("#000000")).unwrap(),
    };
    assert_eq!(solid.grid(480.0, 100.0), (1, 1));
}

/// Decision 0027: `/1` lifts `max_bps` past the social 25 Mbit/s for an upload canvas, up
/// to the level's MaxBR; `/0` still stops at 25 Mbit/s, and a portrait canvas never
/// passes it.
#[test]
fn upload_rates_above_the_social_ceiling() {
    let landscape = |v: &mut Value| {
        v["output"]["canvas"] = json!({"w": 2560, "h": 1440});
        for p in v["parts"].as_array_mut().unwrap() {
            for l in p["layers"].as_array_mut().unwrap() {
                l["place"] = l["crop"].clone();
                l["place"]["x"] = json!(0);
                l["place"]["y"] = json!(0);
            }
        }
    };
    let with = |schema: &str, level: &str, target: u64, max: u64, land: bool| {
        let mut v = common::h79();
        v["schema"] = json!(schema);
        if land {
            landscape(&mut v);
        }
        v["output"]["video"]["level"] = json!(level);
        v["output"]["video"]["rate"] = json!({"mode": "vbr", "target_bps": target, "max_bps": max});
        v
    };
    let v1s = "gmclip.renderplan/1";
    // 1440p60 at 36 / 54 Mbit/s, High 5.1
    let p = common::validate(&with(v1s, "5.1", 36_000_000, 54_000_000, true)).unwrap();
    assert_eq!(
        p.output.video.rate.as_ref().unwrap().max_bps,
        Some(54_000_000)
    );
    // the level's MaxBR is the cap (4.2: 62.5 Mbit/s; 5.1: 300)
    assert!(common::validate(&with(v1s, "4.2", 40_000_000, 62_500_000, true)).is_ok());
    let rows: [(Value, &str); 5] = [
        // /0 keeps its 25 Mbit/s bound (a schema error)
        (
            with("gmclip.renderplan/0", "5.1", 36_000_000, 54_000_000, true),
            "/output/video/rate/max_bps",
        ),
        // a portrait (social) canvas never passes 25 Mbit/s
        (
            with(v1s, "5.1", 20_000_000, 30_000_000, false),
            "/output/video/rate/max_bps",
        ),
        // over the level's MaxBR
        (
            with(v1s, "4.0", 20_000_000, 30_000_000, true),
            "/output/video/rate/max_bps",
        ),
        (
            with(v1s, "4.2", 40_000_000, 70_000_000, true),
            "/output/video/rate/max_bps",
        ),
        // the target above the peak
        (
            with(v1s, "5.1", 60_000_000, 54_000_000, true),
            "/output/video/rate/target_bps",
        ),
    ];
    for (v, at) in rows {
        let e = common::validate(&v).expect_err(at);
        assert_eq!(e.code(), "Invalid", "{at}: {e}");
        assert_eq!(e.path(), at, "{e}");
    }
    // the schema's own ceiling
    assert!(common::validate(&with(v1s, "5.1", 1, 300_000_001, true)).is_err());
}

/// Decision 0028: a backdrop (a video or a still under every part, on the output clock)
/// and clip events (videos picture-in-picture) are `/1` only, and their rules hold.
#[test]
fn backdrop_and_clip_events() {
    let with = |schema: &str| {
        let mut v = common::h79();
        v["schema"] = json!(schema);
        v["sources"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "rain", "path": "/tmp/rain.mp4"}));
        v["background"] = json!({"kind": "backdrop", "rgb": "#000000",
            "crop": {"x": 0, "y": 0, "w": 1080, "h": 1920}, "place": {"x": 0, "y": 0, "w": 1080, "h": 1920},
            "video": {"source": "rain", "first_frame": 0, "frames": 300, "loop": true}});
        v["clip_events"] = json!([{"id": "k1", "out": [10, 40], "layer": 2,
            "picture": {"source": "rain", "first_frame": 90, "fps": {"num": 60, "den": 1}},
            "crop": {"x": 0, "y": 0, "w": 1920, "h": 1080}, "place": {"x": 100, "y": 300, "w": 480, "h": 270},
            "opacity": 0.9}]);
        v
    };
    let p = common::validate(&with("gmclip.renderplan/1")).unwrap();
    assert_eq!(p.clip_events.as_ref().unwrap()[0].picture.local(5), 5);
    let back = serde_json::to_value(&p).unwrap();
    assert_eq!(back["background"]["video"]["loop"], true);
    assert_eq!(common::validate(&back).unwrap(), p);
    // the local frame: looped, held, or straight
    let gmclip_plan::types::Background::Backdrop { video: Some(v), .. } = &p.background else {
        panic!("a video backdrop");
    };
    assert_eq!((v.local(299), v.local(300), v.local(301)), (299, 0, 1));
    let mut held = v.clone();
    held.r#loop = None;
    assert_eq!((held.local(299), held.local(1000)), (299, 299));
    // both refused under /0
    for at in ["/background", "/clip_events"] {
        let e = common::validate(&with("gmclip.renderplan/0")).expect_err(at);
        assert_eq!(e.code(), "Invalid", "{e}");
        let mut v = with("gmclip.renderplan/0");
        if at == "/clip_events" {
            v["background"] = common::h79()["background"].clone();
        } else {
            v.as_object_mut().unwrap().remove("clip_events");
        }
        let e = common::validate(&v).expect_err(at);
        assert!(e.path().starts_with(at), "{at}: {e}");
    }
    let bad: [(&Mutation, &str); 7] = [
        (&|v| v["background"]["image"] = json!("logo"), "/background"),
        (
            &|v| v["background"]["video"]["source"] = json!("nope"),
            "/background/video/source",
        ),
        (
            &|v| v["background"]["place"]["w"] = json!(500),
            "/background/place",
        ),
        (
            &|v| v["clip_events"][0]["out"] = json!([10, 99999]),
            "/clip_events/0/out",
        ),
        (
            &|v| v["clip_events"][0]["picture"]["fps"] = json!({"num": 30, "den": 1}),
            "/clip_events/0/picture/fps",
        ),
        (
            &|v| v["clip_events"][0]["place"]["h"] = json!(400),
            "/clip_events/0/place",
        ),
        (
            &|v| {
                let e = v["clip_events"][0].clone();
                v["clip_events"].as_array_mut().unwrap().push(e);
            },
            "/clip_events/1/id",
        ),
    ];
    for (f, at) in bad {
        let mut v = with("gmclip.renderplan/1");
        f(&mut v);
        let e = common::validate(&v).expect_err(at);
        assert_eq!(e.path(), at, "{e}");
    }
    // a still backdrop names an image
    let mut v = with("gmclip.renderplan/1");
    v["background"].as_object_mut().unwrap().remove("video");
    v["background"]["image"] = json!("plate");
    v["images"] = json!([{"id": "plate", "path": "/tmp/plate.png", "sha256": "ab".repeat(32), "w": 1080, "h": 1920}]);
    assert!(common::validate(&v).is_ok());
    v["background"]["image"] = json!("other");
    assert_eq!(
        common::validate(&v).unwrap_err().path(),
        "/background/image"
    );
}

/// Decision 0030: `parts[].speed` is `/1` only, in [¼, 4] and never 1; a join touching a
/// sped part is not continuous, so a fade there is allowed.
#[test]
fn part_speed() {
    let mut v = common::h79();
    v["schema"] = json!("gmclip.renderplan/1");
    v["parts"][0]["speed"] = json!({"num": 3, "den": 2, "audio": "stretch"});
    let p = common::validate(&v).unwrap();
    let sp = p.parts[0].speed.unwrap();
    assert!(sp.stretches());
    assert_eq!(sp.source_samples(1000), 1500);
    assert_eq!(sp.source_samples(3), 5, "rounded up");
    assert!(!gmclip_plan::validate::continuous_join(&p, 0));
    let mut v0 = v.clone();
    v0["schema"] = json!("gmclip.renderplan/0");
    let e = common::validate(&v0).unwrap_err();
    assert!(e.path().starts_with("/parts/0/speed"), "{e}");
    for (sp, ok) in [
        (json!({"num": 1, "den": 1}), false),
        (json!({"num": 5, "den": 1}), false),
        (json!({"num": 1, "den": 5}), false),
        (json!({"num": 4, "den": 1}), true),
        (json!({"num": 1, "den": 4, "audio": "mute"}), true),
        (json!({"num": 0, "den": 1}), false),
        (json!({"num": 2, "den": 1, "audio": "loud"}), false),
    ] {
        let mut w = v.clone();
        w["parts"][0]["speed"] = sp.clone();
        assert_eq!(common::validate(&w).is_ok(), ok, "{sp}");
    }
}

/// Decision 0033: a cover pinned to a moving point moves its place by the point's move
/// from `(x, y)` in whole pixels and its crop by that move scaled into source pixels; only
/// a cover is pinned, and its keys increase inside the part.
#[test]
fn layer_track() {
    let mut v = v1();
    let out = v["parts"][0]["out"].clone();
    let (a, z) = (out[0].as_u64().unwrap(), out[1].as_u64().unwrap());
    v["parts"][0]["layers"][0]["cover"] = json!({"kind": "blur", "radius": 12});
    v["parts"][0]["layers"][0]["track"] = json!({"x": 100, "y": 200,
        "keys": [{"frame": a, "x": 100, "y": 200}, {"frame": a + 10, "x": 120.6, "y": 190}]});
    let p = common::validate(&v).unwrap();
    let l = &p.parts[0].layers[0];
    let t = l.track.as_ref().unwrap();
    assert_eq!(t.offset(a + 5), (10, -5));
    assert_eq!(t.offset(z - 1), (21, -10), "held after the last key");
    let (crop, place) = t.moved(a + 10, l.crop, l.place);
    assert_eq!((place.x - l.place.x, place.y - l.place.y), (21.0, -10.0));
    let k = l.crop.w / l.place.w;
    assert!((crop.x - l.crop.x - 21.0 * k).abs() < 1e-9);
    assert_eq!((crop.w, place.w), (l.crop.w, l.place.w));
    let back = serde_json::to_value(&p).unwrap();
    assert_eq!(
        back["parts"][0]["layers"][0]["track"]["keys"][1]["x"],
        120.6
    );
    // under /0, on a layer that is not a cover, keys out of order or outside the part
    let mut v0 = common::h79();
    v0["parts"][0]["layers"][0]["track"] = v["parts"][0]["layers"][0]["track"].clone();
    assert!(common::validate(&v0).is_err());
    for (path, bad) in [
        ("cover", Value::Null),
        ("track", json!({"x": 0, "y": 0, "keys": []})),
        (
            "track",
            json!({"x": 0, "y": 0, "keys": [{"frame": a + 2, "x": 0, "y": 0}, {"frame": a + 1, "x": 0, "y": 0}]}),
        ),
        (
            "track",
            json!({"x": 0, "y": 0, "keys": [{"frame": z, "x": 0, "y": 0}]}),
        ),
    ] {
        let mut w = v.clone();
        if bad.is_null() {
            w["parts"][0]["layers"][0]
                .as_object_mut()
                .unwrap()
                .remove(path);
        } else {
            w["parts"][0]["layers"][0][path] = bad.clone();
        }
        let e = common::validate(&w).expect_err(&format!("{path}: {bad}"));
        assert!(e.path().starts_with("/parts/0/layers/0"), "{e}");
    }
}

/// Decision 0033: a speed ramp goes to another speed in [¼, 4] over frames inside the
/// part; it may start at 1×; the part reaches the source its integral does.
#[test]
fn part_speed_ramp() {
    let mut v = common::h79();
    v["schema"] = json!("gmclip.renderplan/1");
    let len = v["parts"][0]["out"][1].as_u64().unwrap() - v["parts"][0]["out"][0].as_u64().unwrap();
    assert!(len >= 40, "{len}");
    let ramp = |to: (u64, u64), from: u64, frames: u64| json!({"to": {"num": to.0, "den": to.1}, "from": from, "frames": frames, "curve": "ease_in_out"});
    v["parts"][0]["speed"] = json!({"num": 1, "den": 1, "ramp": ramp((2, 1), 0, 20)});
    let p = common::validate(&v).unwrap();
    let sp = p.parts[0].speed.unwrap();
    // 20 frames at an average 1.5×, then 2× for the rest
    assert_eq!(sp.advance(20), Some((30, 1)));
    assert_eq!(sp.source_frames(len), Some(30 + 2 * (len - 20)));
    assert!(!gmclip_plan::validate::continuous_join(&p, 0));
    let mut v0 = v.clone();
    v0["schema"] = json!("gmclip.renderplan/0");
    assert!(common::validate(&v0).is_err());
    for (sp, ok) in [
        (
            json!({"num": 1, "den": 2, "ramp": ramp((3, 1), 5, 10)}),
            true,
        ),
        (
            json!({"num": 1, "den": 1, "ramp": ramp((1, 1), 0, 10)}),
            false,
        ),
        (
            json!({"num": 2, "den": 1, "ramp": ramp((4, 2), 0, 10)}),
            false,
        ),
        (
            json!({"num": 1, "den": 1, "ramp": ramp((5, 1), 0, 10)}),
            false,
        ),
        (
            json!({"num": 1, "den": 1, "ramp": ramp((1, 5), 0, 10)}),
            false,
        ),
        (
            json!({"num": 1, "den": 1, "ramp": ramp((2, 1), 0, 0)}),
            false,
        ),
        (
            json!({"num": 1, "den": 1, "ramp": ramp((2, 1), len - 10, 11)}),
            false,
        ),
        (
            json!({"num": 1, "den": 1, "ramp": ramp((2, 1), len - 10, 10)}),
            true,
        ),
        (
            json!({"num": 1, "den": 1, "ramp": ramp((20001, 10001), 0, 10)}),
            false,
        ),
        (
            json!({"num": 1, "den": 1, "ramp": {"to": {"num": 2, "den": 1}, "from": 0, "frames": 5, "curve": "bounce"}}),
            false,
        ),
    ] {
        let mut w = v.clone();
        w["parts"][0]["speed"] = sp.clone();
        let r = common::validate(&w);
        assert_eq!(r.is_ok(), ok, "{sp}: {r:?}");
        if let Err(e) = r {
            assert!(e.path().starts_with("/parts/0/speed"), "{e}");
        }
    }
}

/// Decision 0031: `tracks` pin an image, shape or text event to a moving point; keys
/// increase inside the event's span; the move is the point's since the event's start.
#[test]
fn event_tracks() {
    let mut v = v1();
    v["tracks"] = json!([
        {"kind": "shape", "event": "b1", "keys": [{"frame": 0, "x": 100, "y": 300}, {"frame": 20, "x": 140.6, "y": 280}]},
        {"kind": "image", "event": "i1", "keys": [{"frame": 5, "x": 10, "y": 10}]}
    ]);
    let p = common::validate(&v).unwrap();
    let t = &p.tracks.as_ref().unwrap()[0];
    assert_eq!(t.at(10), (120.3, 290.0));
    assert_eq!(t.offset(0, 10), (20, -10));
    assert_eq!(t.offset(0, 99), (41, -20), "held after the last key");
    assert_eq!(
        p.tracks.as_ref().unwrap()[1].offset(0, 3),
        (0, 0),
        "held before the first"
    );
    // `/0` refuses the key (on a plan with nothing else of `/1`)
    let mut v0 = common::h79();
    let id = v0["text_events"][0]["id"].clone();
    let at0 = v0["text_events"][0]["out"][0].clone();
    v0["tracks"] = json!([{"kind": "text", "event": id, "keys": [{"frame": at0, "x": 1, "y": 1}]}]);
    assert!(
        common::validate(&v0)
            .unwrap_err()
            .path()
            .starts_with("/tracks")
    );
    v0["schema"] = json!("gmclip.renderplan/1");
    assert!(common::validate(&v0).is_ok());
    let bad: [(&Mutation, &str); 4] = [
        (
            &|v| v["tracks"][0]["event"] = json!("nope"),
            "/tracks/0/event",
        ),
        (
            &|v| v["tracks"][0]["kind"] = json!("text"),
            "/tracks/0/event",
        ),
        (
            &|v| v["tracks"][0]["keys"][1]["frame"] = json!(0),
            "/tracks/0/keys/1/frame",
        ),
        (
            &|v| v["tracks"][0]["keys"][1]["frame"] = json!(31),
            "/tracks/0/keys/1/frame",
        ),
    ];
    for (f, at) in bad {
        let mut w = v.clone();
        f(&mut w);
        let e = common::validate(&w).expect_err(at);
        assert_eq!(e.path(), at, "{e}");
    }
}

/// Decision 0042: clip and image events take a `z` among the part's layers, and a clip
/// event a roto matte; a plan without them writes neither.
#[test]
fn events_among_the_layers() {
    let mut v = common::h79();
    v["schema"] = json!("gmclip.renderplan/1");
    v["sources"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "rain", "path": "/tmp/rain.mp4"}));
    v["clip_events"] = json!([{"id": "k1", "out": [10, 40],
        "picture": {"source": "rain", "first_frame": 0},
        "crop": {"x": 0, "y": 0, "w": 1920, "h": 1080}, "place": {"x": 0, "y": 0, "w": 1080, "h": 608}}]);
    let p = common::validate(&v).unwrap();
    let back = serde_json::to_value(&p).unwrap();
    assert!(back["clip_events"][0].get("z").is_none());
    assert!(back["clip_events"][0].get("matte").is_none());
    v["clip_events"][0]["z"] = json!(-1);
    v["clip_events"][0]["matte"] = json!({"kind": "roto", "model": "apple_vision"});
    let p = common::validate(&v).unwrap();
    let e = &p.clip_events.as_ref().unwrap()[0];
    assert_eq!(e.z, Some(-1));
    assert!(e.matte.is_some());
    assert_eq!(
        common::validate(&serde_json::to_value(&p).unwrap()).unwrap(),
        p
    );
    v["clip_events"][0]["z"] = json!("low");
    assert!(common::validate(&v).is_err(), "z is an integer");
}

/// Decision 0045: a recorded cursor is a source's track and images; a layer of that
/// source draws it. Refused under `/0`, and every reference checked.
#[test]
fn recorded_cursors() {
    let mut v = common::h79();
    v["schema"] = json!("gmclip.renderplan/1");
    v["images"] = json!([{"id": "arrow", "path": "/tmp/arrow.png", "sha256": "cd".repeat(32), "w": 64, "h": 64}]);
    v["cursors"] = json!([{"id": "cur-1", "source": "mkv1440", "path": "/tmp/input.json",
        "sha256": "ef".repeat(32), "shapes": ["arrow"], "scale": 1.5, "smooth_ms": 40, "clicks": true}]);
    v["parts"][0]["layers"][0]["cursor"] = json!("cur-1");
    let p = common::validate(&v).unwrap();
    let c = &p.cursors.as_ref().unwrap()[0];
    assert_eq!(
        (c.scale, c.smooth_ms, c.hide_idle_ms, c.clicks),
        (1.5, Some(40), None, Some(true))
    );
    assert_eq!(
        p.parts[0].layers[0].cursor.as_ref().unwrap().as_str(),
        "cur-1"
    );
    let back = serde_json::to_value(&p).unwrap();
    assert!(back["cursors"][0].get("hide_idle_ms").is_none());
    assert!(back["parts"][0]["layers"][1].get("cursor").is_none());
    assert_eq!(common::validate(&back).unwrap(), p);

    let mut v0 = v.clone();
    v0["schema"] = json!("gmclip.renderplan/0");
    v0.as_object_mut().unwrap().remove("images");
    v0.as_object_mut().unwrap().remove("cursors");
    assert!(common::validate(&v0).is_err(), "a layer cursor under /0");

    let bad: [(&Mutation, &str); 7] = [
        (
            &|v| v["cursors"][0]["source"] = json!("nope"),
            "/cursors/0/source",
        ),
        (
            &|v| v["cursors"][0]["shapes"] = json!(["arrow", "hand"]),
            "/cursors/0/shapes/1",
        ),
        (&|v| v["cursors"][0]["scale"] = json!(0), "/cursors/0/scale"),
        (
            &|v| v["cursors"][0]["smooth_ms"] = json!(-1),
            "/cursors/0/smooth_ms",
        ),
        (
            &|v| v["parts"][0]["layers"][0]["cursor"] = json!("cur-2"),
            "/parts/0/layers/0/cursor",
        ),
        (
            &|v| v["cursors"][0]["source"] = json!("proxy"),
            "/parts/0/layers/0/cursor",
        ),
        (
            &|v| {
                let c = v["cursors"][0].clone();
                v["cursors"].as_array_mut().unwrap().push(c);
            },
            "/cursors",
        ),
    ];
    for (f, at) in bad {
        let mut w = v.clone();
        f(&mut w);
        let e = common::validate(&w).expect_err(at);
        assert!(e.path().starts_with(at), "{at}: {e}");
    }
}

/// Decision 0046: a cursor's feel (smoothness, click style, shrink, shake, loop, motion
/// blur) parses, round-trips and is range-checked; a cursor without it keeps its bytes.
#[test]
fn cursor_feel() {
    let mut v = common::h79();
    v["schema"] = json!("gmclip.renderplan/1");
    v["images"] = json!([{"id": "arrow", "path": "/tmp/arrow.png", "sha256": "cd".repeat(32), "w": 64, "h": 64}]);
    v["cursors"] = json!([{"id": "cur-1", "source": "mkv1440", "path": "/tmp/input.json",
        "sha256": "ef".repeat(32), "shapes": ["arrow"], "scale": 1.0, "clicks": true}]);
    v["parts"][0]["layers"][0]["cursor"] = json!("cur-1");
    let plain = common::validate(&v).unwrap();
    let c = &plain.cursors.as_ref().unwrap()[0];
    assert_eq!(
        c.click_style(),
        gmclip_plan::types::CursorClick::Ring,
        "from clicks"
    );
    assert_eq!(
        (
            c.smoothness,
            c.click,
            c.click_shrink,
            c.shake_px,
            c.loop_ms,
            c.motion_blur
        ),
        (None, None, None, None, None, None)
    );
    let back = serde_json::to_value(&plain).unwrap();
    assert_eq!(
        back["cursors"][0], v["cursors"][0],
        "no new keys without the feel"
    );

    let mut f = v.clone();
    f["cursors"][0]["smoothness"] = json!(0.6);
    f["cursors"][0]["click"] = json!("ripple");
    f["cursors"][0]["click_shrink"] = json!(true);
    f["cursors"][0]["shake_px"] = json!(4);
    f["cursors"][0]["loop_ms"] = json!(1500);
    f["cursors"][0]["motion_blur"] = json!(true);
    let p = common::validate(&f).unwrap();
    let c = &p.cursors.as_ref().unwrap()[0];
    assert_eq!(c.click_style(), gmclip_plan::types::CursorClick::Ripple);
    assert_eq!(
        (
            c.smoothness,
            c.click_shrink,
            c.shake_px,
            c.loop_ms,
            c.motion_blur
        ),
        (Some(0.6), Some(true), Some(4.0), Some(1500), Some(true))
    );
    assert_eq!(
        common::validate(&serde_json::to_value(&p).unwrap()).unwrap(),
        p
    );

    let bad: [(&Mutation, &str); 5] = [
        (
            &|v| v["cursors"][0]["smoothness"] = json!(1.5),
            "/cursors/0/smoothness",
        ),
        (
            &|v| v["cursors"][0]["click"] = json!("burst"),
            "/cursors/0/click",
        ),
        (
            &|v| v["cursors"][0]["shake_px"] = json!(21),
            "/cursors/0/shake_px",
        ),
        (
            &|v| v["cursors"][0]["loop_ms"] = json!(10_001),
            "/cursors/0/loop_ms",
        ),
        (
            &|v| v["cursors"][0]["motion_blur"] = json!(1),
            "/cursors/0/motion_blur",
        ),
    ];
    for (m, at) in bad {
        let mut w = f.clone();
        m(&mut w);
        let e = common::validate(&w).expect_err(at);
        // a long path echoes cut, marked `…`
        let p = e.path().trim_end_matches('…');
        assert!(
            p.starts_with("/cursors/0/") && at.starts_with(p),
            "{at}: {e}"
        );
    }
}

/// Decision 0046: zoom sections parse and round-trip under `/1` (a cursor, a focus and a
/// point target, camera layers), are refused under `/0`, and every rule is at its pointer.
#[test]
fn zoom_sections() {
    let mut v = common::h79();
    v["schema"] = json!("gmclip.renderplan/1");
    v["images"] = json!([{"id": "arrow", "path": "/tmp/arrow.png", "sha256": "cd".repeat(32), "w": 64, "h": 64}]);
    v["cursors"] = json!([{"id": "cur-1", "source": "mkv1440", "path": "/tmp/input.json",
        "sha256": "ef".repeat(32), "shapes": ["arrow"], "scale": 1.0}]);
    v["parts"][0]["layers"][0]["cursor"] = json!("cur-1");
    v["zooms"] = json!([
        {"id": "zoom-1", "out": [30, 120], "depth": 2.0, "target": {"cursor": "cur-1"},
         "ease_in": 21, "ease_out": 21,
         "layers": [{"part": "01-payoff-banana", "layer": "screen"}, {"part": "02-claim", "layer": "screen"}],
         "camera_scale": 0.5,
         "camera_layers": [{"part": "01-payoff-banana", "layer": "cam"}, {"part": "02-claim", "layer": "cam"}]},
        {"id": "zoom-2", "out": [200, 260], "depth": 3.0, "target": {"focus": "cur-1"},
         "ease_in": 10, "ease_out": 0, "motion_blur": true,
         "layers": [{"part": "03-terrible-prompt", "layer": "screen"}]},
        {"id": "zoom-3", "out": [300, 397], "depth": 1.5, "target": {"point": [0.25, 0.75]},
         "ease_in": 0, "ease_out": 0,
         "layers": [{"part": "03-terrible-prompt", "layer": "detail-0"}]}
    ]);
    let p = common::validate(&v).unwrap();
    let zs = p.zooms.as_ref().unwrap();
    assert_eq!(zs.len(), 3);
    assert_eq!(
        zs[0].target,
        gmclip_plan::types::ZoomTarget::Cursor(gmclip_plan::scalars::Id::new("cur-1").unwrap())
    );
    assert_eq!(
        zs[2].target,
        gmclip_plan::types::ZoomTarget::Point([0.25, 0.75])
    );
    assert_eq!(
        (zs[1].motion_blur, zs[0].camera_scale),
        (Some(true), Some(0.5))
    );
    let back = serde_json::to_value(&p).unwrap();
    assert_eq!(back["zooms"], v["zooms"], "round-trips key for key");
    assert_eq!(common::validate(&back).unwrap(), p);

    let mut v0 = v.clone();
    v0["schema"] = json!("gmclip.renderplan/0");
    for k in ["images", "cursors"] {
        v0.as_object_mut().unwrap().remove(k);
    }
    v0["parts"][0]["layers"][0]
        .as_object_mut()
        .unwrap()
        .remove("cursor");
    v0["zooms"][0]["target"] = json!({"point": [0.5, 0.5]});
    v0["zooms"][1]["target"] = json!({"point": [0.5, 0.5]});
    assert!(common::validate(&v0).is_err(), "zooms under /0");

    let bad: [(&Mutation, &str); 10] = [
        (&|v| v["zooms"][0]["out"] = json!([120, 30]), "/zooms/0/out"),
        (&|v| v["zooms"][0]["out"] = json!([30, 398]), "/zooms/0/out"),
        (&|v| v["zooms"][0]["depth"] = json!(7), "/zooms/0/depth"),
        (
            &|v| v["zooms"][0]["ease_in"] = json!(80),
            "/zooms/0/ease_in",
        ),
        (
            &|v| v["zooms"][0]["target"] = json!({"cursor": "cur-2"}),
            "/zooms/0/target",
        ),
        (
            &|v| v["zooms"][2]["target"] = json!({"point": [1.5, 0.5]}),
            "/zooms/2/target",
        ),
        (
            &|v| v["zooms"][0]["layers"][1]["part"] = json!("03-terrible-prompt"),
            "/zooms/0/layers/1/part",
        ),
        (
            &|v| v["zooms"][0]["layers"][0]["layer"] = json!("detail-0"),
            "/zooms/0/layers/0/layer",
        ),
        (
            &|v| v["zooms"][0]["camera_layers"][0]["part"] = json!("nope"),
            "/zooms/0/camera_layers/0/part",
        ),
        (
            &|v| {
                let z = v["zooms"][0].clone();
                v["zooms"].as_array_mut().unwrap().push(z);
            },
            "/zooms",
        ),
    ];
    for (f, at) in bad {
        let mut w = v.clone();
        f(&mut w);
        let e = common::validate(&w).expect_err(at);
        assert!(e.path().starts_with(at), "{at}: {e}");
    }
}

/// Decision 0046: a layer's frame (rounded corners, a shadow, a border) parses and round
/// trips under `/1`, is refused under `/0`, keeps to its ranges and is never on a cover.
#[test]
fn layer_frames() {
    let frame = json!({"radius": 0.5,
        "shadow": {"blur": 0.05, "offset_y": 0.015, "opacity": 0.45, "rgb": "#000000"},
        "border": {"width": 0.005, "rgb": "#FFFFFF"}});
    for f in [frame.clone(), json!({"radius": 0.03})] {
        let mut v = v1();
        v["parts"][0]["layers"][0]["frame"] = f.clone();
        let p = common::validate(&v).unwrap();
        assert!(p.parts[0].layers[0].frame.is_some());
        let back = serde_json::to_value(&p).unwrap();
        assert_eq!(back["parts"][0]["layers"][0]["frame"], f);
        assert_eq!(common::validate(&back).unwrap(), p);
        let mut v0 = common::h79();
        v0["parts"][0]["layers"][0]["frame"] = f;
        let e = common::validate(&v0).expect_err("frame under /0");
        assert!(e.path().starts_with("/parts/0/layers/0"), "{e}");
    }
    for bad in [
        json!({"radius": 0.6}),
        json!({"radius": -0.1}),
        json!({}),
        json!({"radius": 0.1, "shadow": {"blur": 0.05, "offset_y": 0, "opacity": 1.5, "rgb": "#000000"}}),
        json!({"radius": 0.1, "shadow": {"blur": 0.05, "offset_y": 0, "opacity": 0.5}}),
        json!({"radius": 0.1, "border": {"width": 0.2, "rgb": "#FFFFFF"}}),
        json!({"radius": 0.1, "border": {"width": 0.01, "rgb": "white"}}),
        json!({"radius": 0.1, "glow": true}),
    ] {
        let mut v = v1();
        v["parts"][0]["layers"][0]["frame"] = bad.clone();
        assert!(common::validate(&v).is_err(), "{bad}");
    }
    let mut v = v1();
    v["parts"][0]["layers"][0]["cover"] = json!({"kind": "blur", "radius": 8});
    v["parts"][0]["layers"][0]["frame"] = json!({"radius": 0.1});
    let e = common::validate(&v).unwrap_err();
    assert_eq!(e.path(), "/parts/0/layers/0/frame", "{e}");
}

/// Decision 0047: a layer of another source at another rate (a 25 fps camera beside a
/// 30 fps screen) is read by time with `fps`; without it the source is `Unsupported`, and
/// `fps` equal to the output's, unlike the source's, or on the part's own source is
/// `Invalid`. Nominally, 25 in 30 repeats one frame in six.
#[test]
fn another_source_at_another_rate() {
    let with = |v: &mut Value, fps: Option<Value>| {
        v["sources"].as_array_mut().unwrap().push(
            json!({"id": "face", "path": "face.mov", "expect": {"video": {"fps": {"num": 25, "den": 1}}}}),
        );
        let l = &mut v["parts"][0]["layers"][1];
        l["picture_source"] = json!("face");
        l["first_frame"] = json!(30);
        if let Some(f) = fps {
            l["fps"] = f;
        }
    };
    let mut v = v1();
    with(&mut v, Some(json!({"num": 25, "den": 1})));
    let p = common::validate(&v).unwrap();
    let l = &p.parts[0].layers[1];
    assert_eq!(l.fps.map(|r| (r.num, r.den)), Some((25, 1)));
    let back = serde_json::to_value(&p).unwrap();
    assert_eq!(common::validate(&back).unwrap(), p);
    let out = p.output.fps;
    let picks: Vec<u64> = (0..7)
        .map(|k| gmclip_plan::retime::layer_frame(l, k, out, None).unwrap())
        .collect();
    // (30 + k) / 30 s at 25 fps
    assert_eq!(picks, [25, 25, 26, 27, 28, 29, 30]);

    let mut w = v1();
    with(&mut w, None);
    let e = common::validate(&w).expect_err("no fps");
    let face = w["sources"].as_array().unwrap().len() - 1;
    assert_eq!(e.code(), "Unsupported", "{e}");
    assert_eq!(e.path(), format!("/sources/{face}/expect/video/fps"), "{e}");
    for (bad, why) in [
        (json!({"num": 30, "den": 1}), "equals output.fps"),
        (json!({"num": 24, "den": 1}), "differs from the source"),
    ] {
        let mut w = v1();
        with(&mut w, Some(bad));
        let e = common::validate(&w).expect_err(why);
        assert_eq!(
            (e.code(), e.path()),
            ("Invalid", "/parts/0/layers/1/fps"),
            "{e}"
        );
    }
    let mut w = v.clone();
    w["parts"][0]["layers"][1]["picture_source"] = w["parts"][0]["picture"]["source"].clone();
    w["parts"][0]["layers"][1]
        .as_object_mut()
        .unwrap()
        .remove("first_frame");
    let e = common::validate(&w).expect_err("own source");
    assert_eq!(e.path(), "/parts/0/layers/1/fps", "{e}");
}

/// Decision 0046: a layer of another source read live (`first_frame` with a
/// `picture_source`): accepted under `/1`, refused under `/0`, without another source,
/// with a hold, or on a sped part.
#[test]
fn another_source_read_live() {
    let with = |v: &mut Value| {
        v["sources"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "face", "path": "face.mov"}));
        let l = &mut v["parts"][0]["layers"][1];
        l["picture_source"] = json!("face");
        l["first_frame"] = json!(120);
    };
    let mut v = v1();
    with(&mut v);
    let p = common::validate(&v).unwrap();
    assert_eq!(p.parts[0].layers[1].first_frame, Some(120));
    let back = serde_json::to_value(&p).unwrap();
    assert_eq!(common::validate(&back).unwrap(), p);
    let mut v0 = common::h79();
    with(&mut v0);
    assert!(common::validate(&v0).is_err());
    let rows: [&Mutation; 3] = [
        &|v| {
            v["parts"][0]["layers"][1]
                .as_object_mut()
                .unwrap()
                .remove("picture_source");
        },
        &|v| v["parts"][0]["layers"][1]["time_map"] = json!({"mode": "hold", "frame": 3}),
        &|v| v["parts"][0]["speed"] = json!({"num": 2, "den": 1}),
    ];
    for f in rows {
        let mut w = v.clone();
        f(&mut w);
        let e = common::validate(&w).expect_err("refused");
        assert!(e.path().starts_with("/parts/0"), "{e}");
    }
}

/// Decision 0051: a part hears other sources under its own audio, and music or a sound
/// effect plays over the cut, ducked under the speech ranges; all three are `/1` only and
/// every reference, range and order is checked.
#[test]
fn sound_under_overlays_and_speech() {
    let mut v = common::h79();
    v["schema"] = json!("gmclip.renderplan/1");
    v["sources"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "screen", "path": "/tmp/screen.mov"}));
    v["parts"][0]["audio"]["under"] =
        json!([{"source": "screen", "first_sample": 1600, "gain_db": -12}]);
    v["audio"]["overlays"] = json!([{"id": "music", "source": "screen", "first_sample": 0,
        "out_samples": [0, 48000], "gain_db": -18, "fade_in_samples": 4800, "fade_out_samples": 9600,
        "duck": {"depth_db": -12, "attack_samples": 4800, "release_samples": 14400}}]);
    v["audio"]["speech"] = json!([[1600, 20000], [24000, 30000]]);
    let p = common::validate(&v).unwrap();
    assert_eq!(p.parts[0].audio.under.as_ref().unwrap()[0].gain_db, -12.0);
    assert_eq!(
        p.audio.overlays.as_ref().unwrap()[0].duck.unwrap().depth_db,
        -12.0
    );
    let back = serde_json::to_value(&p).unwrap();
    assert_eq!(common::validate(&back).unwrap(), p);
    for (key, at) in [
        ("under", "/parts/0/audio/under"),
        ("overlays", "/audio/overlays"),
        ("speech", "/audio/speech"),
    ] {
        let mut v0 = v.clone();
        v0["schema"] = json!("gmclip.renderplan/0");
        for k in ["under", "overlays", "speech"] {
            if k == key {
                continue;
            }
            if k == "under" {
                v0["parts"][0]["audio"].as_object_mut().unwrap().remove(k);
            } else {
                v0["audio"].as_object_mut().unwrap().remove(k);
            }
        }
        let e = common::validate(&v0).unwrap_err();
        assert!(e.path().starts_with(at), "{key}: {e}");
    }
    let bad: [(&Mutation, &str); 9] = [
        (
            &|v| v["parts"][0]["audio"]["under"][0]["source"] = json!("nope"),
            "/parts/0/audio/under/0/source",
        ),
        (
            &|v| v["parts"][0]["audio"]["under"][0]["gain_db"] = json!(-61),
            "/parts/0/audio/under/0/gain_db",
        ),
        (
            &|v| v["parts"][0]["speed"] = json!({"num": 2, "den": 1}),
            "/parts/0/audio/under/0",
        ),
        (
            &|v| v["parts"][0]["audio"]["under"] = json!([]),
            "/parts/0/audio/under",
        ),
        (
            &|v| v["audio"]["overlays"][0]["source"] = json!("nope"),
            "/audio/overlays/0/source",
        ),
        (
            &|v| v["audio"]["overlays"][0]["fade_out_samples"] = json!(48000),
            "/audio/overlays/0/fade_in_samples",
        ),
        (
            &|v| v["audio"]["overlays"][0]["duck"]["depth_db"] = json!(3),
            "/audio/overlays/0/duck",
        ),
        (
            &|v| v["audio"]["speech"] = json!([[24000, 30000], [1600, 20000]]),
            "/audio/speech/1",
        ),
        (
            &|v| v["audio"]["speech"] = json!([[1600, 20000], [19000, 30000]]),
            "/audio/speech/1",
        ),
    ];
    for (f, at) in bad {
        let mut w = v.clone();
        f(&mut w);
        let e = common::validate(&w).expect_err(at);
        assert!(e.path().starts_with(at), "{at}: {e}");
    }
}
