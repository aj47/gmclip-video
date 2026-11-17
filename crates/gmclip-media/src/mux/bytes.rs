//! Big-endian box writing helpers. Every box is written as a 32-bit size placeholder
//! that [`BoxBuf::end`] patches; a box that would outgrow 32 bits is an error, never a
//! silent truncation (only `mdat` uses a largesize header, written by the writer).

use super::MuxError;

/// A growable buffer of ISO BMFF boxes.
#[derive(Debug, Default)]
pub(crate) struct BoxBuf {
    pub(crate) buf: Vec<u8>,
}

/// An open box: where its size field starts.
#[derive(Debug, Clone, Copy)]
#[must_use]
pub(crate) struct Open(usize);

impl BoxBuf {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub(crate) fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub(crate) fn u24(&mut self, v: u32) {
        let b = v.to_be_bytes();
        self.buf.extend_from_slice(&b[1..]);
    }

    pub(crate) fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub(crate) fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub(crate) fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub(crate) fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub(crate) fn bytes(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }

    pub(crate) fn zeros(&mut self, n: usize) {
        self.buf.resize(self.buf.len() + n, 0);
    }

    /// Opens a plain box.
    pub(crate) fn begin(&mut self, fourcc: &[u8; 4]) -> Open {
        let at = self.buf.len();
        self.u32(0);
        self.bytes(fourcc);
        Open(at)
    }

    /// Opens a full box (version + 24-bit flags).
    pub(crate) fn begin_full(&mut self, fourcc: &[u8; 4], version: u8, flags: u32) -> Open {
        let o = self.begin(fourcc);
        self.u8(version);
        self.u24(flags);
        o
    }

    /// Closes a box, patching its 32-bit size.
    pub(crate) fn end(&mut self, o: Open) -> Result<(), MuxError> {
        let size = self
            .buf
            .len()
            .checked_sub(o.0)
            .ok_or(MuxError::Internal("box end"))?;
        let size = u32::try_from(size).map_err(|_| MuxError::BoxTooLarge)?;
        let slot = self
            .buf
            .get_mut(o.0..o.0 + 4)
            .ok_or(MuxError::Internal("box slot"))?;
        slot.copy_from_slice(&size.to_be_bytes());
        Ok(())
    }

    pub(crate) fn into_inner(self) -> Vec<u8> {
        self.buf
    }
}

/// MPEG-4 descriptor length in the fixed 4-byte form (`0x80 0x80 0x80 len`), as
/// ffmpeg and AVFoundation write it; lengths up to 2^28 − 1.
pub(crate) fn descriptor_len(b: &mut BoxBuf, len: usize) -> Result<(), MuxError> {
    if len >= 1 << 28 {
        return Err(MuxError::BoxTooLarge);
    }
    // Truncation is intended: each byte carries 7 bits of `len`.
    #[allow(clippy::cast_possible_truncation)]
    {
        b.u8(0x80 | ((len >> 21) & 0x7f) as u8);
        b.u8(0x80 | ((len >> 14) & 0x7f) as u8);
        b.u8(0x80 | ((len >> 7) & 0x7f) as u8);
        b.u8((len & 0x7f) as u8);
    }
    Ok(())
}
