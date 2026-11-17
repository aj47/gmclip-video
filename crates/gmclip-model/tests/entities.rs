//! Every SPEC §4.3 kind: a populated sample encodes canonically and decodes back to
//! itself (encode → decode is a fixpoint, A06), every field reads and writes through
//! its `FieldPath`, map fields address single keys, the diff of two samples applies as
//! ops, and the kind has a JSON Schema (A03).

use std::collections::BTreeMap;

use gmclip_model::anchor::{Anchor, CutEdge, FrameRef, LineRef, PartEdge, WordId, WordRef};
use gmclip_model::cbor;
use gmclip_model::entities::*;
use gmclip_model::entity::{FieldChange, FieldPath, FieldValue};
use gmclip_model::fixtures;
use gmclip_model::geometry::{CanvasBox, NormRect, Place};
use gmclip_model::kinds::{KINDS, Stored};
use gmclip_model::{Doc, Entity, EntityKind, EventId, Flicks, Id, OrderKey, RunId, Ulid};

fn id<E>(n: u128) -> Id<E> {
    Id::new(Ulid(n))
}

fn word(n: u32) -> WordRef {
    WordRef {
        word_id: WordId {
            asr_revision: "import:parakeet_coreml.v1".into(),
            ordinal: n,
        },
        t_src_start: Flicks(i64::from(n) * 1000),
        t_src_end: Flicks(i64::from(n) * 1000 + 500),
        normalised_text: "sixty".into(),
        prev2: vec!["in".into(), "under".into()],
        next2: vec!["seven".into(), "seconds".into()],
    }
}

fn style() -> TextStyle {
    TextStyle {
        font: "Neometric Heavy".into(),
        size: 66.66,
        fill: "#FFFFFF".into(),
        outline: Some(Outline {
            w: 12.0,
            colour: "#000000".into(),
        }),
        shadow: Some(Shadow {
            dx: 7.0,
            dy: 7.0,
            blur: 0.0,
            colour: "#000000".into(),
        }),
        align: TextAlign::Centre,
        max_width: Some(0.925),
        fit: TextFit::Shrink,
        line_height: 1.0,
        letter_spacing: 0.0,
        case: TextCase::AsIs,
        pill: Some(Pill {
            on: true,
            colour: "#101010".into(),
            opacity: 0.81,
            radius: 0.0,
            pad_x: 16.0,
            pad_y: 16.0,
            per_line: false,
        }),
    }
}

fn patch() -> LayerPatch {
    LayerPatch {
        removed: Some(false),
        crop: Some(NormRect {
            x: 0.1,
            y: 0.2,
            w: 0.3,
            h: 0.4,
        }),
        opacity: Some(0.5),
        grade: Some(OptSet::Clear),
        cover: Some(OptSet::Set(CoverStyle::Blur { radius: 8.0 })),
        ..LayerPatch::default()
    }
}

/// One populated sample per kind, as (kind, canonical bytes, a tweaked variant).
struct Sample {
    kind: EntityKind,
    check: Box<dyn Fn()>,
}

fn sample<E: Stored>(a: E, b: E) -> Sample {
    Sample {
        kind: E::KIND,
        check: Box::new(move || check_kind(&a, &b)),
    }
}

fn check_kind<E: Stored>(a: &E, b: &E) {
    assert_ne!(a, b, "{}: the variant must differ", E::KIND.0);
    // encode → decode is a fixpoint, and the bytes are canonical
    let bytes = cbor::to_vec(a);
    assert!(cbor::is_canonical(&bytes), "{}", E::KIND.0);
    let back: E = cbor::from_slice(&bytes).unwrap();
    assert_eq!(&back, a, "{}", E::KIND.0);
    assert_eq!(cbor::to_vec(&back), bytes, "{}", E::KIND.0);

    // every field reads and writes through its path
    for d in E::fields() {
        if d.map {
            assert!(
                a.get_field(&FieldPath::field(d.id.0)).is_err(),
                "{}.{} needs a key",
                E::KIND.0,
                d.name
            );
        } else {
            let v = a.get_field(&FieldPath::field(d.id.0)).unwrap();
            assert!(cbor::is_canonical(&v.0));
            let mut c = b.clone();
            c.set_field(&FieldPath::field(d.id.0), &v).unwrap();
            assert_eq!(c.get_field(&FieldPath::field(d.id.0)).unwrap(), v);
            assert!(
                a.get_field(&FieldPath::key(d.id.0, "x")).is_err(),
                "{}.{} takes no key",
                E::KIND.0,
                d.name
            );
        }
    }

    // the diff applies field by field and reaches the variant; map fields diff per key
    let mut changes: Vec<FieldChange> = Vec::new();
    a.diff(b, &mut changes);
    assert!(!changes.is_empty());
    let mut c = a.clone();
    for ch in &changes {
        let d = &E::fields()[usize::from(ch.path.field.0)];
        assert_eq!(d.map, ch.path.key.is_some(), "{}.{}", E::KIND.0, d.name);
        assert_eq!(c.get_field(&ch.path).unwrap(), ch.before);
        c.set_field(&ch.path, &ch.after).unwrap();
    }
    assert_eq!(&c, b, "{}", E::KIND.0);

    // a schema exists and names the fields
    let schema = serde_json::to_value(schemars::schema_for!(E)).unwrap();
    let props = schema["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("{} schema", E::KIND.0));
    for d in E::fields() {
        assert!(
            props.contains_key(d.name),
            "{}: schema lacks {}",
            E::KIND.0,
            d.name
        );
    }

    // the doc stores it and serves it back by kind
    let mut doc = Doc::new();
    doc.insert_for_test(a.clone());
    assert_eq!(doc.encode(a.entity_id()).unwrap().0, bytes);
}

#[allow(clippy::vec_init_then_push)]
fn samples() -> Vec<Sample> {
    let cut = fixtures::cut(1);
    let fr = id::<Framing>(2);
    let part = {
        let mut p = fixtures::part(3, cut.id, OrderKey::first(), 1000, 2000);
        p.look.insert(
            fr,
            PartLook {
                layout_ref: Some(id(4)),
                overrides: BTreeMap::from([(id::<Layer>(5), patch())]),
            },
        );
        p.caption_place_override.insert(
            fr,
            Place {
                x: 0.5,
                y: 0.55,
                w: 0.9,
            },
        );
        p.lineage = PartLineage {
            split_of: Some(id(6)),
            merged: vec![id(7)],
            suggestion: Some(id(8)),
            import_role: Some("story".into()),
        };
        p.speed = 0.5;
        p.ramp = Some(SpeedRamp {
            to: 2.0,
            src_start: Flicks(1200),
            src_end: Flicks(1600),
            curve: Curve::EaseInOut,
        });
        p
    };
    let anchor = Anchor::PartEnd(PartEdge {
        part: part.id,
        offset_ms: Some(-40),
    });
    let mut v = Vec::new();
    v.push(sample(
        Project {
            id: id(10),
            meta: ProjectMeta {
                name: "loc_4fadb756".into(),
                created_ms: 1_760_000_000_000,
                app_version: "0.0.1".into(),
            },
            active_asr: BTreeMap::from([("src".into(), "import:v1".into())]),
            profile_ref: Some("aj".into()),
        },
        Project {
            id: id(10),
            meta: ProjectMeta::default(),
            active_asr: BTreeMap::from([
                ("src".into(), "gmclip:1".into()),
                ("other".into(), "x".into()),
            ]),
            profile_ref: None,
        },
    ));
    v.push(sample(
        CaptionCorrections {
            id: id(11),
            entries: BTreeMap::from([("XD.dev".into(), "exe.dev".into())]),
        },
        CaptionCorrections {
            id: id(11),
            entries: BTreeMap::new(),
        },
    ));
    v.push(sample(
        Terms {
            id: id(12),
            entries: BTreeMap::from([(
                "exe".into(),
                Term {
                    written: "exe.dev".into(),
                    heard_as: vec!["XD.dev".into()],
                },
            )]),
        },
        Terms {
            id: id(12),
            entries: BTreeMap::new(),
        },
    ));
    v.push(sample(
        KeepPairs {
            id: id(13),
            entries: BTreeMap::from([("two minutes".into(), vec!["two".into(), "minutes".into()])]),
        },
        KeepPairs {
            id: id(13),
            entries: BTreeMap::new(),
        },
    ));
    v.push(sample(
        Styles {
            id: id(14),
            entries: BTreeMap::from([("caption".into(), style())]),
        },
        Styles {
            id: id(14),
            entries: BTreeMap::new(),
        },
    ));
    v.push(sample(
        CaptionPresets {
            id: id(140),
            entries: BTreeMap::from([(
                "word bounce".into(),
                CaptionPreset {
                    style: style(),
                    grouping: CaptionGrouping::AdaptiveWords,
                    look: CaptionLook {
                        animation: CaptionAnimation::Bounce,
                        keyword_animation: KeywordAnimation::Pop,
                        keyword_colour: Some("#FFD400FF".into()),
                        auto_emphasis: false,
                    },
                },
            )]),
        },
        CaptionPresets {
            id: id(140),
            entries: BTreeMap::new(),
        },
    ));
    v.push(sample(
        StyleLibrary {
            id: id(142),
            entries: BTreeMap::from([
                ("text_style:hook".into(), LibraryItem::TextStyle(style())),
                (
                    "bands:story".into(),
                    LibraryItem::Bands(BandSet {
                        bands: BTreeMap::from([("hook".into(), Band { y0: 0.11, y1: 0.21 })]),
                    }),
                ),
                (
                    "stream_layout:obs".into(),
                    LibraryItem::StreamLayout(StreamLayoutPreset {
                        canvas_size: Canvas { w: 2560, h: 1440 },
                        regions: vec![StreamRegion {
                            role: RegionRole::Chat,
                            rect: NormRect {
                                x: 0.7,
                                y: 0.0,
                                w: 0.3,
                                h: 0.5,
                            },
                        }],
                    }),
                ),
            ]),
        },
        StyleLibrary {
            id: id(142),
            entries: BTreeMap::new(),
        },
    ));
    v.push(sample(
        BrandKit {
            id: id(141),
            name: "TechFren".into(),
            handle: "@techfren".into(),
            heading_font: Some("Figtree Black".into()),
            body_font: None,
            colours: BrandColours::default(),
            logo: Some(OverlayImage {
                asset: id(15),
                w: 512,
                h: 512,
            }),
            caption_preset: Some("word_bounce".into()),
            templates: BTreeMap::from([(
                "pill".into(),
                OverlayTemplate {
                    kind: OverlayKind::Text,
                    role: Some("section_pill".into()),
                    band: Some("pill".into()),
                    z: 20,
                    opacity: 1.0,
                    geom: OverlayGeom::Place(Place {
                        x: 0.04,
                        y: 0.14,
                        w: 0.8,
                    }),
                    dur_ms: 3000,
                    text: None,
                    counter: Some(TemplateCounter {
                        clock: CounterClock::Src,
                        format: "m:ss".into(),
                        running_label: "GENERATING".into(),
                        done_label: "DONE IN".into(),
                    }),
                    image: None,
                    shape: None,
                },
            )]),
        },
        BrandKit {
            id: id(141),
            name: String::new(),
            handle: String::new(),
            heading_font: None,
            body_font: None,
            colours: BrandColours::default(),
            logo: None,
            caption_preset: None,
            templates: BTreeMap::new(),
        },
    ));
    v.push(sample(
        Asset {
            id: id(15),
            name: "logo.png".into(),
            sha256: "ab".repeat(32),
            size: 1234,
            media_type: "image/png".into(),
            probe: None,
        },
        Asset {
            id: id(15),
            name: "logo2.png".into(),
            sha256: "cd".repeat(32),
            size: 1,
            media_type: "video/mp4".into(),
            probe: Some(AssetProbe {
                w: 1080,
                h: 1920,
                fps: Some(gmclip_model::Rational::new(30, 1)),
                frames: Some(300),
            }),
        },
    ));
    v.push(sample(
        Font {
            id: id(16),
            family: "Neometric".into(),
            style: "Heavy".into(),
            sha256: None,
            em_per_size: 0.8332,
        },
        Font {
            id: id(16),
            family: "Inter".into(),
            style: "Bold".into(),
            sha256: Some("x".into()),
            em_per_size: 0.9,
        },
    ));
    let mut src = fixtures::source(17);
    src.probe = Probe {
        duration: Flicks(705_600_000 * 3600),
        tracks: vec![TrackProbe {
            index: 0,
            kind: TrackKind::Video,
            codec: "h264".into(),
            width: Some(2560),
            height: Some(1440),
            fps: Some(gmclip_model::Rational::new(60, 1)),
            timescale: 15360,
            gop_max_s: Some(2.0),
        }],
        audio_tracks: vec![AudioTrackProbe {
            index: 1,
            codec: "aac".into(),
            rate: 48_000,
            layout: "stereo".into(),
            bitrate: Some(160_000),
            speech_ratio: Some(0.71),
        }],
    };
    src.audio_roles = AudioRoles {
        voice: Some(1),
        program: vec![2],
        exclude: vec![3],
    };
    v.push(sample(src.clone(), fixtures::source(17)));
    v.push(sample(
        Region {
            id: id(18),
            source: src.id,
            role: RegionRole::Cam,
            rect: NormRect {
                x: 0.8,
                y: 0.44,
                w: 0.19,
                h: 0.24,
            },
            from: RegionFrom::Layout,
        },
        Region {
            id: id(18),
            source: src.id,
            role: RegionRole::Chat,
            rect: NormRect::FULL,
            from: RegionFrom::Human,
        },
    ));
    v.push(sample(
        SyncGroup {
            id: id(19),
            master_audio: Some(id(20)),
            picture: None,
        },
        SyncGroup {
            id: id(19),
            master_audio: None,
            picture: Some(id(20)),
        },
    ));
    v.push(sample(
        SyncMember {
            id: id(20),
            group: id(19),
            order: OrderKey::first(),
            source: src.id,
            role: MemberRole::Both,
            rate_ppm: 0.0,
            method: SyncMethod::Import,
            confidence: 1.0,
            under_db: None,
        },
        SyncMember {
            id: id(20),
            group: id(19),
            order: OrderKey::parse("a1").unwrap(),
            source: src.id,
            role: MemberRole::Audio,
            rate_ppm: 12.5,
            method: SyncMethod::FrameMatch,
            confidence: 0.9,
            under_db: Some(-12.0),
        },
    ));
    v.push(sample(
        SyncSegment {
            id: id(21),
            member: id(20),
            order: OrderKey::first(),
            src_range: SrcRange {
                start: Flicks(0),
                end: Flicks(10),
            },
            offset: Flicks(-1642),
        },
        SyncSegment {
            id: id(21),
            member: id(20),
            order: OrderKey::first(),
            src_range: SrcRange {
                start: Flicks(5),
                end: Flicks(10),
            },
            offset: Flicks(0),
        },
    ));
    v.push(sample(
        StreamLayout {
            id: id(22),
            name: "Main scene".into(),
            canvas_size: Canvas { w: 2560, h: 1440 },
            fingerprint: "fp".into(),
            regions: vec![StreamRegion {
                role: RegionRole::Cam,
                rect: NormRect::FULL,
            }],
            audio_roles: Some(AudioRoles::default()),
            default_template: Some("panel+face".into()),
        },
        StreamLayout {
            id: id(22),
            name: "Other".into(),
            canvas_size: Canvas { w: 1920, h: 1080 },
            fingerprint: "fp2".into(),
            regions: vec![],
            audio_roles: None,
            default_template: None,
        },
    ));
    let moment = Moment {
        id: id(23),
        media: MediaRef::Source(src.id),
        span: MomentSpan {
            start: Anchor::LineStart(LineRef {
                asr_revision: "r".into(),
                line: 4,
            }),
            end: Anchor::WordEnd(word(9)),
            edges: Edges::Line,
        },
        payoff: Some(Payoff {
            start: Anchor::WordStart(word(20)),
            end: Anchor::WordEnd(word(22)),
            from: PayoffFrom::Agent,
        }),
        quote: "my PC made this".into(),
        excerpt: Excerpt {
            text: "made this".into(),
            exact: true,
        },
        score: 5,
        r#type: "proof".into(),
        why: "result first".into(),
        payoff_hint: Some("00:44:39–00:45:00".into()),
        text_hook: Some("My PC made this in 67 seconds.".into()),
        origin: MomentOrigin::Agent,
        origin_ref: Some("loc_4fadb756_h79".into()),
        entry_angle: Some("proof-first".into()),
        variant_of: None,
        visual: Some(VisualRating {
            verdict: "yes".into(),
            r#match: Some("strong".into()),
        }),
        state: MomentState::Staged,
        pass_ref: None,
        harness_label: Some("claude-code".into()),
    };
    let mut moment2 = moment.clone();
    moment2.state = MomentState::Posted;
    moment2.payoff = None;
    v.push(sample(moment, moment2));
    let mut cut2 = cut.clone();
    cut2.notes = "AGENT EDIT PASS".into();
    cut2.last_run_summary = Some(RunSummary {
        run: RunId(Ulid(99)),
        text: "trimmed".into(),
        reversible_choices: vec![ReversibleChoice {
            label: "kept the uh".into(),
            restore: vec![FieldRestore {
                entity: cut.id.entity(),
                field: "notes".into(),
                key: None,
                value: "\"\"".into(),
            }],
        }],
    });
    v.push(sample(cut.clone(), cut2));
    let topic = Topic {
        id: id(26),
        cut: cut.id,
        order: OrderKey::first(),
        media: MediaRef::Source(id(7)),
        span: Span {
            start: Anchor::WordStart(word(1)),
            end: Anchor::WordEnd(word(9)),
        },
        title: "GPU prices".into(),
        colour: "#4f8cff".into(),
        origin: EditOrigin::Human,
        state: ProposalState::Accepted,
        notes: String::new(),
        conflict: false,
        replaces: vec![],
        pass_ref: None,
    };
    let mut topic2 = topic.clone();
    topic2.origin = EditOrigin::Agent;
    topic2.state = ProposalState::Proposed;
    topic2.conflict = true;
    topic2.replaces = vec![id(27)];
    topic2.pass_ref = Some("topics@1".into());
    v.push(sample(topic, topic2));
    let fluff = FluffProposal {
        id: id(28),
        cut: cut.id,
        order: OrderKey::first(),
        media: MediaRef::Source(id(7)),
        span: Span {
            start: Anchor::WordStart(word(10)),
            end: Anchor::WordEnd(word(12)),
        },
        reason: "dead air while typing".into(),
        origin: EditOrigin::Agent,
        state: ProposalState::Proposed,
        conflict: false,
        pass_ref: Some("topics@1".into()),
    };
    let mut fluff2 = fluff.clone();
    fluff2.state = ProposalState::Rejected;
    v.push(sample(fluff, fluff2));
    // decision 0015
    let comment = Comment {
        id: id(60),
        cut: cut.id,
        reply_to: None,
        at: Some(SrcAt {
            media: MediaRef::Source(id(7)),
            at_src: Flicks(705_600_000 * 12),
            part: Some(id(3)),
        }),
        frame_box: Some(NormRect {
            x: 0.1,
            y: 0.2,
            w: 0.3,
            h: 0.1,
        }),
        author: "review-link:0123456789abcdef".into(),
        text: "the prompt is unreadable here".into(),
        at_ms: 1,
        resolved: false,
        export_ref: Some("a1b2c3d4e5f60718".into()),
    };
    let mut comment2 = comment.clone();
    comment2.resolved = true;
    comment2.reply_to = Some(id(59));
    comment2.frame_box = None;
    v.push(sample(comment, comment2));
    let proposal = EditProposal {
        id: id(61),
        cut: cut.id,
        edit: SuggestedEdit::Trim {
            part: id(3),
            src_in: Flicks(0),
            src_out: Flicks(705_600_000),
        },
        author: "review-link:0123456789abcdef".into(),
        note: "start on the result".into(),
        at_ms: 2,
        state: ProposalState::Proposed,
        comment: Some(id(60)),
    };
    let mut proposal2 = proposal.clone();
    proposal2.state = ProposalState::Accepted;
    proposal2.edit = SuggestedEdit::CaptionMove {
        framing: id(4),
        part: None,
        place: Place {
            x: 0.5,
            y: 0.6,
            w: 0.9,
        },
    };
    v.push(sample(proposal, proposal2));
    let thumb = Thumbnail {
        id: id(62),
        cut: cut.id,
        cover: SrcAt {
            media: MediaRef::Source(id(7)),
            at_src: Flicks(705_600_000 * 3),
            part: None,
        },
        title: "My PC made this in *67 s*".into(),
        hook_options: vec!["a".into(), "b".into(), "c".into()],
        style: ThumbStyle {
            font: String::new(),
            size: 0.08,
            fill: "#FFFFFF".into(),
            outline: "#000000".into(),
            accent: "#FFD400".into(),
            uppercase: true,
        },
        layout: ThumbLayout {
            title_box: NormRect {
                x: 0.05,
                y: 0.1,
                w: 0.9,
                h: 0.25,
            },
            align: TextAlign::Centre,
        },
        origin: EditOrigin::Human,
        author: "human".into(),
        at_ms: 3,
        picked: true,
        notes: String::new(),
    };
    let mut thumb2 = thumb.clone();
    thumb2.picked = false;
    thumb2.origin = EditOrigin::Agent;
    thumb2.title = "b".into();
    v.push(sample(thumb, thumb2));
    let opening = Opening {
        id: id(63),
        cut: cut.id,
        label: "the reaction take".into(),
        spans: vec![OpeningSpan {
            media: MediaRef::Source(id(7)),
            src_in: Flicks(705_600_000 * 10),
            src_out: Flicks(705_600_000 * 14),
        }],
        hook_text: Some("Done in 67 seconds".into()),
        from: OpeningFrom::Moment,
        from_ref: Some("01J00000000000000000000009".into()),
        origin: EditOrigin::Human,
        at_ms: 4,
    };
    let mut opening2 = opening.clone();
    opening2.spans.push(OpeningSpan {
        media: MediaRef::Group(id(8)),
        src_in: Flicks(1),
        src_out: Flicks(2),
    });
    opening2.hook_text = None;
    opening2.from = OpeningFrom::WasMain;
    v.push(sample(opening, opening2));
    let draft = CopyDraft {
        id: id(64),
        cut: cut.id,
        platform: CopyPlatform::Youtube,
        title: "My PC made this in 67 seconds".into(),
        description: "A local video model, start to finish.".into(),
        state: ProposalState::Proposed,
        origin: EditOrigin::Agent,
        edited: false,
        pass_ref: Some("publish_copy@1:abc".into()),
        author: "launched:r1".into(),
        at_ms: 5,
    };
    let mut draft2 = draft.clone();
    draft2.platform = CopyPlatform::Tiktok;
    draft2.state = ProposalState::Accepted;
    draft2.edited = true;
    v.push(sample(draft, draft2));
    // decision 0046: a zoom section of every framing, then a layout section of one
    let section = Section {
        id: id(65),
        cut: cut.id,
        framing: None,
        start: Anchor::CutStart(CutEdge {
            cut: cut.id,
            offset_ms: None,
        }),
        end: Anchor::CutEnd(CutEdge {
            cut: cut.id,
            offset_ms: Some(-500),
        }),
        kind: SectionKind::Zoom(ZoomSpec {
            target: ZoomTarget::Point { x: 0.25, y: 0.75 },
            camera_scale: Some(0.5),
            auto: true,
            ..ZoomSpec::default()
        }),
    };
    let mut section2 = section.clone();
    section2.framing = Some(id(66));
    section2.kind = SectionKind::Layout {
        layout: id(67),
        ease_ms: 400,
    };
    v.push(sample(section, section2));
    let strike = SuggestedEdit::Strike {
        media: MediaRef::Source(id(7)),
        start: Flicks(1),
        end: Flicks(2),
        words: "um".into(),
    };
    let j = serde_json::to_value(&strike).unwrap();
    assert_eq!(j["op"], "strike");
    v.push(sample(
        StorySuggestion {
            id: id(24),
            cut: cut.id,
            order: OrderKey::first(),
            span: Span {
                start: Anchor::WordStart(word(1)),
                end: Anchor::WordEnd(word(3)),
            },
            role: PartRole::Evidence,
            relevance: 0.8,
            quote: "q".into(),
            state: SuggestionState::Suggested,
        },
        StorySuggestion {
            id: id(24),
            cut: cut.id,
            order: OrderKey::first(),
            span: Span {
                start: Anchor::WordStart(word(1)),
                end: Anchor::WordEnd(word(4)),
            },
            role: PartRole::Evidence,
            relevance: 0.8,
            quote: "q".into(),
            state: SuggestionState::Rejected,
        },
    ));
    v.push(sample(
        Review {
            id: id(25),
            cut: cut.id,
            at_ms: 1,
            author: "local".into(),
            text: "tighten the open".into(),
            verdict: Verdict::Change,
            export_ref: None,
        },
        Review {
            id: id(25),
            cut: cut.id,
            at_ms: 2,
            author: "review-link:x".into(),
            text: "ok".into(),
            verdict: Verdict::Keep,
            export_ref: Some("e".into()),
        },
    ));
    let mut part2 = part.clone();
    part2.look.insert(id(30), PartLook::default());
    part2.look.remove(&fr);
    part2.gain_db = 10.0;
    v.push(sample(part.clone(), part2));
    v.push(sample(
        VisualSplit {
            id: id(26),
            part: part.id,
            framing: fr,
            at_src: Flicks(1500),
            overrides: BTreeMap::from([(id(5), patch())]),
            transition: Transition::Ease {
                dur_ms: 300,
                curve: Curve::EaseInOut,
            },
            tag: None,
        },
        VisualSplit {
            id: id(26),
            part: part.id,
            framing: fr,
            at_src: Flicks(1600),
            overrides: BTreeMap::new(),
            transition: Transition::Cut,
            tag: None,
        },
    ));
    let hold = ContentHold {
        id: id(27),
        cut: cut.id,
        from: Anchor::PartStart(PartEdge {
            part: part.id,
            offset_ms: None,
        }),
        to: anchor.clone(),
        at: HoldAt::Frame(FrameRef {
            source: src.id,
            frame_index: 156_650,
        }),
        roles: vec![LayerRole::Region(RegionRole::Screen), LayerRole::Detail],
        per_framing: BTreeMap::from([(fr, FramingOptOut { ignore: true })]),
    };
    let mut hold2 = hold.clone();
    hold2.per_framing.clear();
    hold2.at = HoldAt::AtSrc(Flicks(1200));
    v.push(sample(hold, hold2));
    let hide = HideRoles {
        id: id(28),
        cut: cut.id,
        from: Anchor::CutStart(gmclip_model::anchor::CutEdge {
            cut: cut.id,
            offset_ms: Some(100),
        }),
        to: Anchor::Measured(id(40)),
        roles: vec![LayerRole::Region(RegionRole::Cam)],
        per_framing: BTreeMap::new(),
    };
    let mut hide2 = hide.clone();
    hide2.per_framing.insert(fr, FramingOptOut { ignore: true });
    v.push(sample(hide, hide2));
    let framing = fixtures::framing(2, cut.id);
    let mut framing2 = framing.clone();
    framing2.background = Background::Backdrop {
        asset: id(15),
        r#loop: true,
        fit: BackdropFit::Cover,
    };
    framing2.loudness_mode = Some(LoudnessMode::Static {
        integrated_lufs: -16.0,
        true_peak_dbtp: -1.5,
    });
    framing2.bands.bands.insert(
        "hook".into(),
        Band {
            y0: 220.0 / 1920.0,
            y1: 400.0 / 1920.0,
        },
    );
    v.push(sample(framing, framing2));
    let layout = fixtures::layout(4, cut.id);
    let mut layout2 = layout.clone();
    layout2.caption_place = Some(Place {
        x: 0.5,
        y: 0.5546875,
        w: 0.9,
    });
    v.push(sample(layout, layout2));
    let layer = fixtures::layer(
        5,
        LayerOwner::Layout(id(4)),
        OrderKey::first(),
        LayerRole::Region(RegionRole::Cam),
    );
    let mut layer2 = layer.clone();
    layer2.owner = LayerOwner::PartOverride {
        part: part.id,
        framing: fr,
    };
    layer2.matte = Matte::Chroma(ChromaKey {
        colour: "#00ff00".into(),
        similarity: 0.12,
        blend: 0.05,
        despill_mix: 0.5,
    });
    layer2.time_map = TimeMap::Hold(HoldAt::AtSrc(Flicks(5)));
    layer2.grade = Some(Grade {
        brightness: 0.0,
        contrast: 1.1,
        saturation: 1.0,
    });
    v.push(sample(layer, layer2));
    let overlay = Overlay {
        id: id(29),
        parent: OverlayParent::Cut(cut.id),
        kind: OverlayKind::Counter,
        start: Anchor::Frame(FrameRef {
            source: src.id,
            frame_index: 1,
        }),
        end: OverlayEnd::DurMs(3000),
        band: Some("pill".into()),
        z: 20,
        opacity: 1.0,
        geom: OverlayGeom::Box(CanvasBox {
            x: 0.04,
            y: 0.13,
            w: 0.5,
            h: 0.05,
        }),
        per_framing: BTreeMap::from([(
            fr,
            OverlayPerFraming {
                visible: true,
                place: Some(Place {
                    x: 0.1,
                    y: 0.1,
                    w: 0.5,
                }),
            },
        )]),
        span_mode: SpanMode::ClampToStretch,
        text: Some(OverlayText {
            runs: vec![TextRun {
                text: "GENERATING".into(),
                colour: None,
                keyword: Some(true),
            }],
            style: style(),
        }),
        counter: Some(Counter {
            start: CounterStart::Measurement(id(40)),
            stop: Some(CounterStart::Anchor(Anchor::VisualEvent(EventId(Ulid(3))))),
            clock: CounterClock::Src,
            format: "m:ss".into(),
            running_label: "GENERATING".into(),
            done_label: "DONE IN".into(),
        }),
        role: Some("run_clock".into()),
        number_provenance: vec![NumberProvenance {
            text: "1:07".into(),
            measurement: Some(id(40)),
            note: None,
            spoken: None,
        }],
        image: Some(OverlayImage {
            asset: id(15),
            w: 400,
            h: 200,
        }),
        shape: Some(OverlayShape {
            kind: ShapeKind::Arrow,
            stroke: "#FFD400FF".into(),
            width: 9.0,
            fill: None,
            radius: 0.0,
            from: Some([0.6, 0.5]),
            to: Some([0.4, 0.3]),
        }),
        track: Some(OverlayTrack {
            media: MediaRef::Source(src.id),
            keys: vec![TrackPoint {
                t: Flicks(705_600_000),
                x: 0.5,
                y: 0.25,
            }],
            method: "apple_vision.track_object".into(),
        }),
        video: Some(OverlayVideo {
            media: AssetOrSpan::Asset(id(15)),
            r#loop: true,
            crop: Some(gmclip_model::geometry::NormRect {
                x: 0.0,
                y: 0.0,
                w: 0.5,
                h: 0.5,
            }),
            matte: Some(Matte::Roto {
                model: "apple_vision".into(),
                soften: 2.0,
                strokes: vec![],
            }),
            lock: None,
        }),
        under: Some(1),
    };
    let mut overlay2 = overlay.clone();
    overlay2.per_framing.clear();
    overlay2.shape = None;
    overlay2.end = OverlayEnd::At(anchor.clone());
    v.push(sample(overlay, overlay2));
    v.push(sample(
        Keyframe {
            id: id(31),
            overlay: id(29),
            order: OrderKey::first(),
            at_local: anchor.clone(),
            place: None,
            scale: Some(1.18),
            opacity: Some(0.5),
            ease: Curve::EaseOut,
        },
        Keyframe {
            id: id(31),
            overlay: id(29),
            order: OrderKey::first(),
            at_local: anchor.clone(),
            place: Some(Place::default()),
            scale: None,
            opacity: None,
            ease: Curve::Linear,
        },
    ));
    v.push(sample(
        AudioOverlay {
            id: id(32),
            cut: cut.id,
            media: AssetOrSpan::Span {
                media: MediaRef::Source(src.id),
                src_in: Flicks(0),
                src_out: Flicks(10),
            },
            start: anchor.clone(),
            end: anchor.clone(),
            gain_db: -6.0,
            fade_in_ms: 100,
            fade_out_ms: 200,
            duck_under_speech: Some(Duck {
                depth_db: -12.0,
                attack_ms: 50,
                release_ms: 300,
            }),
        },
        AudioOverlay {
            id: id(32),
            cut: cut.id,
            media: AssetOrSpan::Asset(id(15)),
            start: anchor.clone(),
            end: anchor.clone(),
            gain_db: 0.0,
            fade_in_ms: 0,
            fade_out_ms: 0,
            duck_under_speech: None,
        },
    ));
    v.push(sample(
        GainSegment {
            id: id(33),
            cut: cut.id,
            order: OrderKey::first(),
            start: Anchor::WordStart(word(5)),
            end: Anchor::WordEnd(word(8)),
            gain_db: 10.0,
            ramp_in_ms: 80,
            ramp_out_ms: 120,
            curve: GainCurve::LinearDb,
            measurement_ref: Some(id(40)),
        },
        GainSegment {
            id: id(33),
            cut: cut.id,
            order: OrderKey::first(),
            start: Anchor::WordStart(word(5)),
            end: Anchor::WordEnd(word(9)),
            gain_db: 6.0,
            ramp_in_ms: 80,
            ramp_out_ms: 120,
            curve: GainCurve::LinearDb,
            measurement_ref: None,
        },
    ));
    let mut cd = fixtures::caption_doc(34, cut.id);
    cd.edits
        .insert(word(1).word_id, CaptionEdit::Text("exe.dev".into()));
    cd.edits.insert(word(2).word_id, CaptionEdit::Drop);
    cd.word_time_overrides.insert(
        word(3).word_id,
        WordTimeOverride {
            start: Some(Flicks(10)),
            end: None,
        },
    );
    cd.keywords.insert("k1".into(), word(4).word_id);
    cd.no_caption_ranges.insert(
        "n1".into(),
        NoCaptionRange {
            from: anchor.clone(),
            to: anchor.clone(),
        },
    );
    cd.subline = Some(Subline {
        content: "phrase".into(),
        style_ref: None,
    });
    let mut cd2 = cd.clone();
    cd2.edits.remove(&word(2).word_id);
    cd2.keywords.insert("k2".into(), word(5).word_id);
    cd2.source = CaptionSource::Imported;
    v.push(sample(cd, cd2));
    v.push(sample(
        Cue {
            id: id(35),
            caption_doc: id(34),
            order: OrderKey::first(),
            from: Anchor::Frame(FrameRef {
                source: src.id,
                frame_index: 10,
            }),
            to: Anchor::Frame(FrameRef {
                source: src.id,
                frame_index: 40,
            }),
            runs: vec![TextRun {
                text: "67 SECONDS".into(),
                colour: Some("&H00D4FF&".into()),
                keyword: Some(true),
            }],
            style_ref: Some("caption".into()),
            provenance: CueProvenance::Import,
        },
        Cue {
            id: id(35),
            caption_doc: id(34),
            order: OrderKey::first(),
            from: Anchor::Frame(FrameRef {
                source: src.id,
                frame_index: 10,
            }),
            to: Anchor::Frame(FrameRef {
                source: src.id,
                frame_index: 41,
            }),
            runs: vec![],
            style_ref: None,
            provenance: CueProvenance::Import,
        },
    ));
    let clip = Clip {
        id: id(36),
        cut: cut.id,
        media: AssetOrSpan::Asset(id(15)),
        slot: ClipSlot::After(part.id),
        look: BTreeMap::from([(fr, PartLook::default())]),
        key: None,
        gain_db: -4.0,
        match_loudness_to_cut: true,
        words: vec![ClipWord {
            word: "hi".into(),
            start: Flicks(0),
            end: Flicks(5),
        }],
        caption_place: None,
    };
    let mut clip2 = clip.clone();
    clip2.slot = ClipSlot::Intro;
    clip2.look.clear();
    v.push(sample(clip, clip2));
    v.push(sample(
        Measurement {
            id: id(40),
            kind: MeasurementKind::Interval,
            from: MeasureEnd::Frame(FrameRef {
                source: src.id,
                frame_index: 156_650,
            }),
            to: Some(MeasureEnd::Frame(FrameRef {
                source: src.id,
                frame_index: 160_696,
            })),
            value: 67.434,
            unit: "s".into(),
            evidence_frames: vec![FrameRef {
                source: src.id,
                frame_index: 156_650,
            }],
            method: "comfyui queue 0→1 to first result frame".into(),
            author: "human".into(),
        },
        Measurement {
            id: id(40),
            kind: MeasurementKind::Edge,
            from: MeasureEnd::Event(EventId(Ulid(1))),
            to: None,
            value: 0.0,
            unit: "s".into(),
            evidence_frames: vec![],
            method: "m".into(),
            author: "agent".into(),
        },
    ));
    v
}

#[test]
fn every_kind_round_trips_and_has_a_schema() {
    let samples = samples();
    let mut covered: Vec<&str> = samples.iter().map(|s| s.kind.0).collect();
    covered.sort_unstable();
    let mut all: Vec<&str> = KINDS.iter().map(|k| k.kind().0).collect();
    all.sort_unstable();
    assert_eq!(covered, all, "a sample per kind");
    for s in &samples {
        (s.check)();
    }
}

#[test]
fn map_keys_must_be_canonical() {
    let mut p = fixtures::part(3, id(1), OrderKey::first(), 0, 1);
    let look = FieldPath::named::<Part>("look", Some("0000000000000000000000000Z")).unwrap();
    let v = gmclip_model::entity::rt::encode(&PartLook::default());
    p.set_field(&look, &v).unwrap();
    assert_eq!(p.look.len(), 1);
    // a lowercase spelling of the same ULID is not a second key
    let lower = FieldPath::named::<Part>("look", Some("0000000000000000000000000z")).unwrap();
    assert!(p.get_field(&lower).is_err());
    // absent removes
    p.set_field(&look, &FieldValue::ABSENT).unwrap();
    assert!(p.look.is_empty());
    assert!(FieldPath::named::<Part>("look", None).is_none());
    assert!(FieldPath::named::<Part>("gain_db", Some("x")).is_none());
}

/// Decision 0011 is additive (R-CMD-5): a short (every cut made before `kind` existed)
/// encodes with no `kind` or lineage topics at all, so stored rows and ops keep their
/// exact bytes; a stream edit or long-form carries them.
#[test]
fn cut_kind_and_lineage_topics_are_additive() {
    let short = fixtures::cut(1);
    let bytes = cbor::to_vec(&short);
    let v: ciborium::Value = ciborium::from_reader(bytes.as_slice()).unwrap();
    let keys: Vec<String> = v
        .as_map()
        .unwrap()
        .iter()
        .filter_map(|(k, _)| k.as_text().map(str::to_owned))
        .collect();
    assert!(!keys.iter().any(|k| k == "kind"), "{keys:?}");
    let back: Cut = cbor::from_slice(&bytes).unwrap();
    assert_eq!(back.kind, CutKind::Short);
    assert_eq!(cbor::to_vec(&back), bytes);
    let mut long = short.clone();
    long.kind = CutKind::Long;
    long.lineage.forked_from = Some(id(2));
    long.lineage.topics = vec![id(3), id(4)];
    long.lineage.parent_base = vec![BaseRange {
        topic: id(3),
        media: MediaRef::Source(id(5)),
        start: Flicks(0),
        end: Flicks(705_600_000),
    }];
    let lb = cbor::to_vec(&long);
    assert!(cbor::is_canonical(&lb));
    assert_eq!(cbor::from_slice::<Cut>(&lb).unwrap(), long);
    let j = serde_json::to_value(&long).unwrap();
    assert_eq!(j["kind"], "long");
    assert_eq!(j["lineage"]["topics"].as_array().unwrap().len(), 2);
}
