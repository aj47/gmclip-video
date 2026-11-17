//! Small constructors for tests and benches (this crate's and gmclip-core's). Ids are
//! `Ulid(n)` so fixtures read clearly; real ids come from the actor's minter.
#![doc(hidden)]

use std::collections::BTreeMap;

use crate::entities::*;
use crate::geometry::{NormRect, Place};
use crate::ids::{Id, Ulid};
use crate::order::OrderKey;
use crate::time::{Flicks, Rational};

pub fn cut(n: u128) -> Cut {
    Cut {
        id: Id::new(Ulid(n)),
        title: format!("cut {n}"),
        moment: None,
        brief: String::new(),
        notes: String::new(),
        last_run_summary: None,
        lineage: CutLineage::default(),
        story_arc: Vec::new(),
        loop_check: None,
        kind: CutKind::Short,
    }
}

pub fn source(n: u128) -> Source {
    Source {
        id: Id::new(Ulid(n)),
        path: format!("/media/{n}.mkv"),
        fingerprint: format!("fp{n}"),
        probe: Probe::default(),
        audio_roles: AudioRoles::default(),
        stream_layout: None,
        relink_hint: None,
        cursor: None,
        markers: Vec::new(),
    }
}

pub fn part(n: u128, cut: Id<Cut>, order: OrderKey, src_in: i64, src_out: i64) -> Part {
    Part {
        id: Id::new(Ulid(n)),
        cut,
        order,
        media: MediaRef::Source(Id::new(Ulid(9_000))),
        src_in: Flicks(src_in),
        src_out: Flicks(src_out),
        audio_from: None,
        role: PartRole::Other,
        from: PartFrom::Manual,
        lineage: PartLineage::default(),
        gain_db: 0.0,
        speed: 1.0,
        ramp: None,
        part_group: None,
        under_db: None,
        look: BTreeMap::new(),
        caption_place_override: BTreeMap::new(),
    }
}

pub fn framing(n: u128, cut: Id<Cut>) -> Framing {
    Framing {
        id: Id::new(Ulid(n)),
        cut,
        name: "9:16".into(),
        canvas: Canvas { w: 1080, h: 1920 },
        fps: Rational::new(30, 1),
        aspect_class: AspectClass::Portrait,
        background: Background::Solid {
            colour: "#0c1118".into(),
        },
        caption_style_ref: None,
        caption_grouping: CaptionGrouping::StoryV5,
        caption_place: None,
        bands: BandSet::default(),
        safe_zones: "instagram".into(),
        clips_enabled: Vec::new(),
        loudness_mode: None,
        export_presets: Vec::new(),
        caption_look: None,
    }
}

pub fn layout(n: u128, cut: Id<Cut>) -> Layout {
    Layout {
        id: Id::new(Ulid(n)),
        cut: Some(cut),
        scope: LayoutScope::Cut,
        name: format!("layout {n}"),
        aspect_class: AspectClass::Portrait,
        caption_place: None,
        region_relative: false,
    }
}

pub fn layer(n: u128, owner: LayerOwner, order: OrderKey, role: LayerRole) -> Layer {
    Layer {
        id: Id::new(Ulid(n)),
        owner,
        order,
        kind: LayerKind::SourceView,
        member: None,
        role,
        crop: NormRect::FULL,
        place: Place {
            x: 0.0,
            y: 0.0,
            w: 1.0,
        },
        z: 0,
        opacity: 1.0,
        visible: true,
        mirror: false,
        feather: Feather::default(),
        grade: None,
        matte: Matte::None,
        time_map: TimeMap::Live,
        out_of_range: OutOfRange::Hide,
        cover: None,
        track: None,
        frame: None,
    }
}

pub fn caption_doc(n: u128, cut: Id<Cut>) -> CaptionDoc {
    CaptionDoc {
        id: Id::new(Ulid(n)),
        cut,
        enabled: true,
        source: CaptionSource::Heard,
        edits: BTreeMap::new(),
        word_time_overrides: BTreeMap::new(),
        keywords: BTreeMap::new(),
        keyword_terms: Vec::new(),
        no_caption_ranges: BTreeMap::new(),
        cue_breaks: Default::default(),
        subline: None,
        drop_fillers: true,
    }
}
