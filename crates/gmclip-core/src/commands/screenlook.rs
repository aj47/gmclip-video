//! Built-in layouts for a screen + camera recording (decision 0046, "camera bubble"):
//! "Screen + camera bubble" in each corner, "Camera full", "Screen only" and, for 9:16,
//! "Screen over camera" (decision 0052), made in a cut from a recipe by role. The screen is the cut's screen picture (a recorded screen
//! file — a source with `cursor` — or the group's picture member, or a single source's
//! screen layer); the camera is the paired face file of a recording (another member of
//! the part's sync group with a picture), or a single source's camera region (the
//! stack's `cam` layer or its stream layout's `cam` region).

use gmclip_macros::command;
use gmclip_model::entities::*;
use gmclip_model::geometry::{NormRect, Place};
use gmclip_model::{Id, Ulid};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::look::{LayerAdd, LayerAddArgs, LayoutSet, LayoutSetArgs};
use crate::command::{Command, Cx};

/// The screen's padding on every side, a fraction of the canvas's short side.
pub const SCREEN_PADDING: f64 = 0.06;
/// The bubble's default diameter, a fraction of the canvas's short side (10–40 %).
pub const BUBBLE_SIZE: f64 = 0.22;
/// The bubble's distance from its corner's edges, a fraction of the canvas's short side.
pub const BUBBLE_MARGIN: f64 = 0.04;
/// The most of the canvas's height the screen takes in "Screen over camera".
pub const STACK_SCREEN_MAX: f64 = 0.55;
/// The 9:16 stack: a 9:16 framing of a recording with a face file starts with it.
pub const SCREEN_OVER_CAMERA: &str = "Screen over camera";

/// Where the bubble sits.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Corner {
    BottomRight,
    BottomLeft,
    TopRight,
    TopLeft,
}

/// One built-in layout's recipe.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Recipe {
    /// The screen padded and framed, the camera a bubble in a corner.
    ScreenBubble(Corner),
    /// The camera filling the canvas.
    CameraFull,
    /// The screen padded and framed.
    ScreenOnly,
    /// For 9:16: the whole screen across the top, the camera filling the rest.
    ScreenOverCamera,
}

/// The built-in layouts, by name.
pub const BUILTIN_LAYOUTS: &[(&str, Recipe)] = &[
    (
        "Screen + camera bubble (bottom right)",
        Recipe::ScreenBubble(Corner::BottomRight),
    ),
    (
        "Screen + camera bubble (bottom left)",
        Recipe::ScreenBubble(Corner::BottomLeft),
    ),
    (
        "Screen + camera bubble (top right)",
        Recipe::ScreenBubble(Corner::TopRight),
    ),
    (
        "Screen + camera bubble (top left)",
        Recipe::ScreenBubble(Corner::TopLeft),
    ),
    ("Camera full", Recipe::CameraFull),
    ("Screen only", Recipe::ScreenOnly),
    (SCREEN_OVER_CAMERA, Recipe::ScreenOverCamera),
];

impl Recipe {
    pub fn needs_screen(self) -> bool {
        !matches!(self, Recipe::CameraFull)
    }
    pub fn needs_camera(self) -> bool {
        !matches!(self, Recipe::ScreenOnly)
    }
}

/// The screen's frame in the built-ins: Screen Studio's rounded corners and soft shadow.
pub fn screen_frame() -> LayerFrame {
    LayerFrame {
        radius: 0.03,
        shadow: Some(FrameShadow {
            blur: 0.05,
            offset_y: 0.012,
            opacity: 0.4,
            colour: "#000000".into(),
        }),
        border: None,
    }
}

/// The bubble's frame: a circle (or a rounded square) with a thin white border.
pub fn bubble_frame(shape: BubbleShape) -> LayerFrame {
    LayerFrame {
        radius: match shape {
            BubbleShape::Circle => 0.5,
            BubbleShape::Rounded => 0.2,
        },
        shadow: Some(FrameShadow {
            blur: 0.04,
            offset_y: 0.01,
            opacity: 0.35,
            colour: "#000000".into(),
        }),
        border: Some(FrameBorder {
            width: 0.005,
            colour: "#FFFFFF".into(),
        }),
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BubbleShape {
    #[default]
    Circle,
    Rounded,
}

/// A picture the recipe places: which member (of the part's sync group) it reads, its
/// crop and its size in px.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    pub member: Option<Id<SyncMember>>,
    pub crop: NormRect,
    pub dims: (u32, u32),
}

fn dims_of(doc: &gmclip_model::Doc, s: Id<Source>) -> Option<(u32, u32)> {
    let v = doc
        .get(s)?
        .probe
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)?;
    Some((v.width?, v.height?))
}

/// The screen and the camera of a part in a framing (see the module docs).
pub fn screen_and_camera(
    doc: &gmclip_model::Doc,
    part: &Part,
    framing: Id<Framing>,
) -> (Option<Pick>, Option<Pick>) {
    let stack = gmclip_model::effective::effective(doc, part.id, framing, part.src_in);
    let crop_of = |role: RegionRole| {
        stack.as_ref().and_then(|s| {
            s.layers
                .iter()
                .find(|l| {
                    l.values.role == LayerRole::Region(role) && l.values.kind != LayerKind::Cover
                })
                .map(|l| l.values.crop)
        })
    };
    match part.media {
        MediaRef::Group(g) => {
            let members: Vec<&SyncMember> = doc
                .children::<SyncMember>(g.entity())
                .filter_map(|m| doc.get(m))
                .filter(|m| dims_of(doc, m.source).is_some())
                .collect();
            let group = doc.get(g);
            // a recorded screen file (decision 0045), else the group's picture
            let screen = members
                .iter()
                .find(|m| doc.get(m.source).is_some_and(|s| s.cursor.is_some()))
                .or_else(|| {
                    members
                        .iter()
                        .find(|m| group.and_then(|g| g.picture) == Some(m.id))
                })
                .or_else(|| {
                    members
                        .iter()
                        .find(|m| matches!(m.role, MemberRole::Picture | MemberRole::Both))
                })
                .copied();
            let camera = members
                .iter()
                .find(|m| screen.is_none_or(|s| s.source != m.source))
                .copied();
            let pick = |m: &SyncMember| {
                dims_of(doc, m.source).map(|dims| Pick {
                    member: Some(m.id),
                    crop: NormRect::FULL,
                    dims,
                })
            };
            (screen.and_then(pick), camera.and_then(pick))
        }
        MediaRef::Source(s) => {
            let Some(dims) = dims_of(doc, s) else {
                return (None, None);
            };
            let region = |role: RegionRole| {
                doc.get(s)
                    .and_then(|x| x.stream_layout)
                    .and_then(|l| doc.get(l))
                    .and_then(|l| l.regions.iter().find(|r| r.role == role).map(|r| r.rect))
            };
            let screen = Pick {
                member: None,
                crop: crop_of(RegionRole::Screen)
                    .or_else(|| region(RegionRole::Screen))
                    .unwrap_or(NormRect::FULL),
                dims,
            };
            let camera = crop_of(RegionRole::Cam)
                .or_else(|| region(RegionRole::Cam))
                .map(|crop| Pick {
                    member: None,
                    crop,
                    dims,
                });
            (Some(screen), camera)
        }
    }
}

/// The centred crop of `c` (normalised, in a `dims` picture) with the aspect `aw / ah`.
fn crop_to_aspect(c: NormRect, dims: (u32, u32), aw: f64, ah: f64) -> NormRect {
    let (sw, sh) = (f64::from(dims.0), f64::from(dims.1));
    let (w, h) = (c.w * sw, c.h * sh);
    let (nw, nh) = if w * ah > h * aw {
        (h * aw / ah, h)
    } else {
        (w, w * ah / aw)
    };
    NormRect {
        x: c.x + (w - nw) / 2.0 / sw,
        y: c.y + (h - nh) / 2.0 / sh,
        w: nw / sw,
        h: nh / sh,
    }
}

/// One layer a recipe makes: `(screen or camera, role, crop, place, z, frame)`.
/// `(of, role, picture, place, z, frame, visible)`. A recipe that leaves the screen or
/// the camera out still carries it, hidden, so a layout section of it hides that layer
/// (sections map layout layers onto a part's layers by role).
pub type RecipeLayer = (
    &'static str,
    LayerRole,
    Pick,
    Place,
    i32,
    Option<LayerFrame>,
    bool,
);

/// "Screen over camera": the screen across the canvas's full width at the top, whole
/// (cut top and bottom only where it would take more than [`STACK_SCREEN_MAX`] of the
/// height: a portrait display), and the camera below it filling the rest, cropped about
/// its centre. Edge to edge, without frames.
fn stack_layers(
    (cw, ch): (f64, f64),
    screen: Option<&Pick>,
    camera: Option<&Pick>,
) -> Vec<RecipeLayer> {
    let mut out = Vec::new();
    let mut top = 0.0;
    if let Some(s) = screen {
        let (sw, sh) = (
            s.crop.w * f64::from(s.dims.0),
            s.crop.h * f64::from(s.dims.1),
        );
        let h = (cw * sh / sw).min(STACK_SCREEN_MAX * ch);
        top = h;
        out.push((
            "screen",
            LayerRole::Region(RegionRole::Screen),
            Pick {
                crop: crop_to_aspect(s.crop, s.dims, cw, h),
                ..s.clone()
            },
            Place {
                x: 0.0,
                y: 0.0,
                w: 1.0,
            },
            0,
            None,
            true,
        ));
    }
    if let Some(c) = camera {
        out.push((
            "camera",
            LayerRole::Region(RegionRole::Cam),
            Pick {
                crop: crop_to_aspect(c.crop, c.dims, cw, ch - top),
                ..c.clone()
            },
            Place {
                x: 0.0,
                y: top / ch,
                w: 1.0,
            },
            10,
            None,
            true,
        ));
    }
    out
}

/// What a recipe draws on a `canvas`, given the screen and camera.
pub fn recipe_layers(
    r: Recipe,
    canvas: (u32, u32),
    screen: Option<&Pick>,
    camera: Option<&Pick>,
    size: f64,
    shape: BubbleShape,
) -> Vec<RecipeLayer> {
    let (cw, ch) = (f64::from(canvas.0), f64::from(canvas.1));
    if r == Recipe::ScreenOverCamera {
        return stack_layers((cw, ch), screen, camera);
    }
    let short = cw.min(ch);
    let mut out = Vec::new();
    let padded = |s: &Pick| {
        let (sw, sh) = (
            s.crop.w * f64::from(s.dims.0),
            s.crop.h * f64::from(s.dims.1),
        );
        let pad = SCREEN_PADDING * short;
        let (aw, ah) = (cw - 2.0 * pad, ch - 2.0 * pad);
        let k = (aw / sw).min(ah / sh);
        let (w, h) = (sw * k, sh * k);
        Place {
            x: (cw - w) / 2.0 / cw,
            y: (ch - h) / 2.0 / ch,
            w: w / cw,
        }
    };
    if let Some(s) = screen {
        out.push((
            "screen",
            LayerRole::Region(RegionRole::Screen),
            s.clone(),
            padded(s),
            0,
            Some(screen_frame()),
            r.needs_screen(),
        ));
    }
    if let Some(c) = camera {
        match r {
            Recipe::ScreenBubble(corner) => {
                let d = size * short;
                let m = BUBBLE_MARGIN * short;
                let x = match corner {
                    Corner::BottomRight | Corner::TopRight => cw - m - d,
                    _ => m,
                };
                let y = match corner {
                    Corner::BottomRight | Corner::BottomLeft => ch - m - d,
                    _ => m,
                };
                out.push((
                    "camera",
                    LayerRole::Region(RegionRole::Cam),
                    Pick {
                        crop: crop_to_aspect(c.crop, c.dims, 1.0, 1.0),
                        ..c.clone()
                    },
                    Place {
                        x: x / cw,
                        y: y / ch,
                        w: d / cw,
                    },
                    10,
                    Some(bubble_frame(shape)),
                    true,
                ));
            }
            Recipe::CameraFull => out.push((
                "camera",
                LayerRole::Region(RegionRole::Cam),
                Pick {
                    crop: crop_to_aspect(c.crop, c.dims, cw, ch),
                    ..c.clone()
                },
                Place {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                },
                10,
                None,
                true,
            )),
            Recipe::ScreenOnly | Recipe::ScreenOverCamera => out.push((
                "camera",
                LayerRole::Region(RegionRole::Cam),
                c.clone(),
                Place {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                },
                10,
                None,
                false,
            )),
        }
    }
    out
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayoutBuiltinListArgs {
    /// Say which ones this cut can make (it has a screen, a camera).
    #[serde(default)]
    pub cut: Option<Id<Cut>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct BuiltinLayoutRow {
    pub name: String,
    pub needs_screen: bool,
    pub needs_camera: bool,
    /// With `cut`: the cut's first part has what the layout needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct BuiltinLayoutList {
    pub layouts: Vec<BuiltinLayoutRow>,
}

#[derive(Debug)]
pub struct LayoutBuiltinList;

#[command(
    name = "layout_builtin_list",
    scope = "read",
    undo = "read_only",
    doc = "The built-in layouts for a screen + camera recording (decision 0046): \"Screen + camera bubble\" in each corner, \"Camera full\", \"Screen only\", and \"Screen over camera\" for 9:16 (decision 0052); with `cut`, whether its first part has the screen and camera each needs.",
    example = r#"{"cut": "01J00000000000000000000001"}"#
)]
impl Command for LayoutBuiltinList {
    type Args = LayoutBuiltinListArgs;
    type Output = BuiltinLayoutList;
    fn run(a: LayoutBuiltinListArgs, cx: &mut Cx<'_>) -> Result<BuiltinLayoutList, CmdError> {
        let have = match a.cut {
            Some(c) => {
                cx.tx.get(c)?;
                let doc = cx.tx.peek();
                let part = doc
                    .children::<Part>(c.entity())
                    .next()
                    .and_then(|p| doc.get(p));
                let framing = doc.children::<Framing>(c.entity()).next();
                Some(match (part, framing) {
                    (Some(p), Some(f)) => {
                        let (s, cam) = screen_and_camera(doc, p, f);
                        (s.is_some(), cam.is_some())
                    }
                    _ => (false, false),
                })
            }
            None => None,
        };
        Ok(BuiltinLayoutList {
            layouts: BUILTIN_LAYOUTS
                .iter()
                .map(|(n, r)| BuiltinLayoutRow {
                    name: (*n).to_owned(),
                    needs_screen: r.needs_screen(),
                    needs_camera: r.needs_camera(),
                    available: have
                        .map(|(s, c)| (!r.needs_screen() || s) && (!r.needs_camera() || c)),
                })
                .collect(),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayoutBuiltinAddArgs {
    pub cut: Id<Cut>,
    /// The framing whose canvas the layout is laid out for (and the parts linked in).
    pub framing: Id<Framing>,
    /// A built-in layout's name (`layout_builtin_list`).
    pub name: String,
    /// Link these parts to the new layout in `framing`.
    #[serde(default)]
    pub parts: Vec<Id<Part>>,
    /// The bubble's diameter, a fraction of the canvas's short side (0.1–0.4; 0.22).
    #[serde(default)]
    pub size: Option<f64>,
    /// `circle` (the default) or `rounded` (a rounded square).
    #[serde(default)]
    pub shape: Option<BubbleShape>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MadeLayer {
    pub id: String,
    /// `screen` or `camera`.
    pub of: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct BuiltinLayoutMade {
    /// The new layout's id.
    pub id: String,
    pub layers: Vec<MadeLayer>,
}

#[derive(Debug)]
pub struct LayoutBuiltinAdd;

#[command(
    name = "layout_builtin_add",
    scope = "edit",
    undo = "undoable",
    doc = "Make a built-in layout (decision 0046) in a cut from its recipe: the screen padded 6 % with rounded corners and a shadow, the camera a circle bubble (22 % of the short side, a white border, 4 % from the corner) or full; \"Screen over camera\" (for 9:16, decision 0052) puts the whole screen across the top (at most 55 % of the height) and the camera, cropped about its centre, filling the rest. Screen and camera are found by role (a recorded screen file, the paired face file, or a source's screen and cam regions). Links `parts` in `framing`. One undo step.",
    example = r#"{"cut": "01J00000000000000000000001", "framing": "01J00000000000000000000005", "name": "Screen + camera bubble (bottom right)", "parts": ["01J00000000000000000000003"]}"#
)]
impl Command for LayoutBuiltinAdd {
    type Args = LayoutBuiltinAddArgs;
    type Output = BuiltinLayoutMade;
    fn run(a: LayoutBuiltinAddArgs, cx: &mut Cx<'_>) -> Result<BuiltinLayoutMade, CmdError> {
        let recipe = BUILTIN_LAYOUTS
            .iter()
            .find(|(n, _)| *n == a.name)
            .map(|(_, r)| *r)
            .ok_or_else(|| CmdError::invalid("/name", "no built-in layout of that name"))?;
        let size = a.size.unwrap_or(BUBBLE_SIZE);
        if !(size.is_finite() && (0.1..=0.4).contains(&size)) {
            return Err(CmdError::invalid("/size", "0.1–0.4 of the short side"));
        }
        cx.tx.get(a.cut)?;
        let fr = cx.tx.get(a.framing)?.clone();
        if fr.cut != a.cut {
            return Err(CmdError::invalid("/framing", "a framing of this cut"));
        }
        let mut parts = Vec::new();
        for p in &a.parts {
            let part = cx.tx.get(*p)?.clone();
            if part.cut != a.cut {
                return Err(CmdError::invalid("/parts", "parts of this cut"));
            }
            parts.push(part);
        }
        // the screen and camera of the first part linked, else of the cut's first part
        let first = match parts.first() {
            Some(p) => p.clone(),
            None => {
                let id = cx
                    .tx
                    .children::<Part>(a.cut.entity())
                    .first()
                    .copied()
                    .ok_or_else(|| CmdError::invalid("/cut", "the cut has no parts"))?;
                cx.tx.get(id)?.clone()
            }
        };
        let (screen, camera) = screen_and_camera(cx.tx.peek(), &first, a.framing);
        if recipe.needs_screen() && screen.is_none() {
            return Err(CmdError::invalid("/name", "the cut has no screen picture"));
        }
        if recipe.needs_camera() && camera.is_none() {
            return Err(CmdError::invalid(
                "/name",
                "the cut has no camera (a paired face file or a cam region)",
            ));
        }
        let canvas = (fr.canvas.w, fr.canvas.h);
        let layers = recipe_layers(
            recipe,
            canvas,
            screen.as_ref(),
            camera.as_ref(),
            size,
            a.shape.unwrap_or_default(),
        );
        let id: Id<Layout> = cx.tx.mint();
        cx.tx.create(Layout {
            id,
            cut: Some(a.cut),
            scope: LayoutScope::Cut,
            name: a.name.clone(),
            aspect_class: match canvas.0.cmp(&canvas.1) {
                std::cmp::Ordering::Less => AspectClass::Portrait,
                std::cmp::Ordering::Greater => AspectClass::Landscape,
                std::cmp::Ordering::Equal => AspectClass::Square,
            },
            caption_place: None,
            region_relative: false,
        })?;
        let mut made = Vec::new();
        for (of, role, pick, place, z, frame, visible) in layers {
            let r = LayerAdd::run(
                LayerAddArgs {
                    owner: LayerOwner::Layout(id),
                    role,
                    kind: Some(LayerKind::SourceView),
                    member: pick.member,
                    crop: pick.crop,
                    place,
                    z,
                    cover: None,
                },
                cx,
            )?;
            let lid: Id<Layer> =
                Ulid::parse(&r.id)
                    .map(Id::new)
                    .map_err(|_| CmdError::Internal {
                        trace_id: "layout-builtin-id".into(),
                    })?;
            if frame.is_some() || !visible {
                cx.tx.update(lid, |l| {
                    l.frame = frame;
                    l.visible = visible;
                })?;
            }
            made.push(MadeLayer {
                id: r.id,
                of: of.to_owned(),
            });
        }
        for p in parts {
            LayoutSet::run(
                LayoutSetArgs {
                    part: p.id,
                    framing: a.framing,
                    layout: Some(id),
                },
                cx,
            )?;
        }
        Ok(BuiltinLayoutMade {
            id: id.0.to_string(),
            layers: made,
        })
    }
}
