//! A checked MSB-first bit reader with Exp-Golomb codes (H.264 §7.2, §9.1).

use super::{CodecError, Result};

/// Reads bits from a byte slice; every read past the end is an error.
#[derive(Clone, Debug)]
pub struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }

    /// Bits not yet read.
    pub fn remaining(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.bit)
    }

    /// Bits read so far.
    pub fn position(&self) -> usize {
        self.bit
    }

    fn short(&self, what: &'static str) -> CodecError {
        CodecError::Truncated {
            field: what,
            bit: self.bit,
        }
    }

    /// One bit as a bool.
    pub fn flag(&mut self, what: &'static str) -> Result<bool> {
        Ok(self.bits(1, what)? == 1)
    }

    /// `n` ≤ 32 bits, unsigned.
    pub fn bits(&mut self, n: u32, what: &'static str) -> Result<u32> {
        if n > 32 {
            return Err(CodecError::Invalid {
                field: what,
                reason: "more than 32 bits",
            });
        }
        if (n as usize) > self.remaining() {
            return Err(self.short(what));
        }
        let mut v: u64 = 0;
        for _ in 0..n {
            let byte = self
                .data
                .get(self.bit / 8)
                .copied()
                .ok_or_else(|| self.short(what))?;
            let b = (byte >> (7 - (self.bit % 8))) & 1;
            v = (v << 1) | u64::from(b);
            self.bit += 1;
        }
        u32::try_from(v).map_err(|_| CodecError::Invalid {
            field: what,
            reason: "value",
        })
    }

    /// Skip `n` bits.
    pub fn skip(&mut self, n: usize, what: &'static str) -> Result<()> {
        if n > self.remaining() {
            return Err(self.short(what));
        }
        self.bit += n;
        Ok(())
    }

    /// ue(v): unsigned Exp-Golomb, at most 31 leading zeros (a value below 2^32 − 1).
    pub fn ue(&mut self, what: &'static str) -> Result<u32> {
        let mut zeros = 0u32;
        while !self.flag(what)? {
            zeros += 1;
            if zeros > 31 {
                return Err(CodecError::Invalid {
                    field: what,
                    reason: "Exp-Golomb code longer than 32 bits",
                });
            }
        }
        let rest = self.bits(zeros, what)?;
        let v = (1u64 << zeros) - 1 + u64::from(rest);
        u32::try_from(v).map_err(|_| CodecError::Invalid {
            field: what,
            reason: "Exp-Golomb value",
        })
    }

    /// se(v): signed Exp-Golomb.
    pub fn se(&mut self, what: &'static str) -> Result<i32> {
        let k = i64::from(self.ue(what)?);
        let v = if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) };
        i32::try_from(v).map_err(|_| CodecError::Invalid {
            field: what,
            reason: "Exp-Golomb value",
        })
    }

    /// ue(v) that must not exceed `max` (the syntax's own bound).
    pub fn ue_max(&mut self, max: u32, what: &'static str) -> Result<u32> {
        let v = self.ue(what)?;
        if v > max {
            return Err(CodecError::Invalid {
                field: what,
                reason: "out of range",
            });
        }
        Ok(v)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn exp_golomb() {
        // 1 | 010 | 011 | 011 | 00100 -> ue 0, 1, 2; se -1, 2
        let mut r = BitReader::new(&[0b1010_0110, 0b1100_1000, 0]);
        assert_eq!(r.ue("a").unwrap(), 0);
        assert_eq!(r.ue("b").unwrap(), 1);
        assert_eq!(r.ue("c").unwrap(), 2);
        assert_eq!(r.se("d").unwrap(), -1);
        assert_eq!(r.se("e").unwrap(), 2);
        assert!(r.ue("f").is_err(), "runs out");
        assert!(BitReader::new(&[0; 8]).ue("g").is_err(), "too many zeros");
    }

    #[test]
    fn bits_past_end_fail() {
        let mut r = BitReader::new(&[0xFF]);
        assert_eq!(r.bits(4, "a").unwrap(), 0xF);
        assert!(r.bits(5, "b").is_err());
        assert_eq!(r.bits(4, "c").unwrap(), 0xF);
        assert!(r.flag("d").is_err());
    }
}

/// Builds bitstreams for known-answer tests.
#[cfg(test)]
#[allow(clippy::indexing_slicing)]
#[derive(Default)]
pub(crate) struct BitWriter {
    bits: Vec<bool>,
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
impl BitWriter {
    pub fn put(&mut self, v: u64, n: u32) -> &mut Self {
        for i in (0..n).rev() {
            self.bits.push((v >> i) & 1 == 1);
        }
        self
    }
    pub fn flag(&mut self, b: bool) -> &mut Self {
        self.put(u64::from(b), 1)
    }
    pub fn ue(&mut self, v: u32) -> &mut Self {
        let x = u64::from(v) + 1;
        let len = 64 - x.leading_zeros();
        self.put(0, len - 1).put(x, len)
    }
    pub fn se(&mut self, v: i32) -> &mut Self {
        let k = if v > 0 {
            2 * v as u32 - 1
        } else {
            (-2 * v) as u32
        };
        self.ue(k)
    }
    /// rbsp_stop_one_bit and alignment.
    pub fn bytes(&self) -> Vec<u8> {
        let mut b = self.bits.clone();
        b.push(true);
        while !b.len().is_multiple_of(8) {
            b.push(false);
        }
        b.chunks(8)
            .map(|c| c.iter().fold(0u8, |a, &x| (a << 1) | u8::from(x)))
            .collect()
    }
}
