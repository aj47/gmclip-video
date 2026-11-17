//! Callouts (`/1` shape events, decision 0014; SPEC §5.9): a box or an arrow on a screen
//! region, as polygons rasterised by the text raster's vello_cpu path (single-threaded,
//! fallback level: the same bytes on every machine) into a premultiplied tile.
//!
//! - **box**: a stroke ring (the outer rounded rectangle and the inner one wound the
//!   other way, so a non-zero fill leaves the inside open) centred on `rect`'s edge, and an
//!   optional fill inside the stroke;
//! - **arrow**: one path — a shaft of `width` from `from` to the base of a head
//!   `head` long and `2.2 × width` wide at `to` — so a translucent stroke stays even where
//!   shaft and head meet.

use gmclip_plan::types::Shape;

use crate::text::Colour;
use crate::text::raster::{Contour, Geometry, RasterError, Tile, rounded_rect};

fn colour(c: Option<&gmclip_plan::scalars::Rgba>) -> Option<Colour> {
    c.map(Colour::from_plan)
}

fn reversed(mut c: Contour) -> Contour {
    c.reverse();
    c
}

/// The shape's geometry in canvas px.
pub fn geometry(shape: &Shape) -> Geometry {
    let mut g = Geometry::default();
    match shape {
        Shape::Box {
            rect,
            width,
            stroke,
            fill,
            radius,
        } => {
            let r = radius.unwrap_or(0.0).max(0.0);
            let (x0, y0, x1, y1) = (rect.x, rect.y, rect.x + rect.w, rect.y + rect.h);
            g.anchor = [x0, y0];
            let half = width / 2.0;
            if let Some(f) = colour(fill.as_ref()) {
                g.fills.push((
                    f,
                    vec![rounded_rect(
                        x0 + half,
                        y0 + half,
                        x1 - half,
                        y1 - half,
                        (r - half).max(0.0),
                    )],
                ));
            }
            if let Some(s) = colour(stroke.as_ref()).filter(|_| *width > 0.0) {
                let outer = rounded_rect(x0 - half, y0 - half, x1 + half, y1 + half, r + half);
                let inner = rounded_rect(
                    x0 + half,
                    y0 + half,
                    x1 - half,
                    y1 - half,
                    (r - half).max(0.0),
                );
                let ring = if x1 - x0 > *width && y1 - y0 > *width {
                    vec![outer, reversed(inner)]
                } else {
                    vec![outer]
                };
                g.fills.push((s, ring));
            }
        }
        Shape::Arrow {
            from,
            to,
            width,
            stroke,
            head,
        } => {
            g.anchor = [from.x, from.y];
            let (dx, dy) = (to.x - from.x, to.y - from.y);
            let len = (dx * dx + dy * dy).sqrt();
            if len < 1e-6 {
                return g;
            }
            let (ux, uy) = (dx / len, dy / len);
            let (nx, ny) = (-uy, ux);
            let hl = head.unwrap_or(3.0 * width).min(len);
            let hw = width * 2.2 / 2.0 * (hl / (3.0 * width)).clamp(0.6, 1.6);
            let w2 = width / 2.0;
            let bx = to.x - ux * hl;
            let by = to.y - uy * hl;
            // one outline, clockwise: shaft left edge, head, shaft right edge
            let poly = vec![
                [from.x + nx * w2, from.y + ny * w2],
                [bx + nx * w2, by + ny * w2],
                [bx + nx * hw.max(w2), by + ny * hw.max(w2)],
                [to.x, to.y],
                [bx - nx * hw.max(w2), by - ny * hw.max(w2)],
                [bx - nx * w2, by - ny * w2],
                [from.x - nx * w2, from.y - ny * w2],
            ];
            g.fills.push((Colour::from_plan(stroke), vec![poly]));
        }
    }
    g
}

/// The shape as a premultiplied tile.
pub fn render(shape: &Shape) -> Result<Tile, RasterError> {
    geometry(shape).rasterise()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmclip_plan::scalars::Rgba;
    use gmclip_plan::types::{Point, Rect};

    fn alpha_at(t: &Tile, x: i32, y: i32) -> u8 {
        let (i, j) = (x - t.x, y - t.y);
        if i < 0 || j < 0 || i >= t.width as i32 || j >= t.height as i32 {
            return 0;
        }
        t.rgba[((j as u32 * t.width + i as u32) * 4 + 3) as usize]
    }

    #[test]
    fn a_box_is_a_ring_and_an_arrow_points() {
        let b = render(&Shape::Box {
            rect: Rect {
                x: 100.0,
                y: 100.0,
                w: 200.0,
                h: 100.0,
            },
            width: 8.0,
            stroke: Some(Rgba::new("#FF3B30FF").unwrap()),
            fill: None,
            radius: Some(12.0),
        })
        .unwrap();
        assert_eq!(alpha_at(&b, 200, 100), 255, "on the stroke");
        assert_eq!(alpha_at(&b, 200, 150), 0, "the inside stays open");
        assert_eq!(alpha_at(&b, 200, 92), 0, "outside the stroke");
        let a = render(&Shape::Arrow {
            from: Point { x: 700.0, y: 900.0 },
            to: Point { x: 500.0, y: 500.0 },
            width: 10.0,
            stroke: Rgba::new("#FFD400FF").unwrap(),
            head: None,
        })
        .unwrap();
        assert_eq!(alpha_at(&a, 600, 700), 255, "on the shaft");
        assert!(alpha_at(&a, 503, 507) > 0, "near the tip");
        assert_eq!(alpha_at(&a, 650, 600), 0);
    }
}
