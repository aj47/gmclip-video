//! Byte sources and a bounds-checked cursor for the demuxers.

use std::fs::File;
use std::path::Path;

use super::{DemuxError, Result};

/// Random-access bytes: a file, or a slice in tests and fuzz targets.
pub trait ByteSource {
    /// Total length in bytes.
    fn len(&self) -> u64;
    /// Fill `buf` from `offset`; reading past the end is an error.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()>;
    /// True when the source is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An in-memory source.
#[derive(Clone, Copy, Debug)]
pub struct SliceSource<'a>(pub &'a [u8]);

impl ByteSource for SliceSource<'_> {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let start =
            usize::try_from(offset).map_err(|_| DemuxError::invalid("read past end", offset))?;
        let end = start
            .checked_add(buf.len())
            .ok_or_else(|| DemuxError::invalid("read past end", offset))?;
        let src = self
            .0
            .get(start..end)
            .ok_or_else(|| DemuxError::invalid("read past end of file", offset))?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

/// A file read with positioned reads. With tracing on, every read's `(offset, len)` is
/// recorded so tests can prove which bytes the index touched (B-01: no `mdat` reads).
#[derive(Debug)]
pub struct FileSource {
    file: File,
    len: u64,
    trace: Option<Vec<(u64, usize)>>,
}

impl FileSource {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|e| DemuxError::Io {
            offset: 0,
            message: e.to_string(),
        })?;
        let len = file
            .metadata()
            .map_err(|e| DemuxError::Io {
                offset: 0,
                message: e.to_string(),
            })?
            .len();
        Ok(Self {
            file,
            len,
            trace: None,
        })
    }
    /// Record every read from now on.
    pub fn with_trace(mut self) -> Self {
        self.trace = Some(Vec::new());
        self
    }
    /// The reads recorded so far.
    pub fn trace(&self) -> &[(u64, usize)] {
        self.trace.as_deref().unwrap_or_default()
    }
}

impl ByteSource for FileSource {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let end = offset.checked_add(buf.len() as u64);
        if end.is_none_or(|e| e > self.len) {
            return Err(DemuxError::invalid("read past end of file", offset));
        }
        if let Some(t) = &mut self.trace {
            t.push((offset, buf.len()));
        }
        read_exact_at(&self.file, buf, offset).map_err(|e| DemuxError::Io {
            offset,
            message: e.to_string(),
        })
    }
}

#[cfg(unix)]
fn read_exact_at(f: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(f, buf, offset)
}

#[cfg(windows)]
fn read_exact_at(f: &File, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match f.seek_read(buf, offset) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = buf.get_mut(n..).unwrap_or_default();
                offset += n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// `Vec::with_capacity` that fails with `Invalid` instead of aborting when the
/// allocator refuses (R-MEDIA-SEC-2: allocations sized from a file use `try_reserve`).
pub fn vec_with_capacity<T>(n: usize, offset: u64) -> Result<Vec<T>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n)
        .map_err(|_| DemuxError::budget(format!("cannot allocate {n} entries"), offset))?;
    Ok(v)
}

/// Read `len` bytes at `offset` into a fresh buffer, checked against the source length
/// and [`super::MAX_METADATA_BYTES`].
pub fn read_vec<S: ByteSource + ?Sized>(src: &mut S, offset: u64, len: u64) -> Result<Vec<u8>> {
    if len > super::MAX_METADATA_BYTES {
        return Err(DemuxError::budget(
            format!("{len}-byte metadata element"),
            offset,
        ));
    }
    if offset.checked_add(len).is_none_or(|e| e > src.len()) {
        return Err(DemuxError::invalid(
            format!("{len}-byte element runs past the end of the file"),
            offset,
        ));
    }
    let n = usize::try_from(len).map_err(|_| DemuxError::budget("element too large", offset))?;
    let mut v: Vec<u8> = vec_with_capacity(n, offset)?;
    v.resize(n, 0);
    src.read_at(offset, &mut v)?;
    Ok(v)
}

/// A bounds-checked big-endian reader over an in-memory buffer. `base` is the buffer's
/// offset in the file, so errors name real file positions.
#[derive(Clone, Debug)]
pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
    base: u64,
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8], base: u64) -> Self {
        Self { data, pos: 0, base }
    }
    /// File offset of the next byte.
    pub fn offset(&self) -> u64 {
        self.base + self.pos as u64
    }
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
    pub fn position(&self) -> usize {
        self.pos
    }
    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }
    fn short(&self, n: usize) -> DemuxError {
        DemuxError::invalid(
            format!("needs {n} bytes, {} remain", self.remaining()),
            self.offset(),
        )
    }
    /// The next `n` bytes, advancing past them.
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or_else(|| self.short(n))?;
        let s = self.data.get(self.pos..end).ok_or_else(|| self.short(n))?;
        self.pos = end;
        Ok(s)
    }
    /// The next `n` bytes as a sub-cursor.
    pub fn sub(&mut self, n: usize) -> Result<Cursor<'a>> {
        let base = self.offset();
        Ok(Cursor::new(self.bytes(n)?, base))
    }
    /// The rest of the buffer.
    pub fn rest(&mut self) -> &'a [u8] {
        let s = self.data.get(self.pos..).unwrap_or_default();
        self.pos = self.data.len();
        s
    }
    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.bytes(n).map(|_| ())
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.bytes(N)?);
        Ok(a)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }
    pub fn u24(&mut self) -> Result<u32> {
        let [a, b, c] = self.array::<3>()?;
        Ok(u32::from_be_bytes([0, a, b, c]))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.array()?))
    }
    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.array()?))
    }
    pub fn fourcc(&mut self) -> Result<[u8; 4]> {
        self.array()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn cursor_never_reads_past_its_buffer() {
        let mut c = Cursor::new(&[1, 2, 3], 100);
        assert_eq!(c.u16().ok(), Some(0x0102));
        assert_eq!(c.offset(), 102);
        assert!(matches!(
            c.u16(),
            Err(DemuxError::Invalid { offset: 102, .. })
        ));
        assert_eq!(c.u8().ok(), Some(3));
        assert!(c.bytes(usize::MAX).is_err());
        assert!(c.is_empty());
    }

    #[test]
    fn slice_source_rejects_out_of_range() {
        let data = [0u8; 4];
        let mut s = SliceSource(&data);
        let mut b = [0u8; 2];
        assert!(s.read_at(3, &mut b).is_err());
        assert!(s.read_at(u64::MAX, &mut b).is_err());
        assert!(s.read_at(2, &mut b).is_ok());
    }

    #[test]
    fn read_vec_checks_length_before_allocating() {
        let data = [0u8; 16];
        let mut s = SliceSource(&data);
        assert!(read_vec(&mut s, 8, 1 << 40).is_err());
        assert!(read_vec(&mut s, 8, 9).is_err());
        assert_eq!(read_vec(&mut s, 8, 8).map(|v| v.len()).ok(), Some(8));
    }
}
