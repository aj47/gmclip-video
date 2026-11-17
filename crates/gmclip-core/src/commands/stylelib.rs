//! The rest of the style library (SPEC §8 [M2], A2.09): text styles, pill presets, band
//! sets, layouts, stream layouts and background plates (decision 0029) saved by name in the project (`StyleLibrary`, keyed
//! `<kind>:<name>`) and applied where each belongs — a `Styles` entry, a framing's bands,
//! a new layout of a cut (linked to parts), a source's stream layout. Caption presets
//! are `caption_preset_*` ([`super::library`]). The app keeps the same items app-wide in
//! the profile's library and applies them with `item` (never stored in the project
//! until saved there).

use gmclip_macros::command;
use gmclip_model::entities::*;
use gmclip_model::{Id, Ulid};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::look::{LayerAdd, LayerAddArgs, LayoutSet, LayoutSetArgs, check_crop, check_place};
use crate::command::{Command, Cx};

const KINDS: &[&str] = &[
    "text_style",
    "pill_preset",
    "bands",
    "layout",
    "stream_layout",
    "plate",
];

/// The plates every project offers (decisions 0029, 0046): `(name, stops)`.
pub const BUILTIN_PLATES: &[(&str, &[&str])] = &[
    ("Midnight", &["#0c1118", "#1b2a4a"]),
    ("Stream blue", &["#0a2a6b", "#1460d6", "#0a2a6b"]),
    ("Ember", &["#1a0b0b", "#5c1d10", "#c2410c"]),
    ("Studio grey", &["#2b2f36", "#14161a"]),
    // decision 0046: Screen Studio-like wallpapers for a screen floating on a background
    ("Dusk", &["#2a1b4e", "#b4507a", "#f4a261"]),
    ("Ocean", &["#0b2a5b", "#1f6fb2", "#5ec8e5"]),
    ("Mint", &["#0f5a4f", "#3fbf9f", "#c8f2dc"]),
    ("Sunset", &["#5b1a3a", "#e0565b", "#ffb36b"]),
    ("Graphite", &["#3a3f47", "#1b1e23"]),
    ("Lilac", &["#5b4bc4", "#a98ee8", "#f1d9ff"]),
];

/// A built-in plate by name.
pub fn builtin_plate(name: &str) -> Option<PlatePreset> {
    BUILTIN_PLATES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, s)| PlatePreset {
            stops: s.iter().map(|c| (*c).to_owned()).collect(),
        })
}

/// A plate's stops: 2–8 `#RRGGBB` colours.
pub fn check_plate(p: &PlatePreset, path: &str) -> Result<(), CmdError> {
    if !(2..=8).contains(&p.stops.len())
        || p.stops
            .iter()
            .any(|c| gmclip_plan::scalars::Rgb::new(c.clone()).is_err())
    {
        return Err(CmdError::invalid(path, "2–8 #RRGGBB stops"));
    }
    Ok(())
}

fn item_name(n: &str) -> Result<(), CmdError> {
    if n.trim().is_empty() || n.chars().count() > 60 || n.chars().any(char::is_control) {
        return Err(CmdError::invalid("/name", "1–60 printable characters"));
    }
    Ok(())
}

fn kind_name(k: &str) -> Result<&'static str, CmdError> {
    KINDS.iter().copied().find(|x| *x == k).ok_or_else(|| {
        CmdError::invalid(
            "/kind",
            "text_style, pill_preset, bands, layout, stream_layout or plate",
        )
    })
}

fn key(kind: &str, name: &str) -> String {
    format!("{kind}:{name}")
}

/// An item's values are in range (sizes, rects, bands; a pill preset has its pill).
fn check_item(i: &LibraryItem, path: &str) -> Result<(), CmdError> {
    let size = |s: &TextStyle| {
        if s.size.is_finite() && s.size > 0.0 && s.size <= 1000.0 && s.font.chars().count() <= 200 {
            Ok(())
        } else {
            Err(CmdError::invalid(
                path,
                "a text size in (0, 1000] and a font name",
            ))
        }
    };
    match i {
        LibraryItem::TextStyle(s) => size(s),
        LibraryItem::PillPreset(s) => {
            size(s)?;
            if s.pill.is_none() {
                return Err(CmdError::invalid(path, "a pill preset has a pill"));
            }
            Ok(())
        }
        LibraryItem::Bands(b) => {
            if b.bands.len() > 32
                || b.bands.iter().any(|(n, x)| {
                    n.is_empty()
                        || n.chars().count() > 40
                        || !(x.y0.is_finite() && x.y1.is_finite())
                        || !(0.0..=1.0).contains(&x.y0)
                        || !(0.0..=1.0).contains(&x.y1)
                        || x.y0 >= x.y1
                })
            {
                return Err(CmdError::invalid(
                    path,
                    "≤ 32 named bands with 0 ≤ y0 < y1 ≤ 1",
                ));
            }
            Ok(())
        }
        LibraryItem::Layout(l) => {
            if l.layers.is_empty() || l.layers.len() > 16 {
                return Err(CmdError::invalid(path, "1–16 layers"));
            }
            if let Some(p) = &l.caption_place {
                check_place(p, path)?;
            }
            for x in &l.layers {
                check_crop(&x.crop, path)?;
                check_place(&x.place, path)?;
                if !(0.0..=1.0).contains(&x.opacity) || x.z.abs() > 10_000 {
                    return Err(CmdError::invalid(
                        path,
                        "opacity in [0, 1], z within ±10 000",
                    ));
                }
                match (&x.kind, &x.cover) {
                    (LayerKind::Cover, Some(c)) => super::look::check_cover(c, path)?,
                    (LayerKind::Cover, None) | (_, Some(_)) => {
                        return Err(CmdError::invalid(
                            path,
                            "a cover style on cover layers only",
                        ));
                    }
                    _ => {}
                }
                if let Some(f) = &x.frame {
                    super::look::check_frame(f, path)?;
                }
            }
            Ok(())
        }
        LibraryItem::Plate(p) => check_plate(p, path),
        LibraryItem::StreamLayout(s) => {
            if s.regions.is_empty()
                || s.regions.len() > 32
                || s.regions.iter().any(|r| !r.rect.is_valid())
                || s.canvas_size.w == 0
                || s.canvas_size.h == 0
            {
                return Err(CmdError::invalid(
                    path,
                    "1–32 regions inside [0,1] on a non-empty canvas",
                ));
            }
            Ok(())
        }
    }
}

/// Where a saved item is read from in the project.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LibrarySource {
    /// A named text style (`Styles`): saved as a pill preset when it has a pill.
    TextStyle(String),
    /// A framing's bands.
    Framing(Id<Framing>),
    /// A layout's layers and caption seat.
    Layout(Id<Layout>),
    /// A stream layout's regions.
    StreamLayout(Id<StreamLayout>),
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StyleLibrarySaveArgs {
    /// The item's name in the library.
    pub name: String,
    /// The item to save.
    #[serde(default)]
    pub item: Option<LibraryItem>,
    /// Or: read the item from the project.
    #[serde(default)]
    pub from: Option<LibrarySource>,
    /// With neither `item` nor `from`: remove `kind:name`.
    #[serde(default)]
    pub kind: Option<String>,
}

/// The library key an item was saved (or removed) under.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LibraryKey {
    pub key: String,
}

#[derive(Debug)]
pub struct StyleLibrarySave;

fn read_item(cx: &mut Cx<'_>, from: LibrarySource) -> Result<LibraryItem, CmdError> {
    Ok(match from {
        LibrarySource::TextStyle(n) => {
            let st = cx
                .tx
                .peek()
                .iter::<Styles>()
                .next()
                .map(|s| s.id)
                .ok_or_else(|| CmdError::invalid("/from", "the project has no text styles"))?;
            let s = cx
                .tx
                .get(st)?
                .entries
                .get(&n)
                .cloned()
                .ok_or_else(|| CmdError::invalid("/from", "no text style of that name"))?;
            if s.pill.is_some() {
                LibraryItem::PillPreset(s)
            } else {
                LibraryItem::TextStyle(s)
            }
        }
        LibrarySource::Framing(f) => LibraryItem::Bands(cx.tx.get(f)?.bands.clone()),
        LibrarySource::Layout(l) => {
            let lay = cx.tx.get(l)?.clone();
            let mut layers = Vec::new();
            for id in cx.tx.children::<Layer>(l.entity()) {
                let x = cx.tx.get(id)?;
                if x.owner != LayerOwner::Layout(l) {
                    continue;
                }
                layers.push(LayerPreset {
                    kind: x.kind,
                    role: x.role,
                    crop: x.crop,
                    place: x.place,
                    z: x.z,
                    opacity: x.opacity,
                    visible: x.visible,
                    cover: x.cover,
                    frame: x.frame.clone(),
                });
            }
            LibraryItem::Layout(LayoutPreset {
                aspect_class: lay.aspect_class,
                caption_place: lay.caption_place,
                layers,
            })
        }
        LibrarySource::StreamLayout(s) => {
            let sl = cx.tx.get(s)?;
            LibraryItem::StreamLayout(StreamLayoutPreset {
                canvas_size: sl.canvas_size,
                regions: sl.regions.clone(),
            })
        }
    })
}

#[command(
    name = "style_library_save",
    scope = "edit",
    undo = "undoable",
    doc = "Save a text style, pill preset, band set, layout, stream layout or background plate (`{plate: {stops: [\"#RRGGBB\", …]}}`, top to bottom) by name in the project's style library (SPEC §8) — given as `item`, or read `from` a text style, framing (its bands), layout or stream layout of the project; with neither, remove `kind:name`.",
    example = r#"{"name": "Panel + face", "from": {"layout": "01J00000000000000000000006"}}"#
)]
impl Command for StyleLibrarySave {
    type Args = StyleLibrarySaveArgs;
    type Output = LibraryKey;
    fn run(a: StyleLibrarySaveArgs, cx: &mut Cx<'_>) -> Result<LibraryKey, CmdError> {
        item_name(&a.name)?;
        let item = match (a.item, a.from) {
            (Some(_), Some(_)) => {
                return Err(CmdError::invalid("/from", "give item or from, not both"));
            }
            (Some(i), None) => Some(i),
            (None, Some(f)) => Some(read_item(cx, f)?),
            (None, None) => None,
        };
        let k = match &item {
            Some(i) => {
                check_item(i, "/item")?;
                key(i.kind(), &a.name)
            }
            None => key(
                kind_name(
                    a.kind
                        .as_deref()
                        .ok_or_else(|| CmdError::invalid("/kind", "the kind to remove"))?,
                )?,
                &a.name,
            ),
        };
        let existing = cx.tx.peek().iter::<StyleLibrary>().next().map(|s| s.id);
        match (existing, item) {
            (Some(id), item) => cx.tx.update(id, |l| match item {
                Some(i) => {
                    l.entries.insert(k.clone(), i);
                }
                None => {
                    l.entries.remove(&k);
                }
            })?,
            (None, Some(i)) => {
                let id: Id<StyleLibrary> = cx.tx.mint();
                cx.tx.create(StyleLibrary {
                    id,
                    entries: [(k.clone(), i)].into_iter().collect(),
                })?;
            }
            (None, None) => {}
        }
        Ok(LibraryKey { key: k })
    }
}

/// Where a library item is applied.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LibraryTarget {
    /// A text style or pill preset: the `Styles` entry to write (`[A-Za-z0-9_-]`, ≤ 64).
    Style(String),
    /// A band set: this framing's bands; a plate: this framing's background (the plate is
    /// kept in the project's library under its name, which the background names).
    Framing(Id<Framing>),
    /// A layout: a new layout in `cut`, linked to `parts` in `framing` when given.
    Cut {
        cut: Id<Cut>,
        #[serde(default)]
        framing: Option<Id<Framing>>,
        #[serde(default)]
        parts: Vec<Id<Part>>,
    },
    /// A stream layout: a new stream layout of this source (its regions: screen, cam,
    /// chat, alerts, private …, read by seeding and lint).
    Source(Id<Source>),
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StyleLibraryApplyArgs {
    pub kind: String,
    pub name: String,
    /// Apply this item (e.g. one of the app-wide library) instead of the project's.
    #[serde(default)]
    pub item: Option<LibraryItem>,
    pub to: LibraryTarget,
}

/// What an apply made: the new layout's or stream layout's id.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct Applied {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug)]
pub struct StyleLibraryApply;

fn parse_id<E>(s: &str) -> Result<Id<E>, CmdError> {
    Ulid::parse(s).map(Id::new).map_err(|_| CmdError::Internal {
        trace_id: "style-library-id".into(),
    })
}

#[command(
    name = "style_library_apply",
    scope = "edit",
    undo = "undoable",
    doc = "Apply a style library item (the project's `kind:name`, a built-in plate by name, or `item` itself): a text style or pill preset becomes a named text style, a band set a framing's bands, a plate a framing's background (kept in the project's library), a layout a new layout of a cut linked to the given parts, a stream layout a source's regions. One undo step.",
    example = r#"{"kind": "layout", "name": "Panel + face", "to": {"cut": {"cut": "01J00000000000000000000001", "framing": "01J00000000000000000000005", "parts": ["01J00000000000000000000003"]}}}"#
)]
impl Command for StyleLibraryApply {
    type Args = StyleLibraryApplyArgs;
    type Output = Applied;
    fn run(a: StyleLibraryApplyArgs, cx: &mut Cx<'_>) -> Result<Applied, CmdError> {
        item_name(&a.name)?;
        let kind = kind_name(&a.kind)?;
        let item = match a.item {
            Some(i) => i,
            None if kind == "plate"
                && let Some(p) = builtin_plate(&a.name)
                && !cx
                    .tx
                    .peek()
                    .iter::<StyleLibrary>()
                    .next()
                    .is_some_and(|l| l.entries.contains_key(&key(kind, &a.name))) =>
            {
                LibraryItem::Plate(p)
            }
            None => {
                let lib = cx
                    .tx
                    .peek()
                    .iter::<StyleLibrary>()
                    .next()
                    .map(|s| s.id)
                    .ok_or_else(|| CmdError::invalid("/name", "no library item of that name"))?;
                cx.tx
                    .get(lib)?
                    .entries
                    .get(&key(kind, &a.name))
                    .cloned()
                    .ok_or_else(|| CmdError::invalid("/name", "no library item of that name"))?
            }
        };
        if item.kind() != kind {
            return Err(CmdError::invalid("/item", "not an item of that kind"));
        }
        check_item(&item, "/item")?;
        match (item, a.to) {
            (LibraryItem::TextStyle(s) | LibraryItem::PillPreset(s), LibraryTarget::Style(n)) => {
                super::text::TextStyleSet::run(
                    super::text::TextStyleSetArgs {
                        name: n,
                        style: Some(s),
                    },
                    cx,
                )?;
                Ok(Applied::default())
            }
            (LibraryItem::Bands(b), LibraryTarget::Framing(f)) => {
                cx.tx.update(f, |x| x.bands = b)?;
                Ok(Applied::default())
            }
            (LibraryItem::Plate(p), LibraryTarget::Framing(f)) => {
                cx.tx.get(f)?;
                // the background names the plate: keep it in the project's library
                let k = key("plate", &a.name);
                let existing = cx.tx.peek().iter::<StyleLibrary>().next().map(|s| s.id);
                match existing {
                    Some(id) => cx.tx.update(id, |l| {
                        l.entries.insert(k, LibraryItem::Plate(p));
                    })?,
                    None => {
                        let id: Id<StyleLibrary> = cx.tx.mint();
                        cx.tx.create(StyleLibrary {
                            id,
                            entries: [(k, LibraryItem::Plate(p))].into_iter().collect(),
                        })?;
                    }
                }
                let name = a.name.clone();
                cx.tx
                    .update(f, |x| x.background = Background::Plate { name })?;
                Ok(Applied::default())
            }
            (
                LibraryItem::Layout(l),
                LibraryTarget::Cut {
                    cut,
                    framing,
                    parts,
                },
            ) => {
                cx.tx.get(cut)?;
                if !parts.is_empty() && framing.is_none() {
                    return Err(CmdError::invalid(
                        "/to/cut/framing",
                        "the framing to link the parts in",
                    ));
                }
                if let Some(f) = framing
                    && cx.tx.get(f)?.cut != cut
                {
                    return Err(CmdError::invalid(
                        "/to/cut/framing",
                        "a framing of this cut",
                    ));
                }
                for p in &parts {
                    if cx.tx.get(*p)?.cut != cut {
                        return Err(CmdError::invalid("/to/cut/parts", "parts of this cut"));
                    }
                }
                let id: Id<Layout> = cx.tx.mint();
                cx.tx.create(Layout {
                    id,
                    cut: Some(cut),
                    scope: LayoutScope::Cut,
                    name: a.name.clone(),
                    aspect_class: l.aspect_class,
                    caption_place: l.caption_place,
                    region_relative: false,
                })?;
                for x in l.layers {
                    let made = LayerAdd::run(
                        LayerAddArgs {
                            owner: LayerOwner::Layout(id),
                            role: x.role,
                            kind: Some(x.kind),
                            member: None,
                            crop: x.crop,
                            place: x.place,
                            z: x.z,
                            cover: x.cover,
                        },
                        cx,
                    )?;
                    if x.opacity != 1.0 || !x.visible || x.frame.is_some() {
                        let lid: Id<Layer> = parse_id(&made.id)?;
                        cx.tx.update(lid, |y| {
                            y.opacity = x.opacity;
                            y.visible = x.visible;
                            y.frame = x.frame;
                        })?;
                    }
                }
                if let Some(f) = framing {
                    for p in parts {
                        LayoutSet::run(
                            LayoutSetArgs {
                                part: p,
                                framing: f,
                                layout: Some(id),
                            },
                            cx,
                        )?;
                    }
                }
                Ok(Applied {
                    id: Some(id.0.to_string()),
                })
            }
            (LibraryItem::StreamLayout(s), LibraryTarget::Source(src)) => {
                cx.tx.get(src)?;
                let id: Id<StreamLayout> = cx.tx.mint();
                cx.tx.create(StreamLayout {
                    id,
                    name: a.name.clone(),
                    canvas_size: s.canvas_size,
                    fingerprint: format!("library:{}", a.name),
                    regions: s.regions,
                    audio_roles: None,
                    default_template: None,
                })?;
                cx.tx.update(src, |x| x.stream_layout = Some(id))?;
                Ok(Applied {
                    id: Some(id.0.to_string()),
                })
            }
            _ => Err(CmdError::invalid("/to", "not a place this kind applies to")),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StyleLibraryListArgs {
    /// Only this kind.
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LibraryRow {
    pub kind: String,
    pub name: String,
    pub item: LibraryItem,
    /// One of gmclip's own (a built-in plate), not saved in the project.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub builtin: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LibraryList {
    pub items: Vec<LibraryRow>,
}

#[derive(Debug)]
pub struct StyleLibraryList;

#[command(
    name = "style_library_list",
    scope = "read",
    undo = "read_only",
    doc = "The project's style library beyond caption presets (caption_preset_list): its text styles, pill presets, band sets, layouts, stream layouts and plates, by kind and name (the built-in plates are listed with `builtin: true`).",
    example = r#"{"kind": "layout"}"#
)]
impl Command for StyleLibraryList {
    type Args = StyleLibraryListArgs;
    type Output = LibraryList;
    fn run(a: StyleLibraryListArgs, cx: &mut Cx<'_>) -> Result<LibraryList, CmdError> {
        let want = a.kind.as_deref().map(kind_name).transpose()?;
        let lib = cx.tx.peek().iter::<StyleLibrary>().next().map(|s| s.id);
        let mut items: Vec<LibraryRow> = match lib {
            Some(id) => cx
                .tx
                .get(id)?
                .entries
                .iter()
                .filter_map(|(k, item)| {
                    let (kind, name) = k.split_once(':')?;
                    (want.is_none_or(|w| w == kind)).then(|| LibraryRow {
                        kind: kind.to_owned(),
                        name: name.to_owned(),
                        item: item.clone(),
                        builtin: false,
                    })
                })
                .collect(),
            None => Vec::new(),
        };
        // the built-in plates, unless the project saved its own of that name
        if want.is_none_or(|w| w == "plate") {
            for (n, _) in BUILTIN_PLATES {
                if !items.iter().any(|r| r.kind == "plate" && r.name == *n)
                    && let Some(p) = builtin_plate(n)
                {
                    items.push(LibraryRow {
                        kind: "plate".into(),
                        name: (*n).to_owned(),
                        item: LibraryItem::Plate(p),
                        builtin: true,
                    });
                }
            }
        }
        Ok(LibraryList { items })
    }
}

/// The stops of the plate a framing's background names: the project's saved plate of that
/// name, else a built-in one (decision 0029).
pub fn plate_stops(doc: &gmclip_model::Doc, name: &str) -> Option<Vec<String>> {
    let saved = doc
        .iter::<StyleLibrary>()
        .next()
        .and_then(|l| l.entries.get(&key("plate", name)));
    match saved {
        Some(LibraryItem::Plate(p)) => Some(p.stops.clone()),
        _ => builtin_plate(name).map(|p| p.stops),
    }
}
