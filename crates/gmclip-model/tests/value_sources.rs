//! A2.09 (SPEC §5.7 "inherited values shown with their source"): `value_sources` says,
//! per field of a layer in a part's stack, whether the visual split in force, the part's
//! own override, the linked layout, the framing's defaults or the part's own layer sets
//! it — and follows a link / unlink of the layout.

use std::collections::BTreeMap;

use gmclip_model::effective::{ValueSource, value_sources};
use gmclip_model::entities::*;
use gmclip_model::fixtures;
use gmclip_model::geometry::{NormRect, Place};
use gmclip_model::{Doc, Flicks, Id, OrderKey, Ulid};

const SEC: i64 = 705_600_000;

#[test]
fn each_field_names_where_it_comes_from() {
    let mut d = Doc::new();
    let cut = fixtures::cut(1);
    let fr = fixtures::framing(2, cut.id);
    let mut p = fixtures::part(3, cut.id, OrderKey::first(), 10 * SEC, 20 * SEC);
    let def = fixtures::layer(
        4,
        LayerOwner::Framing(fr.id),
        OrderKey::first(),
        LayerRole::Region(RegionRole::Screen),
    );
    let lay = fixtures::layout(5, cut.id);
    let in_layout = fixtures::layer(
        6,
        LayerOwner::Layout(lay.id),
        OrderKey::first(),
        LayerRole::Region(RegionRole::Screen),
    );
    let own = fixtures::layer(
        7,
        LayerOwner::PartOverride {
            part: p.id,
            framing: fr.id,
        },
        OrderKey::first(),
        LayerRole::Region(RegionRole::Chat),
    );
    // the part overrides the default layer's place and opacity
    p.look.insert(
        fr.id,
        PartLook {
            layout_ref: None,
            overrides: BTreeMap::from([(
                def.id,
                LayerPatch {
                    place: Some(Place {
                        x: 0.0,
                        y: 0.1,
                        w: 1.0,
                    }),
                    opacity: Some(0.5),
                    ..LayerPatch::default()
                },
            )]),
        },
    );
    for e in [&def, &in_layout, &own] {
        d.insert_for_test((*e).clone());
    }
    d.insert_for_test(cut.clone());
    d.insert_for_test(fr.clone());
    d.insert_for_test(lay.clone());
    d.insert_for_test(p.clone());
    // a visual split at 15 s re-crops the default layer
    d.insert_for_test(VisualSplit {
        id: Id::new(Ulid(8)),
        part: p.id,
        framing: fr.id,
        at_src: Flicks(15 * SEC),
        overrides: BTreeMap::from([(
            def.id,
            LayerPatch {
                crop: Some(NormRect {
                    x: 0.5,
                    y: 0.0,
                    w: 0.5,
                    h: 0.5,
                }),
                ..LayerPatch::default()
            },
        )]),
        transition: Transition::Cut,
        tag: None,
    });

    let at = |d: &Doc, l: Id<Layer>, s: i64| value_sources(d, p.id, fr.id, l, Flicks(s * SEC));
    let v = at(&d, def.id, 11).unwrap();
    assert_eq!(
        (v.owner, v.crop, v.place, v.opacity, v.z, v.cover),
        (
            ValueSource::Framing,
            ValueSource::Framing,
            ValueSource::Part,
            ValueSource::Part,
            ValueSource::Framing,
            ValueSource::Framing
        )
    );
    assert_eq!(v.split, None);
    // after the split: its crop
    let v = at(&d, def.id, 16).unwrap();
    assert_eq!((v.crop, v.place), (ValueSource::Split, ValueSource::Part));
    assert_eq!(v.split, Some(Id::new(Ulid(8))));
    // the part's own layer: every value its own
    let v = at(&d, own.id, 11).unwrap();
    assert_eq!(
        (v.owner, v.crop, v.place),
        (ValueSource::Own, ValueSource::Own, ValueSource::Own)
    );
    // the layout's layer is not in the stack until the part links it
    assert!(at(&d, in_layout.id, 11).is_none());
    let mut linked = d.clone();
    let mut lp = p.clone();
    lp.look.get_mut(&fr.id).unwrap().layout_ref = Some(lay.id);
    linked.insert_for_test(lp);
    let v = at(&linked, in_layout.id, 11).unwrap();
    assert_eq!(
        (v.owner, v.layout, v.crop),
        (ValueSource::Layout, Some(lay.id), ValueSource::Layout)
    );
    // linking replaces the default stack (SPEC §4.3.1)
    assert!(at(&linked, def.id, 11).is_none());
    // serialised for reads: snake_case words
    let j = serde_json::to_value(&v).unwrap();
    assert_eq!(j["owner"], "layout");
    assert_eq!(j["crop"], "layout");
    assert!(j.get("split").is_none());
}
