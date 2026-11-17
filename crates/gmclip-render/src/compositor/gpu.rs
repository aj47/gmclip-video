//! The wgpu compositor: per layer a horizontal pass (NV12 planes → R'G'B' → Lanczos in
//! x) into an RGBA16F intermediate, then a vertical pass (Lanczos in y) blended
//! premultiplied-over into the RGBA16F canvas. Same arithmetic as [`super::cpu`].

use super::cpu::bounds;
use super::draw::{ComposeError, FrameDraw};

const SHADER: &str = r#"
struct P {
    crop: vec4<f32>,
    place: vec4<f32>,
    cb: vec4<i32>,   // cx0, cx1, cy0, cy1
    db: vec4<i32>,   // dx0, dx1, dy0, dy1
    misc: vec4<f32>, // sx, sy, opacity, code scale (255 for 8-bit planes)
    site: vec4<f32>, // x: chroma siting luma-x offset (0 left, 0.5 centre)
    mrect: vec4<f32>, // a matte's mask over the picture: x, y, w, h (picture px)
    frame: vec4<f32>, // decision 0046: x 1 when framed, y the corner radius (px)
};

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var ty: texture_2d<f32>;
@group(0) @binding(2) var tuv: texture_2d<f32>;
@group(0) @binding(3) var mid: texture_2d<f32>;
@group(0) @binding(4) var tm: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let q = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(q * 2.0 - 1.0, 0.0, 1.0);
}

const PI: f32 = 3.14159265358979;

fn lanczos3(t0: f32) -> f32 {
    let t = abs(t0);
    if (t < 1e-6) { return 1.0; }
    if (t >= 3.0) { return 0.0; }
    let pt = PI * t;
    return 3.0 * sin(pt) * sin(pt / 3.0) / (pt * pt);
}

fn rgb_at(x0: i32, y0: i32) -> vec3<f32> {
    let dy = vec2<i32>(textureDimensions(ty));
    let dc = vec2<i32>(textureDimensions(tuv));
    let x = clamp(x0, 0, dy.x - 1);
    let y = clamp(y0, 0, dy.y - 1);
    let luma = textureLoad(ty, vec2<i32>(x, y), 0).r * p.misc.w;
    let cxf = (f32(x) - p.site.x) / 2.0;
    let cyf = (f32(y) - 0.5) / 2.0;
    let j0 = i32(floor(cxf));
    let i0 = i32(floor(cyf));
    let fx = cxf - f32(j0);
    let fy = cyf - f32(i0);
    let a = textureLoad(tuv, clamp(vec2<i32>(j0, i0), vec2<i32>(0), dc - 1), 0).rg;
    let b = textureLoad(tuv, clamp(vec2<i32>(j0 + 1, i0), vec2<i32>(0), dc - 1), 0).rg;
    let c = textureLoad(tuv, clamp(vec2<i32>(j0, i0 + 1), vec2<i32>(0), dc - 1), 0).rg;
    let d = textureLoad(tuv, clamp(vec2<i32>(j0 + 1, i0 + 1), vec2<i32>(0), dc - 1), 0).rg;
    let ch = mix(mix(a, b, fx), mix(c, d, fx), fy) * p.misc.w;
    let yy = (luma - 16.0) / 219.0;
    let cb = (ch.x - 128.0) / 224.0;
    let cr = (ch.y - 128.0) / 224.0;
    return vec3<f32>(yy + 1.5748 * cr, yy - 0.18732427 * cb - 0.4681243 * cr, yy + 1.8556 * cb);
}

// Horizontal: target pixel (i, r) = destination column dx0 + i, source row cy0 + r.
@fragment
fn fs_h(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let x = p.db.x + i32(pos.x);
    let row = p.cb.z + i32(pos.y);
    let u = p.crop.x + (f32(x) + 0.5 - p.place.x) * p.misc.x;
    let s = max(1.0, p.misc.x);
    let k0 = i32(ceil(u - 3.0 * s - 0.5));
    let k1 = i32(floor(u + 3.0 * s - 0.5));
    var acc = vec3<f32>(0.0);
    var sum = 0.0;
    for (var k = k0; k <= k1; k++) {
        let w = lanczos3((f32(k) + 0.5 - u) / s);
        acc += w * rgb_at(clamp(k, p.cb.x, p.cb.y - 1), row);
        sum += w;
    }
    return vec4<f32>(acc / sum, 1.0);
}

// The matte at picture pixel (k, row): nearest mask texel, 0 outside (decision 0013;
// cpu::mask_at is the same in f32).
fn mask_at(k: i32, row: i32) -> f32 {
    let d = vec2<f32>(textureDimensions(tm));
    if (p.mrect.z <= 0.0 || p.mrect.w <= 0.0) { return 0.0; }
    let mx = floor((f32(k) + 0.5 - p.mrect.x) * d.x / p.mrect.z);
    let my = floor((f32(row) + 0.5 - p.mrect.y) * d.y / p.mrect.w);
    if (mx < 0.0 || my < 0.0 || mx >= d.x || my >= d.y) { return 0.0; }
    return textureLoad(tm, vec2<i32>(i32(mx), i32(my)), 0).r;
}

// Horizontal with a matte: premultiplied by the mask, alpha carried.
@fragment
fn fs_hm(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let x = p.db.x + i32(pos.x);
    let row = p.cb.z + i32(pos.y);
    let u = p.crop.x + (f32(x) + 0.5 - p.place.x) * p.misc.x;
    let s = max(1.0, p.misc.x);
    let k0 = i32(ceil(u - 3.0 * s - 0.5));
    let k1 = i32(floor(u + 3.0 * s - 0.5));
    var acc = vec4<f32>(0.0);
    var sum = 0.0;
    for (var k = k0; k <= k1; k++) {
        let w = lanczos3((f32(k) + 0.5 - u) / s);
        let kk = clamp(k, p.cb.x, p.cb.y - 1);
        let m = mask_at(kk, row);
        // site.y = 1: matte view (the mask in white)
        let rgb = select(rgb_at(kk, row), vec3<f32>(1.0), p.site.y > 0.5);
        acc += w * vec4<f32>(rgb * m, m);
        sum += w;
    }
    return acc / sum;
}

fn coverage(k: i32, a: f32, len: f32) -> f32 {
    let lo = max(f32(k), a);
    let hi = min(f32(k + 1), a + len);
    return clamp(hi - lo, 0.0, 1.0);
}

// Signed distance to a rectangle with rounded corners (draw::rounded_sd).
fn rounded_sd(px: f32, py: f32, rect: vec4<f32>, r0: f32) -> f32 {
    let hw = rect.z * 0.5;
    let hh = rect.w * 0.5;
    let r = max(min(min(r0, hw), hh), 0.0);
    let qx = abs(px - (rect.x + hw)) - (hw - r);
    let qy = abs(py - (rect.y + hh)) - (hh - r);
    let ox = max(qx, 0.0);
    let oy = max(qy, 0.0);
    return sqrt(ox * ox + oy * oy) + min(max(qx, qy), 0.0) - r;
}

// The layer's alpha before its opacity: its rounded shape when framed (decision 0046),
// else its place's coverage.
fn shape_cov(x: i32, y: i32) -> f32 {
    if (p.frame.x > 0.5) {
        let d = rounded_sd(f32(x) + 0.5, f32(y) + 0.5, p.place, p.frame.y);
        return clamp(0.5 - d, 0.0, 1.0);
    }
    return coverage(x, p.place.x, p.place.z) * coverage(y, p.place.y, p.place.w);
}

// Vertical: canvas pixel (x, y) inside the destination bounds; premultiplied output.
@fragment
fn fs_v(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let x = i32(pos.x);
    let y = i32(pos.y);
    let v = p.crop.y + (f32(y) + 0.5 - p.place.y) * p.misc.y;
    let s = max(1.0, p.misc.y);
    let k0 = i32(ceil(v - 3.0 * s - 0.5));
    let k1 = i32(floor(v + 3.0 * s - 0.5));
    var acc = vec3<f32>(0.0);
    var sum = 0.0;
    for (var k = k0; k <= k1; k++) {
        let w = lanczos3((f32(k) + 0.5 - v) / s);
        let kk = clamp(k, p.cb.z, p.cb.w - 1) - p.cb.z;
        acc += w * textureLoad(mid, vec2<i32>(x - p.db.x, kk), 0).rgb;
        sum += w;
    }
    let a = shape_cov(x, y) * p.misc.z;
    return vec4<f32>(acc / sum * a, a);
}

// Vertical with a matte: the intermediate is premultiplied with alpha.
@fragment
fn fs_vm(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let x = i32(pos.x);
    let y = i32(pos.y);
    let v = p.crop.y + (f32(y) + 0.5 - p.place.y) * p.misc.y;
    let s = max(1.0, p.misc.y);
    let k0 = i32(ceil(v - 3.0 * s - 0.5));
    let k1 = i32(floor(v + 3.0 * s - 0.5));
    var acc = vec4<f32>(0.0);
    var sum = 0.0;
    for (var k = k0; k <= k1; k++) {
        let w = lanczos3((f32(k) + 0.5 - v) / s);
        let kk = clamp(k, p.cb.z, p.cb.w - 1) - p.cb.z;
        acc += w * textureLoad(mid, vec2<i32>(x - p.db.x, kk), 0);
        sum += w;
    }
    let cov = shape_cov(x, y) * p.misc.z;
    let r = acc / sum;
    return vec4<f32>(r.rgb * cov, clamp(r.a, 0.0, 1.0) * cov);
}
"#;

const TILE_SHADER: &str = r#"
struct T { r: vec4<i32> }; // origin x, y, width, height
@group(0) @binding(0) var<uniform> t: T;
@group(0) @binding(1) var tile: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let q = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(q * 2.0 - 1.0, 0.0, 1.0);
}

// Premultiplied RGBA8 tile texels, drawn premultiplied-over.
@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(pos.xy) - t.r.xy;
    if (p.x < 0 || p.y < 0 || p.x >= t.r.z || p.y >= t.r.w) {
        discard;
    }
    return textureLoad(tile, p, 0);
}
"#;

/// Text tiles drawn onto a canvas smaller (or larger) than the plan's (the preview at
/// display size): each output pixel averages up to 8×8 tile texels over its footprint;
/// texels outside the tile count as transparent. At scale 1 [`TILE_SHADER`] draws instead,
/// so a full-size preview is the export's pixels (R-COMP-1).
const TILE_SCALED_SHADER: &str = r#"
struct T { r: vec4<i32>, s: vec4<f32> }; // origin x, y, width, height (canvas px); scale
@group(0) @binding(0) var<uniform> t: T;
@group(0) @binding(1) var tile: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let q = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(q * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let inv = 1.0 / t.s.xy;
    let c0 = (pos.xy - 0.5) * inv - vec2<f32>(t.r.xy);
    let n = clamp(vec2<i32>(ceil(inv)), vec2<i32>(1), vec2<i32>(8));
    let step = inv / vec2<f32>(n);
    var acc = vec4<f32>(0.0);
    for (var j = 0; j < n.y; j++) {
        for (var i = 0; i < n.x; i++) {
            let p = vec2<i32>(floor(c0 + (vec2<f32>(f32(i), f32(j)) + 0.5) * step));
            if (p.x >= 0 && p.y >= 0 && p.x < t.r.z && p.y < t.r.w) {
                acc += textureLoad(tile, p, 0);
            }
        }
    }
    if (acc.a <= 0.0) {
        discard;
    }
    return acc / f32(n.x * n.y);
}
"#;

/// The blurred-source fill's way back up (`/1`, decision 0014): each canvas pixel is a
/// cubic B-spline of the small picture (4×4 taps, clamped), dimmed, opaque. The same
/// arithmetic as `cpu::draw_backdrop`.
const UP_SHADER: &str = r#"
struct U { s: vec4<f32> }; // blur (canvas px per small px), dim
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var small: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let q = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(q * 2.0 - 1.0, 0.0, 1.0);
}

fn bs(t: f32) -> vec4<f32> {
    let t2 = t * t;
    let t3 = t2 * t;
    return vec4<f32>(
        (1.0 - t) * (1.0 - t) * (1.0 - t) / 6.0,
        (3.0 * t3 - 6.0 * t2 + 4.0) / 6.0,
        (-3.0 * t3 + 3.0 * t2 + 3.0 * t + 1.0) / 6.0,
        t3 / 6.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let d = vec2<i32>(textureDimensions(small));
    let uv = pos.xy / u.s.x - 0.5;
    let i = vec2<i32>(floor(uv));
    let t = uv - floor(uv);
    let wx = bs(t.x);
    let wy = bs(t.y);
    var acc = vec3<f32>(0.0);
    for (var j = 0; j < 4; j++) {
        let yy = clamp(i.y - 1 + j, 0, d.y - 1);
        var row = vec3<f32>(0.0);
        for (var k = 0; k < 4; k++) {
            let xx = clamp(i.x - 1 + k, 0, d.x - 1);
            row += wx[k] * textureLoad(small, vec2<i32>(xx, yy), 0).rgb;
        }
        acc += wy[j] * row;
    }
    return vec4<f32>(acc * (1.0 - u.s.y), 1.0);
}
"#;

/// A cover layer's way back up (decision 0021): canvas pixel `X` reads grid position
/// `u = (X + ½ − place.x) · grid.w / place.w` of the small picture the layer path drew
/// the crop into — a cubic B-spline (blur, 4×4 taps clamped to the grid) or the nearest
/// texel (pixelate) — or one colour (solid); coverage × opacity is the alpha, blended
/// premultiplied-over. The same arithmetic as `cpu::draw_cover`.
const COVER_SHADER: &str = r#"
struct C {
    place: vec4<f32>, // x, y, w, h (canvas px)
    s: vec4<f32>,     // grid.w / place.w, grid.h / place.h, mode (0 blur, 1 pixelate, 2 solid), opacity
    col: vec4<f32>,   // solid R'G'B'
    g: vec4<i32>,     // grid w, h
};
@group(0) @binding(0) var<uniform> c: C;
@group(0) @binding(1) var small: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let q = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(q * 2.0 - 1.0, 0.0, 1.0);
}

fn bs(t: f32) -> vec4<f32> {
    let t2 = t * t;
    let t3 = t2 * t;
    return vec4<f32>(
        (1.0 - t) * (1.0 - t) * (1.0 - t) / 6.0,
        (3.0 * t3 - 6.0 * t2 + 4.0) / 6.0,
        (-3.0 * t3 + 3.0 * t2 + 3.0 * t + 1.0) / 6.0,
        t3 / 6.0);
}

fn coverage(k: i32, a: f32, len: f32) -> f32 {
    let lo = max(f32(k), a);
    let hi = min(f32(k + 1), a + len);
    return clamp(hi - lo, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let x = i32(pos.x);
    let y = i32(pos.y);
    let a = coverage(x, c.place.x, c.place.z) * coverage(y, c.place.y, c.place.w) * c.s.w;
    let u = (f32(x) + 0.5 - c.place.x) * c.s.x;
    let v = (f32(y) + 0.5 - c.place.y) * c.s.y;
    var rgb = c.col.rgb;
    if (c.s.z < 0.5) {
        let uu = u - 0.5;
        let vv = v - 0.5;
        let i = i32(floor(uu));
        let j = i32(floor(vv));
        let wx = bs(uu - floor(uu));
        let wy = bs(vv - floor(vv));
        var acc = vec3<f32>(0.0);
        for (var dj = 0; dj < 4; dj++) {
            let yy = clamp(j - 1 + dj, 0, c.g.y - 1);
            for (var di = 0; di < 4; di++) {
                let xx = clamp(i - 1 + di, 0, c.g.x - 1);
                acc += wy[dj] * wx[di] * textureLoad(small, vec2<i32>(xx, yy), 0).rgb;
            }
        }
        rgb = acc;
    } else if (c.s.z < 1.5) {
        let i = clamp(i32(floor(u)), 0, c.g.x - 1);
        let j = clamp(i32(floor(v)), 0, c.g.y - 1);
        rgb = textureLoad(small, vec2<i32>(i, j), 0).rgb;
    }
    return vec4<f32>(rgb * a, a);
}
"#;

/// A layer frame's shadow (under the layer) or border (over it), decision 0046: the
/// same arithmetic as `cpu::draw_frame`, blended premultiplied-over.
const FRAME_SHADER: &str = r#"
struct F {
    rect: vec4<f32>, // the shape (moved down for a shadow): x, y, w, h (canvas px)
    s: vec4<f32>,    // corner radius px, mode (0 shadow, 1 border), soft px, alpha
    col: vec4<f32>,  // R'G'B'
};
@group(0) @binding(0) var<uniform> f: F;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let q = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(q * 2.0 - 1.0, 0.0, 1.0);
}

fn rounded_sd(px: f32, py: f32, rect: vec4<f32>, r0: f32) -> f32 {
    let hw = rect.z * 0.5;
    let hh = rect.w * 0.5;
    let r = max(min(min(r0, hw), hh), 0.0);
    let qx = abs(px - (rect.x + hw)) - (hw - r);
    let qy = abs(py - (rect.y + hh)) - (hh - r);
    let ox = max(qx, 0.0);
    let oy = max(qy, 0.0);
    return sqrt(ox * ox + oy * oy) + min(max(qx, qy), 0.0) - r;
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let x = floor(pos.x);
    let y = floor(pos.y);
    let d = rounded_sd(x + 0.5, y + 0.5, f.rect, f.s.x);
    var a = 0.0;
    if (f.s.y < 0.5) {
        if (f.s.z < 1.0) {
            a = clamp(0.5 - d, 0.0, 1.0);
        } else {
            let t = clamp(0.5 - d / f.s.z, 0.0, 1.0);
            a = t * t * (3.0 - 2.0 * t);
        }
    } else {
        a = clamp(0.5 - d, 0.0, 1.0) - clamp(0.5 - d - f.s.z, 0.0, 1.0);
    }
    a = a * f.s.w;
    return vec4<f32>(f.col.rgb * a, a);
}
"#;

/// Text tiles uploaded for the frames they are on, keyed by the tile's `Arc` address.
/// Each entry holds its `Arc`, so the address cannot be reused by another tile while
/// the entry exists (a freed-and-reallocated tile would otherwise hit a stale texture).
#[derive(Debug, Default)]
struct TileCache {
    textures: std::collections::HashMap<usize, CachedTile>,
}

#[derive(Debug)]
struct CachedTile {
    _tile: std::sync::Arc<crate::text::raster::Tile>,
    _texture: wgpu::Texture,
    _uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

/// The decoded planes of one layer's source frame, as textures.
#[derive(Debug)]
pub struct GpuPlanes<'a> {
    pub y: &'a wgpu::TextureView,
    pub uv: &'a wgpu::TextureView,
    pub width: u32,
    pub height: u32,
    /// Multiplier from the sampled unorm value to an 8-bit code (255 for 8-bit planes).
    pub code_scale: f32,
    pub siting: super::ChromaSiting,
}

/// The GPU compositor with its RGBA16F canvas.
#[derive(Debug)]
pub struct GpuCompositor {
    device: wgpu::Device,
    h_pipe: wgpu::RenderPipeline,
    v_pipe: wgpu::RenderPipeline,
    h_layout: wgpu::BindGroupLayout,
    v_layout: wgpu::BindGroupLayout,
    /// Matted layers (decision 0013): the horizontal pass with the mask bound, and the
    /// vertical pass that carries alpha.
    hm_pipe: wgpu::RenderPipeline,
    vm_pipe: wgpu::RenderPipeline,
    hm_layout: wgpu::BindGroupLayout,
    /// The current frame's mask textures (kept until the next frame is recorded).
    masks: Vec<wgpu::Texture>,
    canvas: wgpu::Texture,
    canvas_view: wgpu::TextureView,
    tile_pipe: wgpu::RenderPipeline,
    tile_scaled_pipe: wgpu::RenderPipeline,
    tile_layout: wgpu::BindGroupLayout,
    tiles: TileCache,
    /// The still backdrop's tile (decision 0028) and its upload.
    under: Option<std::sync::Arc<crate::text::raster::Tile>>,
    under_cached: Option<CachedTile>,
    /// Image tiles drawn among the layers (decision 0042): `(before layer i, tile)`.
    among: Vec<(usize, std::sync::Arc<crate::text::raster::Tile>)>,
    /// Canvas px per plan px for text tiles (1 = the export's own canvas).
    tile_scale: [f32; 2],
    /// One intermediate, grown to the largest layer seen (no per-size cache).
    mid: Option<wgpu::Texture>,
    /// Per-frame layer parameters at a 256-byte stride (dynamic offsets).
    params: Option<wgpu::Buffer>,
    /// The blurred-source fill: its upsample pass, the small picture and its uniform.
    up_pipe: wgpu::RenderPipeline,
    up_layout: wgpu::BindGroupLayout,
    small: Option<wgpu::Texture>,
    up_uniform: wgpu::Buffer,
    /// Cover layers (decision 0021): the pass back up, the grid picture (grown to the
    /// largest grid seen; each cover reads its own top-left corner) and the per-cover
    /// uniforms at a 256-byte stride.
    cover_pipe: wgpu::RenderPipeline,
    cover_layout: wgpu::BindGroupLayout,
    cover_small: Option<wgpu::Texture>,
    cover_params: Option<wgpu::Buffer>,
    /// Layer frames (decision 0046): the shadow and border pass and its uniforms at a
    /// 256-byte stride.
    frame_pipe: wgpu::RenderPipeline,
    frame_layout: wgpu::BindGroupLayout,
    frame_params: Option<wgpu::Buffer>,
}

const PARAM_STRIDE: u64 = 256;
const PARAM_SIZE: u64 = 128;
const COVER_SIZE: u64 = 64;
const FRAME_SIZE: u64 = 48;

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

impl GpuCompositor {
    pub const CANVAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("compositor"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let uniform = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: wgpu::BufferSize::new(PARAM_SIZE),
            },
            count: None,
        };
        // Both passes share one shader; each layout binds exactly what its entry point
        // reads (Metal's validation layer flags unused bindings).
        let h_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compositor-h"),
            entries: &[uniform, tex_entry(1), tex_entry(2)],
        });
        let v_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compositor-v"),
            entries: &[uniform, tex_entry(3)],
        });
        let hm_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compositor-hm"),
            entries: &[uniform, tex_entry(1), tex_entry(2), tex_entry(4)],
        });
        let pipe = |bgl: &wgpu::BindGroupLayout, entry: &str, blend: Option<wgpu::BlendState>| {
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(entry),
                bind_group_layouts: &[Some(bgl)],
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: Self::CANVAS_FORMAT,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let over = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let h_pipe = pipe(&h_layout, "fs_h", None);
        let v_pipe = pipe(
            &v_layout,
            "fs_v",
            Some(wgpu::BlendState {
                color: over,
                alpha: over,
            }),
        );
        let hm_pipe = pipe(&hm_layout, "fs_hm", None);
        let vm_pipe = pipe(
            &v_layout,
            "fs_vm",
            Some(wgpu::BlendState {
                color: over,
                alpha: over,
            }),
        );
        let canvas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("canvas"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: Self::CANVAS_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let tile_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("text-tiles"),
            source: wgpu::ShaderSource::Wgsl(TILE_SHADER.into()),
        });
        let tile_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("text-tiles"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1),
            ],
        });
        let tile_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("text-tiles"),
            bind_group_layouts: &[Some(&tile_layout)],
            immediate_size: 0,
        });
        let tile_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("text-tiles"),
            layout: Some(&tile_pl),
            vertex: wgpu::VertexState {
                module: &tile_module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &tile_module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: Self::CANVAS_FORMAT,
                    blend: Some(wgpu::BlendState {
                        color: over,
                        alpha: over,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let tile_scaled_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("text-tiles-scaled"),
            source: wgpu::ShaderSource::Wgsl(TILE_SCALED_SHADER.into()),
        });
        let tile_scaled_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("text-tiles-scaled"),
            layout: Some(&tile_pl),
            vertex: wgpu::VertexState {
                module: &tile_scaled_module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &tile_scaled_module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: Self::CANVAS_FORMAT,
                    blend: Some(wgpu::BlendState {
                        color: over,
                        alpha: over,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let up_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("backdrop-up"),
            source: wgpu::ShaderSource::Wgsl(UP_SHADER.into()),
        });
        let up_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("backdrop-up"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1),
            ],
        });
        let up_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("backdrop-up"),
            bind_group_layouts: &[Some(&up_layout)],
            immediate_size: 0,
        });
        let up_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("backdrop-up"),
            layout: Some(&up_pl),
            vertex: wgpu::VertexState {
                module: &up_module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &up_module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: Self::CANVAS_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let up_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("backdrop-up"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let cover_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cover"),
            source: wgpu::ShaderSource::Wgsl(COVER_SHADER.into()),
        });
        let cover_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cover"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(COVER_SIZE),
                    },
                    count: None,
                },
                tex_entry(1),
            ],
        });
        let cover_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cover"),
            bind_group_layouts: &[Some(&cover_layout)],
            immediate_size: 0,
        });
        let cover_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cover"),
            layout: Some(&cover_pl),
            vertex: wgpu::VertexState {
                module: &cover_module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &cover_module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: Self::CANVAS_FORMAT,
                    blend: Some(wgpu::BlendState {
                        color: over,
                        alpha: over,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let frame_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("frame"),
            source: wgpu::ShaderSource::Wgsl(FRAME_SHADER.into()),
        });
        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(FRAME_SIZE),
                },
                count: None,
            }],
        });
        let frame_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("frame"),
            bind_group_layouts: &[Some(&frame_layout)],
            immediate_size: 0,
        });
        let frame_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("frame"),
            layout: Some(&frame_pl),
            vertex: wgpu::VertexState {
                module: &frame_module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &frame_module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: Self::CANVAS_FORMAT,
                    blend: Some(wgpu::BlendState {
                        color: over,
                        alpha: over,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let canvas_view = canvas.create_view(&Default::default());
        Self {
            frame_pipe,
            frame_layout,
            frame_params: None,
            cover_pipe,
            cover_layout,
            cover_small: None,
            cover_params: None,
            up_pipe,
            up_layout,
            small: None,
            up_uniform,
            tile_pipe,
            tile_scaled_pipe,
            tile_layout,
            tiles: TileCache::default(),
            under: None,
            under_cached: None,
            among: Vec::new(),
            tile_scale: [1.0, 1.0],
            device: device.clone(),
            h_pipe,
            v_pipe,
            h_layout,
            v_layout,
            hm_pipe,
            vm_pipe,
            hm_layout,
            masks: Vec::new(),
            canvas,
            canvas_view,
            mid: None,
            params: None,
        }
    }

    pub fn canvas(&self) -> &wgpu::Texture {
        &self.canvas
    }

    /// Records one output frame. `planes[i]` are the source planes of
    /// `draw.layers[i]`. Layer parameters are written with `queue.write_buffer`, so
    /// submit `enc` before recording the next frame.
    pub fn encode(
        &mut self,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        draw: &FrameDraw,
        planes: &[GpuPlanes<'_>],
    ) -> Result<(), ComposeError> {
        self.encode_with_backdrop(queue, enc, draw, planes, None)
    }

    /// [`Self::encode`] with the blurred-source fill's picture (`draw.backdrop`'s planes).
    pub fn encode_with_backdrop(
        &mut self,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        draw: &FrameDraw,
        planes: &[GpuPlanes<'_>],
        backdrop: Option<&GpuPlanes<'_>>,
    ) -> Result<(), ComposeError> {
        if planes.len() != draw.layers.len() {
            return Err(ComposeError::Invalid("one plane set per layer".into()));
        }
        let (cw, ch) = (self.canvas.width(), self.canvas.height());
        // Geometry of every drawn layer first: parameters, then the passes. The backdrop,
        // when there is one, is the first job and draws into the small picture.
        /// Where a job's layer passes draw, and what follows them.
        #[derive(Copy, Clone, PartialEq)]
        enum Target {
            Canvas,
            /// The blurred-source fill's small picture, then its pass up.
            Backdrop,
            /// A cover's grid picture, then the cover pass (index into `covers`).
            Cover(usize),
            /// A solid cover: no layer passes, only the cover pass.
            Solid(usize),
        }
        struct Job<'p> {
            dx0: i32,
            dy0: i32,
            mw: u32,
            mh: u32,
            dh: u32,
            pl: &'p GpuPlanes<'p>,
            target: Target,
            /// Index into `self.masks` for a matted layer.
            mask: Option<usize>,
            /// Its index in `draw.layers` (the image tiles drawn among them go before it).
            layer: usize,
            /// Its frame's shadow and border passes (decision 0046): indexes into `frames`.
            shadow: Option<usize>,
            border: Option<usize>,
        }
        /// A cover's pass back up: its scissor on the canvas (x, y, w, h).
        struct CoverJob {
            rect: (u32, u32, u32, u32),
        }
        self.masks.clear();
        let mut jobs: Vec<Job<'_>> = Vec::new();
        let mut bytes: Vec<u8> = Vec::new();
        let (mut need_w, mut need_h) = (1u32, 1u32);
        let mut covers: Vec<CoverJob> = Vec::new();
        let mut cover_bytes: Vec<u8> = Vec::new();
        // layer frames' passes (decision 0046): scissors (x, y, w, h) and uniforms
        let mut frames: Vec<(u32, u32, u32, u32)> = Vec::new();
        let mut frame_bytes: Vec<u8> = Vec::new();
        let (mut grid_w, mut grid_h) = (1u32, 1u32);
        let small_size = match (&draw.backdrop, backdrop) {
            (Some(b), Some(_)) => Some(b.small(cw, ch)),
            _ => None,
        };
        // (layer, its planes, where it draws, its index in `draw.layers`)
        let mut items: Vec<(
            crate::compositor::draw::LayerDraw,
            &GpuPlanes<'_>,
            Target,
            usize,
        )> = Vec::with_capacity(planes.len() + 1);
        if let (Some(b), Some(pl), Some(((sw, sh), place))) = (&draw.backdrop, backdrop, small_size)
        {
            let _ = (sw, sh);
            items.push((
                crate::compositor::draw::LayerDraw {
                    layer_id: "backdrop".into(),
                    source: b.source.clone(),
                    frame: b.frame,
                    crop: super::draw::BackdropDraw::cover_crop(pl.width, pl.height, cw, ch),
                    place,
                    opacity: 1.0,
                    still: false,
                    matte: None,
                    cover: None,
                    shape: None,
                },
                pl,
                Target::Backdrop,
                0,
            ));
        }
        for (li, (l, pl)) in draw.layers.iter().zip(planes).enumerate() {
            let Some(cv) = &l.cover else {
                items.push((l.clone(), pl, Target::Canvas, li));
                continue;
            };
            // a cover (decision 0021): its place on the canvas, its pass's uniforms
            let p = l.place;
            if p.w <= 0.0 || p.h <= 0.0 {
                return Err(ComposeError::Invalid(format!(
                    "layer {}: empty crop or place",
                    l.layer_id
                )));
            }
            let (dx0, dx1) = bounds(p.x, p.w, cw);
            let (dy0, dy1) = bounds(p.y, p.h, ch);
            if dx1 <= dx0 || dy1 <= dy0 {
                continue;
            }
            let (n, m) = (cv.grid.0.max(1), cv.grid.1.max(1));
            let ci = covers.len();
            cover_bytes.resize(ci * PARAM_STRIDE as usize, 0);
            let (mode, col) = match cv.kind {
                super::CoverKind::Blur => (0.0, [0.0; 3]),
                super::CoverKind::Pixelate => (1.0, [0.0; 3]),
                super::CoverKind::Solid(rgb) => (2.0, rgb),
            };
            for v in [p.x, p.y, p.w, p.h] {
                cover_bytes.extend_from_slice(&(v as f32).to_le_bytes());
            }
            for v in [
                n as f32 / p.w as f32,
                m as f32 / p.h as f32,
                mode,
                l.opacity,
            ] {
                cover_bytes.extend_from_slice(&v.to_le_bytes());
            }
            for v in [col[0], col[1], col[2], 0.0] {
                cover_bytes.extend_from_slice(&v.to_le_bytes());
            }
            for v in [n as i32, m as i32, 0, 0] {
                cover_bytes.extend_from_slice(&v.to_le_bytes());
            }
            covers.push(CoverJob {
                rect: (
                    dx0 as u32,
                    dy0 as u32,
                    (dx1 - dx0) as u32,
                    (dy1 - dy0) as u32,
                ),
            });
            if matches!(cv.kind, super::CoverKind::Solid(_)) {
                items.push((l.clone(), pl, Target::Solid(ci), li));
            } else {
                grid_w = grid_w.max(n);
                grid_h = grid_h.max(m);
                items.push((cv.small_layer(l), pl, Target::Cover(ci), li));
            }
        }
        for (l, pl, target, li) in &items {
            if let Target::Solid(_) = target {
                // no layer passes: an empty parameter slot keeps slots = jobs
                let slot = jobs.len();
                bytes.resize(slot * PARAM_STRIDE as usize + PARAM_SIZE as usize, 0);
                jobs.push(Job {
                    dx0: 0,
                    dy0: 0,
                    mw: 0,
                    mh: 0,
                    dh: 0,
                    pl,
                    target: *target,
                    mask: None,
                    layer: *li,
                    shadow: None,
                    border: None,
                });
                continue;
            }
            let (c, p) = (l.crop, l.place);
            if c.w <= 0.0 || c.h <= 0.0 || p.w <= 0.0 || p.h <= 0.0 {
                return Err(ComposeError::Invalid(format!(
                    "layer {}: empty crop or place",
                    l.layer_id
                )));
            }
            let (tw, th) = match (target, small_size) {
                (Target::Backdrop, Some(((sw, sh), _))) => (sw, sh),
                (Target::Cover(_), _) => (p.w as u32, p.h as u32),
                _ => (cw, ch),
            };
            let (cx0, cx1) = bounds(c.x, c.w, pl.width);
            let (cy0, cy1) = bounds(c.y, c.h, pl.height);
            if cx1 <= cx0 || cy1 <= cy0 {
                return Err(ComposeError::Invalid(format!(
                    "layer {}: crop outside the source",
                    l.layer_id
                )));
            }
            let (dx0, dx1) = bounds(p.x, p.w, tw);
            let (dy0, dy1) = bounds(p.y, p.h, th);
            if dx1 <= dx0 || dy1 <= dy0 {
                continue;
            }
            let slot = jobs.len();
            bytes.resize(slot * PARAM_STRIDE as usize, 0);
            for v in [c.x, c.y, c.w, c.h, p.x, p.y, p.w, p.h] {
                bytes.extend_from_slice(&(v as f32).to_le_bytes());
            }
            for v in [cx0, cx1, cy0, cy1, dx0, dx1, dy0, dy1] {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            for v in [
                (c.w / p.w) as f32,
                (c.h / p.h) as f32,
                l.opacity,
                pl.code_scale,
            ] {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            let view = l.matte.as_ref().is_some_and(|m| m.view);
            for v in [pl.siting.x_offset(), if view { 1.0 } else { 0.0 }, 0.0, 0.0] {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            let mut mask = None;
            let mut mrect = [0f32; 4];
            if let Some(m) = &l.matte {
                let Some((mk, r)) = &m.mask else {
                    return Err(ComposeError::Unsupported(format!(
                        "layer {}: a roto matte with no mask",
                        l.layer_id
                    )));
                };
                if mk.width == 0 || mk.height == 0 {
                    return Err(ComposeError::Invalid(format!(
                        "layer {}: empty matte mask",
                        l.layer_id
                    )));
                }
                mrect = [r.x as f32, r.y as f32, r.w as f32, r.h as f32];
                let t = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("matte-mask"),
                    size: wgpu::Extent3d {
                        width: mk.width,
                        height: mk.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                queue.write_texture(
                    t.as_image_copy(),
                    &mk.data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(mk.width),
                        rows_per_image: Some(mk.height),
                    },
                    t.size(),
                );
                mask = Some(self.masks.len());
                self.masks.push(t);
            }
            for v in mrect {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            // decision 0046: a framed layer's alpha is its rounded shape's (canvas only)
            let shape = l.shape.as_ref().filter(|_| *target == Target::Canvas);
            let framed = match shape {
                Some(f) => [1.0, f.radius_px(&p), 0.0, 0.0],
                None => [0.0; 4],
            };
            for v in framed {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            let mut frame_slot = |part: super::cpu::FramePart| {
                let (rect, r, soft, alpha, rgb, [x0, x1, y0, y1]) =
                    super::cpu::frame_geometry(l, shape?, part, cw, ch)?;
                let k = frames.len();
                frame_bytes.resize(k * PARAM_STRIDE as usize, 0);
                let mode = if part == super::cpu::FramePart::Shadow {
                    0.0
                } else {
                    1.0
                };
                for v in rect.into_iter().chain([r, mode, soft, alpha]) {
                    frame_bytes.extend_from_slice(&v.to_le_bytes());
                }
                for v in [rgb[0], rgb[1], rgb[2], 0.0] {
                    frame_bytes.extend_from_slice(&v.to_le_bytes());
                }
                frames.push((x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32));
                Some(k)
            };
            let shadow = frame_slot(super::cpu::FramePart::Shadow);
            let border = frame_slot(super::cpu::FramePart::Border);
            let (mw, mh) = ((dx1 - dx0) as u32, (cy1 - cy0) as u32);
            need_w = need_w.max(mw);
            need_h = need_h.max(mh);
            jobs.push(Job {
                dx0,
                dy0,
                mw,
                mh,
                dh: (dy1 - dy0) as u32,
                pl,
                target: *target,
                mask,
                layer: *li,
                shadow,
                border,
            });
        }
        if !jobs.is_empty() {
            bytes.resize(
                (jobs.len() - 1) * PARAM_STRIDE as usize + PARAM_SIZE as usize,
                0,
            );
            let size = (jobs.len() as u64) * PARAM_STRIDE;
            if self.params.as_ref().is_none_or(|b| b.size() < size) {
                self.params = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("compositor-params"),
                    size,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            }
            if let Some(b) = &self.params {
                queue.write_buffer(b, 0, &bytes);
            }
            let grow = self
                .mid
                .as_ref()
                .is_none_or(|m| m.width() < need_w || m.height() < need_h);
            if grow {
                let (w, h) = match &self.mid {
                    Some(m) => (m.width().max(need_w), m.height().max(need_h)),
                    None => (need_w, need_h),
                };
                self.mid = Some(self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("compositor-mid"),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: Self::CANVAS_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                }));
            }
        }
        if let (Some(((sw, sh), _)), Some(b)) = (small_size, &draw.backdrop) {
            if self
                .small
                .as_ref()
                .is_none_or(|t| t.width() != sw || t.height() != sh)
            {
                self.small = Some(self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("backdrop-small"),
                    size: wgpu::Extent3d {
                        width: sw,
                        height: sh,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: Self::CANVAS_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                }));
            }
            let mut u = Vec::with_capacity(16);
            for v in [b.blur.max(1) as f32, b.dim, 0.0, 0.0] {
                u.extend_from_slice(&v.to_le_bytes());
            }
            queue.write_buffer(&self.up_uniform, 0, &u);
        }
        if !covers.is_empty() {
            let size = (covers.len() as u64) * PARAM_STRIDE;
            cover_bytes.resize(
                (covers.len() - 1) * PARAM_STRIDE as usize + COVER_SIZE as usize,
                0,
            );
            if self.cover_params.as_ref().is_none_or(|b| b.size() < size) {
                self.cover_params = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("cover-params"),
                    size,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            }
            if let Some(b) = &self.cover_params {
                queue.write_buffer(b, 0, &cover_bytes);
            }
            if self
                .cover_small
                .as_ref()
                .is_none_or(|t| t.width() < grid_w || t.height() < grid_h)
            {
                let (w, h) = match &self.cover_small {
                    Some(t) => (t.width().max(grid_w), t.height().max(grid_h)),
                    None => (grid_w, grid_h),
                };
                self.cover_small = Some(self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("cover-grid"),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: Self::CANVAS_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                }));
            }
        }
        if !frames.is_empty() {
            let size = (frames.len() as u64) * PARAM_STRIDE;
            frame_bytes.resize(
                (frames.len() - 1) * PARAM_STRIDE as usize + FRAME_SIZE as usize,
                0,
            );
            if self.frame_params.as_ref().is_none_or(|b| b.size() < size) {
                self.frame_params = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("frame-params"),
                    size,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            }
            if let Some(b) = &self.frame_params {
                queue.write_buffer(b, 0, &frame_bytes);
            }
        }
        let bg = draw.background;
        {
            let _clear = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("background"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.canvas_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(bg[0]),
                            g: f64::from(bg[1]),
                            b: f64::from(bg[2]),
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        // a still backdrop (decision 0028): its tile right over the clear, under every layer
        if let Some(t) = self.under.clone() {
            self.draw_under(queue, enc, &t);
        }
        let small_view = small_size
            .and(self.small.as_ref())
            .map(|t| t.create_view(&Default::default()));
        if let Some(v) = &small_view {
            let _clear = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("backdrop-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: v,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        // the image tiles drawn among the layers (decision 0042), uploaded before the
        // passes borrow the parameters
        let among = self.among.clone();
        let among_tiles: Vec<_> = among
            .iter()
            .map(|(_, t)| std::sync::Arc::clone(t))
            .collect();
        self.upload_tiles(queue, &among_tiles);
        let mut among_next = 0;
        let (Some(params), Some(mid)) = (&self.params, &self.mid) else {
            self.tile_pass(enc, &among_tiles);
            return Ok(()); // nothing visible
        };
        let mid_view = mid.create_view(&Default::default());
        let binding = wgpu::BufferBinding {
            buffer: params,
            offset: 0,
            size: wgpu::BufferSize::new(PARAM_SIZE),
        };
        let bg_v = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("layer-v"),
            layout: &self.v_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(binding.clone()),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&mid_view),
                },
            ],
        });
        let cover_view = if covers.is_empty() {
            None
        } else {
            self.cover_small
                .as_ref()
                .map(|t| t.create_view(&Default::default()))
        };
        let cover_bind = match (&cover_view, &self.cover_params) {
            (Some(v), Some(b)) => Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("cover"),
                layout: &self.cover_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: b,
                            offset: 0,
                            size: wgpu::BufferSize::new(COVER_SIZE),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(v),
                    },
                ],
            })),
            _ => None,
        };
        let frame_bind = match (&self.frame_params, frames.is_empty()) {
            (Some(b), false) => Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("frame"),
                layout: &self.frame_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: b,
                        offset: 0,
                        size: wgpu::BufferSize::new(FRAME_SIZE),
                    }),
                }],
            })),
            _ => None,
        };
        for (slot, j) in jobs.iter().enumerate() {
            if j.target != Target::Backdrop {
                // `<=`: a layer off the canvas has no job, so its tiles go with the next
                let n = among[among_next..]
                    .iter()
                    .take_while(|(i, _)| *i <= j.layer)
                    .count();
                if n > 0 {
                    self.tile_pass(enc, &among_tiles[among_next..among_next + n]);
                    among_next += n;
                }
            }
            let pl = j.pl;
            let offset = (slot as u64 * PARAM_STRIDE) as u32;
            if let Target::Solid(ci) = j.target {
                if let Some(bind) = &cover_bind {
                    self.cover_pass(enc, bind, ci, covers[ci].rect);
                }
                continue;
            }
            // a frame's shadow under the layer (decision 0046)
            if let (Some(k), Some(bind)) = (j.shadow, &frame_bind) {
                self.frame_pass(enc, bind, k, frames[k]);
            }
            let mask_view = j
                .mask
                .map(|i| self.masks[i].create_view(&Default::default()));
            let mut entries = vec![
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(binding.clone()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(pl.y),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(pl.uv),
                },
            ];
            if let Some(v) = &mask_view {
                entries.push(wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(v),
                });
            }
            let bg_h = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("layer-h"),
                layout: if mask_view.is_some() {
                    &self.hm_layout
                } else {
                    &self.h_layout
                },
                entries: &entries,
            });
            {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("layer-h"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &mid_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                pass.set_pipeline(if j.mask.is_some() {
                    &self.hm_pipe
                } else {
                    &self.h_pipe
                });
                pass.set_bind_group(0, &bg_h, &[offset]);
                // The layer's region is the top-left `mw × mh` of the shared intermediate.
                pass.set_viewport(0.0, 0.0, mid.width() as f32, mid.height() as f32, 0.0, 1.0);
                pass.set_scissor_rect(0, 0, j.mw, j.mh);
                pass.draw(0..3, 0..1);
            }
            let target = match (j.target, &small_view, &cover_view) {
                (Target::Backdrop, Some(v), _) | (Target::Cover(_), _, Some(v)) => v,
                _ => &self.canvas_view,
            };
            {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("layer-v"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                pass.set_pipeline(if j.mask.is_some() {
                    &self.vm_pipe
                } else {
                    &self.v_pipe
                });
                pass.set_bind_group(0, &bg_v, &[offset]);
                pass.set_scissor_rect(j.dx0 as u32, j.dy0 as u32, j.mw, j.dh);
                pass.draw(0..3, 0..1);
            }
            // and its border over it
            if let (Some(k), Some(bind)) = (j.border, &frame_bind) {
                self.frame_pass(enc, bind, k, frames[k]);
            }
            if let (Target::Cover(ci), Some(bind)) = (j.target, &cover_bind) {
                self.cover_pass(enc, bind, ci, covers[ci].rect);
            }
            if j.target == Target::Backdrop
                && let Some(v) = &small_view
            {
                let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("backdrop-up"),
                    layout: &self.up_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self.up_uniform.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(v),
                        },
                    ],
                });
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("backdrop-up"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.canvas_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                pass.set_pipeline(&self.up_pipe);
                pass.set_bind_group(0, &bind, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        self.tile_pass(enc, &among_tiles[among_next..]);
        Ok(())
    }

    /// The image tiles drawn among the layers (decision 0042) for the frames recorded
    /// from now on: `(i, t)` draws `t` just before `draw.layers[i]`, in list order
    /// ([`super::draw::FrameDraw::tiles_at`]).
    pub fn set_among(&mut self, tiles: Vec<(usize, std::sync::Arc<crate::text::raster::Tile>)>) {
        self.among = tiles;
    }

    /// One layer frame's shadow or border pass (decision 0046): `rect` is its scissor.
    fn frame_pass(
        &self,
        enc: &mut wgpu::CommandEncoder,
        bind: &wgpu::BindGroup,
        k: usize,
        rect: (u32, u32, u32, u32),
    ) {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.canvas_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.frame_pipe);
        pass.set_bind_group(0, bind, &[(k as u64 * PARAM_STRIDE) as u32]);
        pass.set_scissor_rect(rect.0, rect.1, rect.2, rect.3);
        pass.draw(0..3, 0..1);
    }

    /// One cover's pass back up (decision 0021): `rect` is its scissor on the canvas.
    fn cover_pass(
        &self,
        enc: &mut wgpu::CommandEncoder,
        bind: &wgpu::BindGroup,
        ci: usize,
        rect: (u32, u32, u32, u32),
    ) {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("cover"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.canvas_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.cover_pipe);
        pass.set_bind_group(0, bind, &[(ci as u64 * PARAM_STRIDE) as u32]);
        pass.set_scissor_rect(rect.0, rect.1, rect.2, rect.3);
        pass.draw(0..3, 0..1);
    }

    /// The still backdrop's tile (decision 0028) for the frames recorded from now on:
    /// drawn over the background colour and under every layer by [`Self::encode`].
    pub fn set_under(&mut self, tile: Option<std::sync::Arc<crate::text::raster::Tile>>) {
        let same = match (&self.under, &tile) {
            (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.under = tile;
            self.under_cached = None;
        }
    }

    fn draw_under(
        &mut self,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        t: &std::sync::Arc<crate::text::raster::Tile>,
    ) {
        let (cw, ch) = (
            f64::from(self.canvas.width()),
            f64::from(self.canvas.height()),
        );
        let [sx, sy] = self.tile_scale;
        let f = |v: i32, s: f32| f64::from(v) * f64::from(s);
        let x0 = f(t.x, sx).floor().max(0.0);
        let y0 = f(t.y, sy).floor().max(0.0);
        let x1 = f(t.x + t.width as i32, sx).ceil().min(cw);
        let y1 = f(t.y + t.height as i32, sy).ceil().min(ch);
        if x1 <= x0 || y1 <= y0 || t.width == 0 || t.height == 0 {
            return;
        }
        if self.under_cached.is_none() {
            self.under_cached = Some(self.upload_tile(queue, t));
        }
        let Some(c) = &self.under_cached else {
            return;
        };
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("backdrop-still"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.canvas_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(if self.tile_scale != [1.0, 1.0] {
            &self.tile_scaled_pipe
        } else {
            &self.tile_pipe
        });
        pass.set_bind_group(0, &c.bind, &[]);
        pass.set_scissor_rect(x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32);
        pass.draw(0..3, 0..1);
    }

    /// Draws text tiles at `sx`, `sy` canvas px per plan px (a preview at display size);
    /// 1, 1 is the export's canvas. Uploaded tiles are dropped (their uniforms carry it).
    pub fn set_tile_scale(&mut self, sx: f32, sy: f32) {
        if self.tile_scale != [sx, sy] {
            self.tile_scale = [sx, sy];
            self.tiles.textures.clear();
            self.under_cached = None;
        }
    }

    /// Records the text tiles of one frame over the canvas, bottom to top, in one render
    /// pass (after [`Self::encode`] for the same frame). Each tile is uploaded once while
    /// it stays on screen, with its bind group.
    pub fn draw_tiles(
        &mut self,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        tiles: &[std::sync::Arc<crate::text::raster::Tile>],
    ) {
        // the tiles kept: these and the ones drawn among the layers
        let live: std::collections::HashSet<usize> = tiles
            .iter()
            .chain(self.among.iter().map(|(_, t)| t))
            .map(|t| std::sync::Arc::as_ptr(t) as usize)
            .collect();
        self.tiles.textures.retain(|k, _| live.contains(k));
        self.upload_tiles(queue, tiles);
        self.tile_pass(enc, tiles);
    }

    /// The scissor of a tile on the canvas, or `None` when nothing of it is on it.
    fn tile_rect(&self, t: &crate::text::raster::Tile) -> Option<[u32; 4]> {
        let (cw, ch) = (
            i64::from(self.canvas.width()),
            i64::from(self.canvas.height()),
        );
        let [sx, sy] = self.tile_scale;
        let (x0, y0, x1, y1) = if self.tile_scale != [1.0, 1.0] {
            let f = |v: i32, s: f32| f64::from(v) * f64::from(s);
            (
                (f(t.x, sx).floor() as i64).max(0),
                (f(t.y, sy).floor() as i64).max(0),
                (f(t.x + t.width as i32, sx).ceil() as i64).min(cw),
                (f(t.y + t.height as i32, sy).ceil() as i64).min(ch),
            )
        } else {
            (
                i64::from(t.x).max(0),
                i64::from(t.y).max(0),
                (i64::from(t.x) + i64::from(t.width)).min(cw),
                (i64::from(t.y) + i64::from(t.height)).min(ch),
            )
        };
        (x1 > x0 && y1 > y0 && t.width > 0 && t.height > 0).then_some([
            x0 as u32,
            y0 as u32,
            (x1 - x0) as u32,
            (y1 - y0) as u32,
        ])
    }

    /// Uploads the visible tiles not uploaded yet.
    fn upload_tiles(
        &mut self,
        queue: &wgpu::Queue,
        tiles: &[std::sync::Arc<crate::text::raster::Tile>],
    ) {
        for t in tiles {
            let key = std::sync::Arc::as_ptr(t) as usize;
            if self.tile_rect(t).is_some() && !self.tiles.textures.contains_key(&key) {
                let entry = self.upload_tile(queue, t);
                self.tiles.textures.insert(key, entry);
            }
        }
    }

    /// One render pass drawing uploaded tiles over the canvas, in order.
    fn tile_pass(
        &self,
        enc: &mut wgpu::CommandEncoder,
        tiles: &[std::sync::Arc<crate::text::raster::Tile>],
    ) {
        let draws: Vec<(usize, [u32; 4])> = tiles
            .iter()
            .filter_map(|t| Some((std::sync::Arc::as_ptr(t) as usize, self.tile_rect(t)?)))
            .filter(|(k, _)| self.tiles.textures.contains_key(k))
            .collect();
        if draws.is_empty() {
            return;
        }
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("text-tiles"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.canvas_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(if self.tile_scale != [1.0, 1.0] {
            &self.tile_scaled_pipe
        } else {
            &self.tile_pipe
        });
        for (key, [x, y, w, h]) in draws {
            pass.set_bind_group(0, &self.tiles.textures[&key].bind, &[]);
            pass.set_scissor_rect(x, y, w, h);
            pass.draw(0..3, 0..1);
        }
    }

    fn upload_tile(
        &self,
        queue: &wgpu::Queue,
        t: &std::sync::Arc<crate::text::raster::Tile>,
    ) -> CachedTile {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("text-tile"),
            size: wgpu::Extent3d {
                width: t.width,
                height: t.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            &t.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(t.width * 4),
                rows_per_image: Some(t.height),
            },
            texture.size(),
        );
        let mut u = Vec::with_capacity(32);
        for v in [t.x, t.y, t.width as i32, t.height as i32] {
            u.extend_from_slice(&v.to_le_bytes());
        }
        for v in [self.tile_scale[0], self.tile_scale[1], 0.0, 0.0] {
            u.extend_from_slice(&v.to_le_bytes());
        }
        let uniform = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("text-tile"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform, 0, &u);
        let view = texture.create_view(&Default::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("text-tile"),
            layout: &self.tile_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
            ],
        });
        CachedTile {
            _tile: std::sync::Arc::clone(t),
            _texture: texture,
            _uniform: uniform,
            bind,
        }
    }
}
