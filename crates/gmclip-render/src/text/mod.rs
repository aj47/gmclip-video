//! Text events, styles and their raster (SPEC §5.9, §7 Text, §8; RP §2.1 rule 6).
//!
//! Events and styles are RenderPlan v0's own types (`gmclip_plan::types`, re-exported here).
//! Nothing here refits or wraps text (rule 6: `size` is final).
//!
//! - [`ass`]: the ASS-subset importer (B-07).
//! - [`font`]: font metrics and the per-font em rule (SPEC Conventions).
//! - [`raster`]: parley + vello_cpu cue raster (B-09).

pub mod ass;
pub mod font;
pub mod raster;

use std::fmt;

pub use gmclip_plan::scalars::{Id, Range};
pub use gmclip_plan::types::{
    Outline, Point, Run, Shadow, ShadowOf, TextBox, TextEvent, TextRole, TextStyle,
};

/// A straight (not premultiplied) RGBA8 colour; RenderPlan writes it `#RRGGBBAA`
/// ([`gmclip_plan::scalars::Rgba`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Colour(pub [u8; 4]);

impl Colour {
    /// Opaque white (libass's default PrimaryColour; the reference caption fill).
    pub const WHITE: Self = Self([0xff, 0xff, 0xff, 0xff]);
    /// Opaque black (libass's default Outline/BackColour; the reference's outline and shadow).
    pub const BLACK: Self = Self([0, 0, 0, 0xff]);
    /// The reference's keyword yellow (`{Y}`, `&H00D4FF&`).
    pub const YELLOW: Self = Self([0xff, 0xd4, 0x00, 0xff]);

    /// Parses `#RRGGBBAA`.
    pub fn parse_hex(s: &str) -> Option<Self> {
        let h = s.strip_prefix('#')?;
        if h.len() != 8 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
        Some(Self([byte(0)?, byte(2)?, byte(4)?, byte(6)?]))
    }

    /// `#RRGGBBAA`, upper case as RP §2.3 writes it.
    pub fn to_hex(self) -> String {
        let [r, g, b, a] = self.0;
        format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
    }

    /// From a plan colour (validated by its pattern, so always parses).
    pub fn from_plan(c: &gmclip_plan::scalars::Rgba) -> Self {
        Self::parse_hex(c.as_str()).unwrap_or(Self::WHITE)
    }

    /// To a plan colour.
    pub fn to_plan(self) -> gmclip_plan::scalars::Rgba {
        gmclip_plan::scalars::Rgba::new(self.to_hex()).expect("#RRGGBBAA matches the rgba pattern")
    }
}

impl fmt::Debug for Colour {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// An event's text with runs concatenated (`\n` between lines).
pub fn event_text(ev: &TextEvent) -> String {
    ev.runs.iter().map(|r| r.text.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::Colour;

    #[test]
    fn colour_round_trip() {
        let c = Colour::parse_hex("#101010CF").unwrap();
        assert_eq!(c, Colour([0x10, 0x10, 0x10, 0xcf]));
        assert_eq!(Colour::from_plan(&c.to_plan()), c);
        assert!(Colour::parse_hex("#12345").is_none());
    }
}
