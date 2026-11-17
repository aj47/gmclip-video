//! The model planner (A18) on hand-built documents: sync offsets per SPEC §4.3
//! (member = group − offset), part-spanning holds, fades, part-edge anchors, and the
//! refusals for what the M1 planner cannot say.

use std::collections::BTreeMap;

use gmclip_core::planner::{self, PlanHost, PlannerError};
use gmclip_model::anchor::{Anchor, FrameRef, PartEdge};
use gmclip_model::entities::*;
use gmclip_model::fixtures;
use gmclip_model::geometry::{NormRect, Place};
use gmclip_model::time::{FLICKS_PER_SECOND, Flicks, Rational};
use gmclip_model::{Doc, Id, OrderKey, Ulid};

const FPF: i64 = FLICKS_PER_SECOND / 30;

struct NoFonts;
impl PlanHost for NoFonts {
    fn font_path(&self, _: &Font) -> Option<String> {
        None
    }
}

fn id<E>(n: u128) -> Id<E> {
    Id::new(Ulid(n))
}

struct Fx {
    doc: Doc,
    cut: Id<Cut>,
    framing: Id<Framing>,
    parts: Vec<Id<Part>>,
    pic: Id<Source>,
}

/// A cut of `ranges` (native frames) on a two-member group: picture offset `off`
/// frames (SPEC: member time = group time − offset), audio at 0.
fn fx(ranges: &[(i64, i64)], off_frames: i64) -> Fx {
    let mut d = Doc::new();
    let cut = fixtures::cut(1);
    let cut_id = cut.id;
    d.insert_for_test(cut);
    let mut framing = fixtures::framing(2, cut_id);
    framing.loudness_mode = Some(LoudnessMode::AsRecorded);
    let framing_id = framing.id;
    d.insert_for_test(framing);
    let mut pic = fixtures::source(10);
    pic.fingerprint = planner::fingerprint(Some(&"a".repeat(64)), Some(5));
    pic.probe.tracks.push(TrackProbe {
        index: 0,
        kind: TrackKind::Video,
        codec: "vp9".into(),
        width: Some(2560),
        height: Some(1440),
        fps: Some(Rational::new(30, 1)),
        timescale: 0,
        gop_max_s: None,
    });
    let mut aud = fixtures::source(11);
    aud.probe.audio_tracks.push(AudioTrackProbe {
        index: 0,
        codec: "aac".into(),
        rate: 48_000,
        layout: "stereo".into(),
        bitrate: None,
        speech_ratio: None,
    });
    let (pic_id, aud_id) = (pic.id, aud.id);
    d.insert_for_test(pic);
    d.insert_for_test(aud);
    let g: Id<SyncGroup> = id(20);
    let pm: Id<SyncMember> = id(21);
    let am: Id<SyncMember> = id(22);
    d.insert_for_test(SyncGroup {
        id: g,
        master_audio: Some(am),
        picture: Some(pm),
    });
    for (m, src, role, n) in [
        (pm, pic_id, MemberRole::Picture, 0u128),
        (am, aud_id, MemberRole::Audio, 1),
    ] {
        d.insert_for_test(SyncMember {
            id: m,
            group: g,
            order: OrderKey::sequence(None, 2).unwrap()[n as usize].clone(),
            source: src,
            role,
            rate_ppm: 0.0,
            method: SyncMethod::Import,
            confidence: 1.0,
            under_db: None,
        });
        d.insert_for_test(SyncSegment {
            id: id(30 + n),
            member: m,
            order: OrderKey::first(),
            src_range: SrcRange {
                start: Flicks(0),
                end: Flicks(i64::MAX / 4),
            },
            offset: Flicks(if m == pm { -off_frames * FPF } else { 0 }),
        });
    }
    let layout: Id<Layout> = id(40);
    d.insert_for_test(fixtures::layout(40, cut_id));
    let mut screen = fixtures::layer(
        41,
        LayerOwner::Layout(layout),
        OrderKey::first(),
        LayerRole::Region(RegionRole::Screen),
    );
    screen.member = Some(pm);
    screen.crop = NormRect {
        x: 0.25,
        y: 0.25,
        w: 0.5,
        h: 0.5,
    };
    screen.place = Place {
        x: 0.1,
        y: 0.2,
        w: 0.8,
    };
    d.insert_for_test(screen);
    let keys = OrderKey::sequence(None, ranges.len()).unwrap();
    let mut parts = Vec::new();
    for (i, (a, b)) in ranges.iter().enumerate() {
        let mut p = fixtures::part(100 + i as u128, cut_id, keys[i].clone(), a * FPF, b * FPF);
        p.media = MediaRef::Group(g);
        p.look.insert(
            framing_id,
            PartLook {
                layout_ref: Some(layout),
                overrides: BTreeMap::new(),
            },
        );
        parts.push(p.id);
        d.insert_for_test(p);
    }
    Fx {
        doc: d,
        cut: cut_id,
        framing: framing_id,
        parts,
        pic: pic_id,
    }
}

fn plan(f: &Fx) -> Result<gmclip_plan::Plan, PlannerError> {
    planner::plan_cut(&f.doc, f.cut, f.framing, &NoFonts)
}

#[test]
fn offsets_frames_samples_fades_and_layers() {
    // RT's MKV frame = native − 1642: the SyncSegment offset is +1642 frames
    let f = fx(
        &[(81_254, 81_309), (81_309, 81_330), (90_000, 90_030)],
        -1642,
    );
    let p = plan(&f).unwrap();
    assert_eq!(p.parts[0].picture.first_frame, 81_254 - 1642);
    assert_eq!(p.parts[0].audio.first_sample, 81_254 * 1600);
    assert_eq!(p.parts[2].out.0, [76, 106]);
    assert_eq!(p.output.frames, 106);
    // fades at the cut's edges and the real join only
    let fades: Vec<(bool, bool)> = p
        .parts
        .iter()
        .map(|x| (x.audio.fade_in().is_some(), x.audio.fade_out().is_some()))
        .collect();
    assert_eq!(fades, [(true, false), (false, true), (true, true)]);
    let l = &p.parts[0].layers[0];
    assert_eq!((l.id.as_str(), l.role.as_str()), ("screen", "screen"));
    assert_eq!(
        (l.crop.x, l.crop.w, l.place.x, l.place.w),
        (640.0, 1280.0, 108.0, 864.0)
    );
    // the place's height follows the crop's aspect (720/1280 of 864 px)
    assert!((l.place.h - 486.0).abs() < 1e-9);
    assert_eq!(p.sources.len(), 2);
    assert_eq!(
        p.sources[0]
            .expect
            .as_ref()
            .unwrap()
            .video
            .as_ref()
            .unwrap()
            .fps
            .as_ref()
            .unwrap()
            .num,
        30
    );
    assert!(p.sources[1].expect.as_ref().unwrap().video.is_none());
}

#[test]
fn a_part_spanning_hold_and_part_edge_overlays() {
    let mut f = fx(&[(300, 330), (330, 360)], 0);
    let p1 = f.parts[1];
    f.doc.insert_for_test(ContentHold {
        id: id(50),
        cut: f.cut,
        from: Anchor::PartStart(PartEdge {
            part: p1,
            offset_ms: None,
        }),
        to: Anchor::PartEnd(PartEdge {
            part: p1,
            offset_ms: None,
        }),
        at: HoldAt::Frame(FrameRef {
            source: f.pic,
            frame_index: 345,
        }),
        roles: vec![LayerRole::Region(RegionRole::Screen), LayerRole::Detail],
        per_framing: BTreeMap::new(),
    });
    let p = plan(&f).unwrap();
    assert_eq!(
        p.parts[0].layers[0].time_map,
        Some(gmclip_plan::types::TimeMap::Live)
    );
    assert!(matches!(
        p.parts[1].layers[0].time_map,
        Some(gmclip_plan::types::TimeMap::Hold { frame: 345, .. })
    ));
    // a hold that starts inside a part is refused, not applied to the whole part
    f.doc.insert_for_test(ContentHold {
        id: id(51),
        cut: f.cut,
        from: Anchor::Raw(gmclip_model::anchor::RawTime {
            time: gmclip_model::clock::ModelTime::Src {
                source: None,
                group: Some(id(20)),
                t: Flicks(310 * FPF),
            },
            reason: "t".into(),
        }),
        to: Anchor::PartEnd(PartEdge {
            part: f.parts[0],
            offset_ms: None,
        }),
        at: HoldAt::Frame(FrameRef {
            source: f.pic,
            frame_index: 310,
        }),
        roles: vec![],
        per_framing: BTreeMap::new(),
    });
    assert!(plan(&f).unwrap_err().0.contains("inside a part"));
}

#[test]
fn what_the_m1_planner_cannot_say_is_refused() {
    let base = || fx(&[(300, 330)], 0);
    let refuse = |f: &Fx, why: &str| {
        let e = plan(f).unwrap_err();
        assert!(e.0.contains(why), "{e}");
    };
    // decision 0030: ¼× to 4× plans (`parts[].speed`); past that is refused
    let mut f = base();
    f.doc.insert_for_test(Part {
        speed: 2.0,
        ..f.doc.get(f.parts[0]).unwrap().clone()
    });
    assert_eq!(
        plan(&f).unwrap().parts[0].speed.map(|s| (s.num, s.den)),
        Some((2, 1))
    );
    f.doc.insert_for_test(Part {
        speed: 8.0,
        ..f.doc.get(f.parts[0]).unwrap().clone()
    });
    refuse(&f, "speed");
    let mut f = base();
    f.doc.insert_for_test(Part {
        src_in: Flicks(300 * FPF + 1),
        ..f.doc.get(f.parts[0]).unwrap().clone()
    });
    // word / agent times between frames snap to the earlier (SPEC §4.1.1) and plan
    assert_eq!(plan(&f).unwrap().parts[0].picture.first_frame, 300);
    let mut f = base();
    f.doc.insert_for_test(SyncMember {
        rate_ppm: 3.0,
        ..f.doc.get(id::<SyncMember>(21)).unwrap().clone()
    });
    refuse(&f, "rate correction");
    let mut f = base();
    f.doc.insert_for_test(ContentHold {
        id: id(61),
        cut: f.cut,
        from: Anchor::PartStart(PartEdge {
            part: f.parts[0],
            offset_ms: None,
        }),
        to: Anchor::PartEnd(PartEdge {
            part: f.parts[0],
            offset_ms: None,
        }),
        at: HoldAt::AtSrc(Flicks(310 * FPF)),
        roles: vec![],
        per_framing: BTreeMap::new(),
    });
    refuse(&f, "at_src");
    let mut f = base();
    let cd = CaptionDoc {
        source: CaptionSource::Heard,
        ..fixtures::caption_doc(70, f.cut)
    };
    f.doc.insert_for_test(cd);
    f.doc.insert_for_test(Cue {
        id: id(71),
        caption_doc: id(70),
        order: OrderKey::first(),
        from: Anchor::CutStart(gmclip_model::anchor::CutEdge {
            cut: f.cut,
            offset_ms: None,
        }),
        to: Anchor::CutEnd(gmclip_model::anchor::CutEdge {
            cut: f.cut,
            offset_ms: None,
        }),
        runs: vec![],
        style_ref: None,
        provenance: CueProvenance::Import,
    });
    refuse(&f, "transcripts");
    let mut f = base();
    f.doc.insert_for_test(Framing {
        fps: Rational::new(120, 1),
        ..f.doc.get(f.framing).unwrap().clone()
    });
    refuse(&f, "not one gmclip exports");
}

/// R.03: a cut visual split inside a part (the activity crop's split at the move)
/// becomes its own plan part with the split's crop, joined continuously to the first.
/// R.01: a roto matte reaches the plan with soften and strokes in source pixels.
#[test]
fn cut_visual_splits_and_roto_mattes_reach_the_plan() {
    let mut f = fx(&[(300, 360)], 0);
    let screen: Id<Layer> = id(41);
    let mut ov = BTreeMap::new();
    ov.insert(
        screen,
        LayerPatch {
            crop: Some(NormRect {
                x: 0.5,
                y: 0.0,
                w: 0.5,
                h: 0.5,
            }),
            ..LayerPatch::default()
        },
    );
    f.doc.insert_for_test(VisualSplit {
        id: id(60),
        part: f.parts[0],
        framing: f.framing,
        at_src: Flicks(320 * FPF),
        overrides: ov,
        transition: Transition::Cut,
        tag: None,
    });
    let mut l = f.doc.get(screen).unwrap().clone();
    l.matte = Matte::Roto {
        model: "apple_vision".into(),
        soften: 3.0,
        strokes: vec![MatteStroke {
            mode: StrokeMode::Subtract,
            radius: 0.01,
            feather: 0.5,
            points: vec![[0.5, 0.5], [0.6, 0.5]],
            at_src: None,
        }],
    };
    f.doc.insert_for_test(l);
    let p = plan(&f).unwrap();
    assert_eq!(p.parts.len(), 2);
    assert_eq!(p.parts[0].out.0, [0, 20]);
    assert_eq!(p.parts[1].out.0, [20, 60]);
    assert_eq!(
        p.parts[1].picture.first_frame,
        p.parts[0].picture.first_frame + 20
    );
    assert_eq!(
        p.parts[1].audio.first_sample,
        p.parts[0].audio.first_sample + 20 * 1600
    );
    // continuous join: no fade between the two
    assert_eq!(p.parts[0].audio.fade_out, Some(None));
    assert_eq!(p.parts[1].audio.fade_in, Some(None));
    let c0 = p.parts[0].layers[0].crop;
    let c1 = p.parts[1].layers[0].crop;
    assert_eq!((c0.x, c0.w), (640.0, 1280.0));
    assert_eq!((c1.x, c1.y, c1.w), (1280.0, 0.0, 1280.0));
    let m = p.parts[0].layers[0].matte.as_ref().unwrap();
    assert_eq!(m.model, "apple_vision");
    assert_eq!(m.soften, Some(3.0));
    let s = &m.strokes.as_ref().unwrap()[0];
    assert_eq!(s.mode, gmclip_plan::types::StrokeMode::Subtract);
    assert!((s.radius - 14.4).abs() < 1e-9);
    assert_eq!((s.points[1].x, s.points[1].y), (1536.0, 720.0));
    assert_eq!(
        planner::plan_part_owners(&f.doc, f.cut, f.framing),
        vec![f.parts[0], f.parts[0]]
    );
    // the plan validates
    let bytes = serde_json::to_vec(&p).unwrap();
    gmclip_plan::parse(&bytes).unwrap();
}

/// Decision 0016: an eased visual split is its own plan part whose layers carry the
/// settled values and `ease {from_*, frames, curve}`; the plan is `/1`; frame by frame
/// the plan's interpolation (what the compositor draws) equals the model's `effective()`.
#[test]
fn an_eased_split_plans_as_an_ease_matching_effective() {
    let mut f = fx(&[(300, 360)], 0);
    let to = NormRect {
        x: 0.4,
        y: 0.3,
        w: 0.25,
        h: 0.25,
    };
    let mut ov = BTreeMap::new();
    ov.insert(
        id::<Layer>(41),
        LayerPatch {
            crop: Some(to),
            ..LayerPatch::default()
        },
    );
    f.doc.insert_for_test(VisualSplit {
        id: id(60),
        part: f.parts[0],
        framing: f.framing,
        at_src: Flicks(320 * FPF),
        overrides: ov,
        transition: Transition::Ease {
            dur_ms: 400,
            curve: Curve::EaseInOut,
        },
        tag: None,
    });
    let p = plan(&f).unwrap();
    assert_eq!(p.schema, gmclip_plan::consts::SchemaVersion::V1);
    assert_eq!(p.parts.len(), 2);
    assert!(p.parts[0].layers[0].ease.is_none());
    let l = &p.parts[1].layers[0];
    let e = l.ease.as_ref().expect("the eased part carries an ease");
    assert_eq!(e.frames, 12);
    assert_eq!(e.curve, gmclip_plan::types::EaseCurve::EaseInOut);
    // from: the layout's crop (0.25 of 2560 × 1440); own: the split's
    assert!((e.from_crop.x - 640.0).abs() < 1e-6 && (e.from_crop.w - 1280.0).abs() < 1e-6);
    assert!((l.crop.x - 0.4 * 2560.0).abs() < 1e-6 && (l.crop.w - 640.0).abs() < 1e-6);
    // every frame of the plan part: the compositor's crop equals effective()'s
    for k in 0..40u64 {
        let st = gmclip_model::effective::effective(
            &f.doc,
            f.parts[0],
            f.framing,
            Flicks((320 + k as i64) * FPF),
        )
        .unwrap();
        let c = st.layers[0].values.crop;
        // (the compositor's `frame_draw` draws `ease.at(offset, …)`)
        let got = e.at(k, l.crop, l.place, 1.0).0;
        assert!(
            (got.x - c.x * 2560.0).abs() < 1e-6 && (got.w - c.w * 2560.0).abs() < 1e-6,
            "frame {k}: {got:?} vs {c:?}"
        );
    }
    // the plan validates under /1 and round-trips
    let json = serde_json::to_value(&p).unwrap();
    assert_eq!(
        json["parts"][1]["layers"][0]["ease"]["curve"],
        "ease_in_out"
    );
}

/// SPEC §6.5.1: the YouTube upload presets plan a 16:9 framing on their own canvas (the
/// layout scales: places are fractions of the canvas) with their own encode block; a
/// framing that is not 16:9 is refused.
#[test]
fn youtube_presets_plan_the_16x9_framing_on_their_canvas() {
    use planner::ExportPreset;
    let mut f = fx(&[(0, 90), (200, 260)], 0);
    // 9:16: refused
    assert!(
        planner::plan_cut_preset(
            &f.doc,
            f.cut,
            f.framing,
            &NoFonts,
            None,
            ExportPreset::Youtube1440
        )
        .is_err()
    );
    let mut fr = f.doc.get(f.framing).unwrap().clone();
    fr.canvas = Canvas { w: 1920, h: 1080 };
    fr.aspect_class = AspectClass::Landscape;
    f.doc.insert_for_test(fr);
    let base = planner::plan_cut(&f.doc, f.cut, f.framing, &NoFonts).unwrap();
    let (yt, _) = planner::plan_cut_preset(
        &f.doc,
        f.cut,
        f.framing,
        &NoFonts,
        None,
        ExportPreset::Youtube1440,
    )
    .unwrap();
    assert_eq!(
        (yt.output.canvas.w.get(), yt.output.canvas.h.get()),
        (2560, 1440)
    );
    assert_eq!(yt.output.video.level.as_deref(), Some("5.1"));
    assert_eq!(yt.output.video.gop_frames, Some(15));
    let r = yt.output.video.rate.as_ref().unwrap();
    // decision 0027: no 25 Mbit/s cap on an upload preset; the peak runs to 1.5 × the
    // target, so the plan is written /1
    assert_eq!(
        (r.target_bps, r.max_bps),
        (Some(24_000_000), Some(36_000_000))
    );
    assert_eq!(
        serde_json::to_value(&yt).unwrap()["schema"],
        "gmclip.renderplan/1"
    );
    assert_eq!(yt.output.frames, base.output.frames);
    let (a, b) = (&base.parts[0].layers[0].place, &yt.parts[0].layers[0].place);
    for (x, y) in [(a.x, b.x), (a.y, b.y), (a.w, b.w), (a.h, b.h)] {
        assert!((x * 4.0 / 3.0 - y).abs() < 1e-6, "{x} → {y}");
    }
    assert_eq!(base.parts[0].layers[0].crop, yt.parts[0].layers[0].crop);
    let (hd, _) = planner::plan_cut_preset(
        &f.doc,
        f.cut,
        f.framing,
        &NoFonts,
        None,
        ExportPreset::Youtube1080,
    )
    .unwrap();
    assert_eq!(
        (hd.output.canvas.w.get(), hd.output.canvas.h.get()),
        (1920, 1080)
    );
    assert_eq!(hd.output.video.level.as_deref(), Some("4.0"));
    let r = hd.output.video.rate.as_ref().unwrap();
    assert_eq!(
        (r.target_bps, r.max_bps),
        (Some(12_000_000), Some(18_000_000))
    );
    // under the social ceiling the plan stays /0, byte for byte as before
    assert_eq!(
        serde_json::to_value(&hd).unwrap()["schema"],
        "gmclip.renderplan/0"
    );
    assert!(gmclip_plan::check_plan(&yt).is_empty() && gmclip_plan::check_plan(&hd).is_empty());
}

/// Decision 0027: YouTube 1440p60 plans its full 36 Mbit/s (peak 54) at High 5.1, and
/// 1080p60 its 18 (peak 27) at 4.2: both past the old 25 Mbit/s plan cap, both valid.
#[test]
fn youtube_presets_at_60_fps_plan_their_full_rate() {
    use planner::ExportPreset;
    let mut f = fx(&[(0, 90), (200, 260)], 0);
    let mut fr = f.doc.get(f.framing).unwrap().clone();
    fr.canvas = Canvas { w: 1920, h: 1080 };
    fr.aspect_class = AspectClass::Landscape;
    f.doc.insert_for_test(fr);
    set_rate(&mut f, Rational::new(60, 1), 60);
    for (preset, level, target, max) in [
        (ExportPreset::Youtube1440, "5.1", 36_000_000, 54_000_000),
        (ExportPreset::Youtube1080, "4.2", 18_000_000, 27_000_000),
    ] {
        let (p, _) =
            planner::plan_cut_preset(&f.doc, f.cut, f.framing, &NoFonts, None, preset).unwrap();
        assert_eq!(p.output.fps.num.get(), 60);
        assert_eq!(p.output.video.level.as_deref(), Some(level));
        assert_eq!(p.output.video.gop_frames, Some(30));
        let r = p.output.video.rate.as_ref().unwrap();
        assert_eq!((r.target_bps, r.max_bps), (Some(target), Some(max)));
        assert_eq!(
            serde_json::to_value(&p).unwrap()["schema"],
            "gmclip.renderplan/1"
        );
        assert!(
            gmclip_plan::check_plan(&p).is_empty(),
            "{:?}",
            gmclip_plan::check_plan(&p)
        );
    }
}

/// Decision 0017: a picture source at another nominal rate than the framing plans as a
/// rate conversion (`picture.fps`, `/1`); `first_frame` counts output frames on the
/// source's clock and the audio is planned exactly as at the same rate.
#[test]
fn a_picture_at_another_rate_plans_as_a_rate_conversion() {
    let rates = [
        (30000, 1001),
        (24000, 1001),
        (25, 1),
        (60, 1),
        (60000, 1001),
        (30, 1),
    ];
    for out in [30i64, 60] {
        for (num, den) in rates {
            let mut f = fx(&[(300, 330), (400, 460)], 0);
            set_rate(&mut f, Rational::new(num, den), out);
            let p = plan(&f).unwrap_or_else(|e| panic!("{num}/{den} into {out}: {e}"));
            let same = num == out * i64::from(den);
            let k = out / 30; // fixture ranges are 30 fps frames
            for (pp, (a, b)) in p.parts.iter().zip([(300, 330), (400, 460)]) {
                assert_eq!(pp.picture.first_frame, (a * k) as u64);
                assert_eq!(pp.out.len(), ((b - a) * k) as u64);
                assert_eq!(
                    pp.audio.first_sample,
                    a as u64 * 1600,
                    "audio at the same time"
                );
                let want = (!same).then_some(gmclip_plan::types::Rational {
                    num: num as u64,
                    den: u64::from(den),
                });
                assert_eq!(pp.picture.fps, want, "{num}/{den} into {out}");
            }
            let v = if same {
                "gmclip.renderplan/0"
            } else {
                "gmclip.renderplan/1"
            };
            assert_eq!(serde_json::to_value(&p).unwrap()["schema"], v);
            assert!(gmclip_plan::check_plan(&p).is_empty());
            assert_eq!(p.output.fps.num.get(), out as u64);
        }
    }
}

/// AJ's 2026-10-15 report: a 29.97 fps file in a 30 fps cut no longer fails planning
/// with "unsupported at /sources/0/expect/video/fps".
#[test]
fn a_29_97_file_in_a_30_fps_cut_plans() {
    let mut f = fx(&[(0, 90)], 0);
    set_rate(&mut f, Rational::new(30000, 1001), 30);
    let p = plan(&f).unwrap();
    // a thousand-and-one output frames show a thousand source frames
    let pp = &p.parts[0];
    let picks: Vec<u64> = (0..pp.out.len())
        .map(|k| gmclip_plan::retime::nominal_frame(pp, k, p.output.fps))
        .collect();
    assert_eq!(picks[..4], [0, 0, 1, 2]);
    assert_eq!(*picks.last().unwrap(), 88);
}

/// Mixed rates in one cut: each part converts (or not) on its own source's rate.
#[test]
fn a_mixed_rate_cut_converts_each_part_on_its_own_source() {
    let mut f = fx(&[(300, 330)], 0);
    set_rate(&mut f, Rational::new(60, 1), 30);
    // a second part straight on a 23.976 source with its own sound
    let mut s2 = fixtures::source(12);
    s2.fingerprint = planner::fingerprint(Some(&"b".repeat(64)), Some(6));
    s2.probe.tracks.push(TrackProbe {
        index: 0,
        kind: TrackKind::Video,
        codec: "h264".into(),
        width: Some(1920),
        height: Some(1080),
        fps: Some(Rational::new(24000, 1001)),
        timescale: 24000,
        gop_max_s: None,
    });
    s2.probe.audio_tracks.push(AudioTrackProbe {
        index: 1,
        codec: "aac".into(),
        rate: 48_000,
        layout: "stereo".into(),
        bitrate: None,
        speech_ratio: None,
    });
    let s2_id = s2.id;
    f.doc.insert_for_test(s2);
    let first = f.doc.get(f.parts[0]).unwrap().clone();
    let mut p2 = fixtures::part(
        150,
        f.cut,
        OrderKey::sequence(Some(&first.order), 1).unwrap()[0].clone(),
        30 * FPF,
        60 * FPF,
    );
    p2.media = MediaRef::Source(s2_id);
    // its own layout: a full-frame screen with no sync member
    let layout2: Id<Layout> = id(60);
    f.doc.insert_for_test(fixtures::layout(60, f.cut));
    f.doc.insert_for_test(fixtures::layer(
        61,
        LayerOwner::Layout(layout2),
        OrderKey::first(),
        LayerRole::Region(RegionRole::Screen),
    ));
    p2.look.insert(
        f.framing,
        PartLook {
            layout_ref: Some(layout2),
            overrides: BTreeMap::new(),
        },
    );
    f.doc.insert_for_test(p2);
    let p = plan(&f).unwrap();
    assert_eq!(p.parts.len(), 2);
    let fps: Vec<_> = p
        .parts
        .iter()
        .map(|x| x.picture.fps.map(|r| (r.num, r.den)))
        .collect();
    assert_eq!(fps, [Some((60, 1)), Some((24000, 1001))]);
    assert_eq!(p.parts[1].picture.first_frame, 30);
    assert_eq!(p.parts[1].audio.first_sample, 30 * 1600);
    assert!(gmclip_plan::check_plan(&p).is_empty());
}

/// Sets the fixture's picture source rate and the framing's (integer) rate.
fn set_rate(f: &mut Fx, r: Rational, out: i64) {
    let mut s = f.doc.get(f.pic).unwrap().clone();
    s.probe.tracks[0].fps = Some(r);
    f.doc.insert_for_test(s);
    let mut fr = f.doc.get(f.framing).unwrap().clone();
    fr.fps = Rational::new(out, 1);
    f.doc.insert_for_test(fr);
}

/// A plan that does not validate is refused in words, not a JSON pointer.
#[test]
fn a_refusal_says_what_about_the_video_does_not_fit() {
    let mut f = fx(&[(0, 30)], 0);
    let aud = f
        .doc
        .iter::<Source>()
        .find(|s| s.id != f.pic)
        .unwrap()
        .clone();
    let mut a = aud;
    a.probe.audio_tracks[0].rate = 44_100;
    f.doc.insert_for_test(a);
    // decision 0051: sound at another rate is read converted to 48 kHz, which is `/1`
    let p = plan(&f).unwrap();
    assert_eq!(p.schema, gmclip_plan::consts::SchemaVersion::V1);
    let mut p0 = p.clone();
    p0.schema = gmclip_plan::consts::SchemaVersion::V0;
    let e = gmclip_plan::check_plan(&p0);
    assert!(e[0].path().ends_with("/expect/audio/rate"), "{:?}", e);
    let mut f = fx(&[(0, 30)], 0);
    let mut fr = f.doc.get(f.framing).unwrap().clone();
    fr.fps = Rational::new(2997, 100);
    f.doc.insert_for_test(fr);
    let e = plan(&f).unwrap_err().0;
    assert!(
        e.contains("frame rate (29.97 fps) is not one gmclip exports"),
        "{e}"
    );
    assert_eq!(planner::fps_words(24000, 1001), "23.976");
    assert_eq!(planner::fps_words(60, 1), "60");
}

/// A cut made from a video takes a rate RenderPlan exports: NTSC exactly (decision
/// 0020), anything else converted into an integer rate (decision 0017).
#[test]
fn a_video_makes_a_cut_at_an_exported_rate() {
    let r = |n, d| {
        let x = planner::output_rate_for(n, d);
        (x.num, x.den)
    };
    assert_eq!(
        [
            r(30000, 1001),
            r(24000, 1001),
            r(60000, 1001),
            r(60000, 2002),
            r(25, 1),
            r(15, 1),
            r(12, 1),
            r(120, 1),
            r(144, 1),
            r(100, 1),
            r(10000, 333),
            r(0, 1),
        ],
        [
            (30000, 1001),
            (24000, 1001),
            (60000, 1001),
            (30000, 1001),
            (25, 1),
            (30, 1),
            (24, 1),
            (60, 1),
            (48, 1),
            (50, 1),
            (30, 1),
            (30, 1)
        ]
    );
}

/// Re-times the fixture's parts to `ranges` frames of `fpf` flicks.
fn regrid(f: &mut Fx, ranges: &[(i64, i64)], fpf: i64) {
    for (pid, (a, b)) in f.parts.clone().into_iter().zip(ranges) {
        let mut p = f.doc.get(pid).unwrap().clone();
        p.src_in = Flicks(a * fpf);
        p.src_out = Flicks(b * fpf);
        f.doc.insert_for_test(p);
    }
}

/// Decision 0020: a 29.97 / 23.976 / 59.94 framing plans at its exact rate (`/1`): the
/// sample clock rounds down per frame and telescopes, continuous joins stay continuous,
/// and a picture at another rate converts into it.
#[test]
fn ntsc_framings_plan_at_their_exact_rate() {
    let rates = [(30000i64, 1001u32), (24000, 1001), (60000, 1001)];
    for (num, den) in rates {
        for (src_num, src_den) in [(num, den), (30, 1), (24000, 1001), (60, 1)] {
            let out = Rational::new(num, den);
            let fpf = FLICKS_PER_SECOND * i64::from(den) / num;
            // two abutting parts (one continuous join) and a third after a jump
            let mut f = fx(&[(0, 1), (1, 2), (5, 6)], 0);
            let mut s = f.doc.get(f.pic).unwrap().clone();
            s.probe.tracks[0].fps = Some(Rational::new(src_num, src_den));
            f.doc.insert_for_test(s);
            let mut fr = f.doc.get(f.framing).unwrap().clone();
            fr.fps = out;
            f.doc.insert_for_test(fr);
            regrid(&mut f, &[(301, 333), (333, 360), (1001, 1031)], fpf);
            let p =
                plan(&f).unwrap_or_else(|e| panic!("{src_num}/{src_den} into {num}/{den}: {e}"));
            assert_eq!(
                (p.output.fps.num.get(), p.output.fps.den.get()),
                (num as u64, u64::from(den))
            );
            assert_eq!(
                serde_json::to_value(&p).unwrap()["schema"],
                "gmclip.renderplan/1"
            );
            assert!(
                gmclip_plan::check_plan(&p).is_empty(),
                "{:?}",
                gmclip_plan::check_plan(&p)
            );
            let same = src_num * i64::from(den) == num * i64::from(src_den);
            assert_eq!(p.parts[0].picture.fps.is_none(), same);
            assert_eq!(p.parts[0].picture.first_frame, 301);
            // the first sample is the part's time, rounded down
            let want = (301i128 * fpf as i128 * 48_000 / FLICKS_PER_SECOND as i128) as u64;
            assert_eq!(p.parts[0].audio.first_sample, want);
            // the abutting part continues the sample count exactly: no fades there
            let n0 = gmclip_plan::validate::part_samples(p.parts[0].out, p.output.fps).unwrap();
            assert_eq!(
                p.parts[1].audio.first_sample,
                p.parts[0].audio.first_sample + n0
            );
            assert_eq!(p.parts[0].audio.fade_out(), None);
            assert!(p.parts[2].audio.fade_in().is_some());
            // and it is within a sample of the time it starts at
            let ideal = 333i128 * fpf as i128 * 48_000 / FLICKS_PER_SECOND as i128;
            assert!((p.parts[1].audio.first_sample as i128 - ideal).abs() <= 1);
        }
    }
}
