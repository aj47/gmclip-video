//! EBML primitives: variable-length IDs and sizes, element values, and an in-memory
//! element tree for the small metadata elements (Info, Tracks, Cues, SeekHead).

use super::super::io::Cursor;
use super::super::{DemuxError, MAX_EBML_DEPTH, Result};

/// An element size of all ones: "unknown" (allowed for Segment and Cluster only).
pub const UNKNOWN: u64 = u64::MAX;

/// Length in bytes of a VINT from its first byte (1..=8), or None for 0x00.
pub fn vint_len(first: u8) -> Option<usize> {
    if first == 0 {
        None
    } else {
        Some(first.leading_zeros() as usize + 1)
    }
}

/// Read an element ID (1–4 bytes, marker bit kept).
pub fn read_id(c: &mut Cursor<'_>) -> Result<u32> {
    let at = c.offset();
    let first = c.u8()?;
    let n = vint_len(first)
        .filter(|&n| n <= 4)
        .ok_or_else(|| DemuxError::invalid("bad EBML ID", at))?;
    let mut v = u32::from(first);
    for _ in 1..n {
        v = (v << 8) | u32::from(c.u8()?);
    }
    // An ID whose value bits are all ones (or all zeros) is reserved.
    let bits = 7 * n as u32;
    let value = v & ((1u32 << bits) - 1);
    if value == 0 || value == (1u32 << bits) - 1 {
        return Err(DemuxError::invalid(format!("reserved EBML ID {v:#x}"), at));
    }
    Ok(v)
}

/// Read an element size (1–8 bytes, marker bit removed); all ones = [`UNKNOWN`].
pub fn read_size(c: &mut Cursor<'_>) -> Result<u64> {
    let at = c.offset();
    let first = c.u8()?;
    let n = vint_len(first).ok_or_else(|| DemuxError::invalid("bad EBML size", at))?;
    let mask = 0xFFu64 >> n;
    let mut v = u64::from(first) & mask;
    let mut all_ones = v == mask;
    for _ in 1..n {
        let b = c.u8()?;
        all_ones &= b == 0xFF;
        v = (v << 8) | u64::from(b);
    }
    Ok(if all_ones { UNKNOWN } else { v })
}

/// Unsigned integer value (0–8 bytes).
pub fn uint(data: &[u8], at: u64) -> Result<u64> {
    if data.len() > 8 {
        return Err(DemuxError::invalid(
            "integer element longer than 8 bytes",
            at,
        ));
    }
    Ok(data.iter().fold(0u64, |v, &b| (v << 8) | u64::from(b)))
}

/// Signed integer value (0–8 bytes).
pub fn sint(data: &[u8], at: u64) -> Result<i64> {
    let u = uint(data, at)?;
    let bits = 8 * data.len() as u32;
    Ok(if bits == 0 || bits == 64 {
        u as i64
    } else {
        ((u << (64 - bits)) as i64) >> (64 - bits)
    })
}

/// Float value (0, 4 or 8 bytes).
pub fn float(data: &[u8], at: u64) -> Result<f64> {
    match data.len() {
        0 => Ok(0.0),
        4 => Ok(f64::from(f32::from_bits(uint(data, at)? as u32))),
        8 => Ok(f64::from_bits(uint(data, at)?)),
        n => Err(DemuxError::invalid(format!("{n}-byte float element"), at)),
    }
}

/// An element parsed in memory.
#[derive(Clone, Debug)]
pub struct Element<'a> {
    pub id: u32,
    /// File offset of the element's ID.
    pub offset: u64,
    pub data: &'a [u8],
    pub data_offset: u64,
    pub children: Vec<Element<'a>>,
}

impl<'a> Element<'a> {
    pub fn child(&self, id: u32) -> Option<&Element<'a>> {
        self.children.iter().find(|c| c.id == id)
    }
    pub fn children_of(&self, id: u32) -> impl Iterator<Item = &Element<'a>> + '_ {
        self.children.iter().filter(move |c| c.id == id)
    }
    pub fn uint(&self) -> Result<u64> {
        uint(self.data, self.data_offset)
    }
    pub fn float(&self) -> Result<f64> {
        float(self.data, self.data_offset)
    }
    pub fn string(&self) -> String {
        let end = self
            .data
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.data.len());
        String::from_utf8_lossy(self.data.get(..end).unwrap_or_default()).into_owned()
    }
    /// A child's unsigned value, or `default` when absent.
    pub fn uint_of(&self, id: u32, default: u64) -> Result<u64> {
        self.child(id).map_or(Ok(default), Element::uint)
    }
}

/// Parse a sequence of elements, recursing into the master IDs in `masters` up to
/// [`MAX_EBML_DEPTH`]. `budget` counts elements.
pub fn parse<'a>(
    data: &'a [u8],
    base: u64,
    masters: &[u32],
    depth: u32,
    budget: &mut u64,
) -> Result<Vec<Element<'a>>> {
    if depth > MAX_EBML_DEPTH {
        return Err(DemuxError::budget(
            format!("EBML nested deeper than {MAX_EBML_DEPTH}"),
            base,
        ));
    }
    let mut c = Cursor::new(data, base);
    let mut out = Vec::new();
    while !c.is_empty() {
        let offset = c.offset();
        let id = read_id(&mut c)?;
        let size = read_size(&mut c)?;
        let len = if size == UNKNOWN {
            c.remaining()
        } else {
            usize::try_from(size).map_err(|_| DemuxError::invalid("element too large", offset))?
        };
        if len > c.remaining() {
            return Err(DemuxError::invalid(
                format!("element {id:#x} ({len} bytes) runs past its parent"),
                offset,
            ));
        }
        *budget = budget
            .checked_sub(1)
            .ok_or_else(|| DemuxError::budget("too many EBML elements", offset))?;
        let data_offset = c.offset();
        let body = c.bytes(len)?;
        let children = if masters.contains(&id) {
            parse(body, data_offset, masters, depth + 1, budget)?
        } else {
            Vec::new()
        };
        out.push(Element {
            id,
            offset,
            data: body,
            data_offset,
            children,
        });
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn vints() {
        let mut c = Cursor::new(&[0x1A, 0x45, 0xDF, 0xA3, 0x81, 0x01, 0x40, 0x02, 0xFF], 0);
        assert_eq!(read_id(&mut c).unwrap(), 0x1A45_DFA3);
        assert_eq!(read_size(&mut c).unwrap(), 1);
        c.skip(1).unwrap();
        assert_eq!(read_size(&mut c).unwrap(), 2);
        assert_eq!(read_size(&mut c).unwrap(), UNKNOWN);
        assert!(read_id(&mut Cursor::new(&[0x00], 0)).is_err());
        assert!(
            read_id(&mut Cursor::new(&[0x08, 1, 2, 3, 4], 0)).is_err(),
            "5-byte ID"
        );
        assert!(read_id(&mut Cursor::new(&[0xFF], 0)).is_err(), "reserved");
        assert!(read_size(&mut Cursor::new(&[0x00], 0)).is_err());
    }

    #[test]
    fn values() {
        assert_eq!(sint(&[0xFF, 0xEB], 0).unwrap(), -21);
        assert_eq!(uint(&[1, 0], 0).unwrap(), 256);
        assert_eq!(float(&[0x47, 0x3B, 0x80, 0x00], 0).unwrap(), 48000.0);
        assert!(uint(&[0; 9], 0).is_err());
    }

    #[test]
    fn deep_nesting_is_invalid() {
        // 1000 nested 0xAE (TrackEntry) elements, sizes as 8-byte VINTs.
        let mut v: Vec<u8> = Vec::new();
        for _ in 0..1000 {
            let mut e = vec![0xAE, 0x01];
            e.extend_from_slice(&(v.len() as u64).to_be_bytes()[1..]);
            e.extend_from_slice(&v);
            v = e;
        }
        let mut budget = 1 << 20;
        let r = parse(&v, 0, &[0xAE], 0, &mut budget);
        assert!(
            matches!(r, Err(DemuxError::Invalid { ref reason, .. }) if reason.starts_with("budget"))
        );
    }
}
