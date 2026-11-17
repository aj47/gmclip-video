//! Part groups (decision 0037): `parts_group` gives parts one group id, a part already
//! grouped brings its whole group, a split keeps both halves in it, `parts_ungroup`
//! clears it; each is one undo step.

mod common;

use common::{SEC, fixture, open};
use gmclip_model::entities::*;
use gmclip_model::{Id, Ulid};
use serde_json::json;

fn group_of(p: &common::Project, part: &str) -> Option<Id<PartGroup>> {
    let id: Id<Part> = Id::new(Ulid::parse(part).unwrap());
    p.h.snapshot().doc.get(id).unwrap().part_group
}

#[test]
fn parts_group_split_and_ungroup() {
    let p = open("part-groups");
    let fx = fixture(&p, 4);
    let (a, b, c, d) = (&fx.parts[0], &fx.parts[1], &fx.parts[2], &fx.parts[3]);

    // one part is not a group
    assert!(
        p.try_human("parts_group", json!({"cut": fx.cut, "parts": [a]}))
            .is_err()
    );
    // a and c: one group (not b between them)
    let g = p
        .human("parts_group", json!({"cut": fx.cut, "parts": [a, c]}))
        .output;
    assert_eq!(g["parts"].as_array().unwrap().len(), 2);
    assert!(group_of(&p, a).is_some());
    assert_eq!(group_of(&p, a), group_of(&p, c));
    assert_eq!(group_of(&p, b), None);

    // grouping c with d brings a along: one group of three
    let g2 = p
        .human("parts_group", json!({"cut": fx.cut, "parts": [c, d]}))
        .output;
    assert_eq!(g2["parts"].as_array().unwrap().len(), 3);
    assert_eq!(group_of(&p, a), group_of(&p, d));

    // a split keeps both halves in the group
    let pa: Id<Part> = Id::new(Ulid::parse(a).unwrap());
    let at = p.h.snapshot().doc.get(pa).unwrap().src_in.0 + SEC / 2;
    let split = p.human("part_split", json!({"part": a, "at": at}));
    let half = split.output["id"].as_str().unwrap().to_owned();
    assert_eq!(group_of(&p, &half), group_of(&p, a));

    // ungroup by any member clears the whole group
    p.human("parts_ungroup", json!({"cut": fx.cut, "parts": [d]}));
    assert_eq!(group_of(&p, a), None);
    assert_eq!(group_of(&p, c), None);
    assert!(
        p.try_human("parts_ungroup", json!({"cut": fx.cut, "parts": [d]}))
            .is_err()
    );

    // undo brings the group back in one step
    p.human("undo", json!({}));
    assert!(group_of(&p, a).is_some());
    assert_eq!(group_of(&p, a), group_of(&p, d));
}
