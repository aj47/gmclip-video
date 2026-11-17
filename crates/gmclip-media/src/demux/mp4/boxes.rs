//! ISO BMFF box headers and the `moov` tree.

use super::super::io::Cursor;
use super::super::{DemuxError, MAX_BOX_DEPTH, Result};

/// Containers the index does not need: a malformed one is kept as a leaf, not an
/// error (players ignore damaged user data). Their nesting still counts toward
/// [`MAX_BOX_DEPTH`] while they parse.
const OPTIONAL: &[&[u8; 4]] = &[
    b"udta", b"tref", b"sinf", b"schi", b"rinf", b"gmhd", b"mfra",
];

/// Boxes whose payload is a sequence of boxes (ISO/IEC 14496-12 and QuickTime).
const CONTAINERS: &[&[u8; 4]] = &[
    b"moov", b"trak", b"edts", b"mdia", b"minf", b"dinf", b"stbl", b"udta", b"mvex", b"moof",
    b"traf", b"mfra", b"tref", b"sinf", b"schi", b"rinf", b"gmhd",
];

/// A parsed box: its type, where its payload starts in the file, the payload, and its
/// children when it is a container.
#[derive(Clone, Debug)]
pub struct Node<'a> {
    pub kind: [u8; 4],
    /// File offset of the box header.
    pub offset: u64,
    pub payload: &'a [u8],
    /// File offset of the payload.
    pub payload_offset: u64,
    pub children: Vec<Node<'a>>,
}

impl<'a> Node<'a> {
    /// First child of a kind.
    pub fn child(&self, kind: &[u8; 4]) -> Option<&Node<'a>> {
        self.children.iter().find(|c| &c.kind == kind)
    }
    /// All children of a kind.
    pub fn children_of<'s>(&'s self, kind: &'s [u8; 4]) -> impl Iterator<Item = &'s Node<'a>> + 's {
        self.children.iter().filter(move |c| &c.kind == kind)
    }
    /// A cursor over the payload.
    pub fn cursor(&self) -> Cursor<'a> {
        Cursor::new(self.payload, self.payload_offset)
    }
    /// Follow a path of child kinds.
    pub fn path(&self, kinds: &[&[u8; 4]]) -> Option<&Node<'a>> {
        let mut n = self;
        for k in kinds {
            n = n.child(k)?;
        }
        Some(n)
    }
}

/// Printable four-character code.
pub fn fourcc(k: &[u8; 4]) -> String {
    k.iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '?'
            }
        })
        .collect()
}

/// Read one box header from a cursor: `(kind, header_len, total_size)`. A size of 0
/// means "to the end of the enclosing payload".
pub fn header(c: &mut Cursor<'_>) -> Result<([u8; 4], usize, u64)> {
    let at = c.offset();
    let size32 = c.u32()?;
    let kind = c.fourcc()?;
    let (head, size) = match size32 {
        0 => (8usize, (c.remaining() as u64) + 8),
        1 => {
            let large = c.u64()?;
            (16, large)
        }
        n => (8, u64::from(n)),
    };
    if size < head as u64 {
        return Err(DemuxError::invalid(
            format!("box '{}' size {size} below its header", fourcc(&kind)),
            at,
        ));
    }
    Ok((kind, head, size))
}

/// Parse a payload into its child boxes, descending into containers up to
/// [`MAX_BOX_DEPTH`]. `budget` counts boxes, bounding work by the bytes present.
pub fn parse_children<'a>(
    data: &'a [u8],
    base: u64,
    depth: u32,
    budget: &mut u64,
) -> Result<Vec<Node<'a>>> {
    if depth > MAX_BOX_DEPTH {
        return Err(DemuxError::budget(
            format!("boxes nested deeper than {MAX_BOX_DEPTH}"),
            base,
        ));
    }
    let mut c = Cursor::new(data, base);
    let mut out = Vec::new();
    while c.remaining() >= 8 {
        let offset = c.offset();
        let (kind, head, size) = header(&mut c)?;
        let body = size - head as u64;
        let body = usize::try_from(body)
            .map_err(|_| DemuxError::invalid("box larger than memory", offset))?;
        if body > c.remaining() {
            return Err(DemuxError::invalid(
                format!(
                    "box '{}' ({size} bytes) runs past its parent",
                    fourcc(&kind)
                ),
                offset,
            ));
        }
        *budget = budget
            .checked_sub(1)
            .ok_or_else(|| DemuxError::budget("too many boxes", offset))?;
        let payload_offset = c.offset();
        let payload = c.bytes(body)?;
        let children = if CONTAINERS.contains(&&kind) {
            match parse_children(payload, payload_offset, depth + 1, budget) {
                Ok(c) => c,
                Err(e) if OPTIONAL.contains(&&kind) && !is_budget(&e) => Vec::new(),
                Err(e) => return Err(e),
            }
        } else {
            Vec::new()
        };
        out.try_reserve(1)
            .map_err(|_| DemuxError::budget("box tree allocation", offset))?;
        out.push(Node {
            kind,
            offset,
            payload,
            payload_offset,
            children,
        });
    }
    // Up to 7 trailing bytes are tolerated (some muxers pad `udta`), as players do.
    Ok(out)
}

fn is_budget(e: &DemuxError) -> bool {
    matches!(e, DemuxError::Invalid { reason, .. } if reason.starts_with("budget"))
}

/// One level of boxes, no recursion, stopping quietly at the first malformed header.
/// For optional children (sample-entry extensions), where players ignore trailing junk.
pub fn flat<'a>(data: &'a [u8], base: u64) -> Vec<Node<'a>> {
    let mut c = Cursor::new(data, base);
    let mut out = Vec::new();
    while c.remaining() >= 8 {
        let offset = c.offset();
        let Ok((kind, head, size)) = header(&mut c) else {
            break;
        };
        let Ok(body) = usize::try_from(size - head as u64) else {
            break;
        };
        let payload_offset = c.offset();
        let Ok(payload) = c.bytes(body) else { break };
        out.push(Node {
            kind,
            offset,
            payload,
            payload_offset,
            children: Vec::new(),
        });
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn nests_and_rejects_overruns() {
        let inner = bx(b"mdhd", &[0; 4]);
        let trak = bx(b"trak", &inner);
        let mut budget = 100;
        let nodes = parse_children(&trak, 0, 1, &mut budget).unwrap_or_default();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes.first().map(|n| n.children.len()), Some(1));
        let mut bad = trak.clone();
        bad[3] = 200;
        assert!(parse_children(&bad, 0, 1, &mut budget).is_err());
    }

    #[test]
    fn deep_nesting_is_invalid() {
        let mut b = bx(b"free", &[]);
        for _ in 0..64 {
            b = bx(b"moov", &b);
        }
        let mut budget = 1000;
        let e = parse_children(&b, 0, 0, &mut budget).unwrap_err();
        assert!(
            matches!(e, DemuxError::Invalid { ref reason, .. } if reason.starts_with("budget")),
            "{e}"
        );
    }

    #[test]
    fn largesize_is_read() {
        let mut v = 1u32.to_be_bytes().to_vec();
        v.extend_from_slice(b"free");
        v.extend_from_slice(&20u64.to_be_bytes());
        v.extend_from_slice(&[0; 4]);
        let mut budget = 10;
        let nodes = parse_children(&v, 0, 0, &mut budget).unwrap_or_default();
        assert_eq!(nodes.first().map(|n| n.payload.len()), Some(4));
    }
}
