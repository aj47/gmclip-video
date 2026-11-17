//! Order keys (SPEC §3.2): every collection whose elements carry ids is ordered by a
//! fractional index, so a reorder or insert writes one entity, never its siblings.
//!
//! The format is the "fractional indexing" scheme (D. Greenspan, as in rocicorp's
//! `fractional-indexing`), base 62 (`0-9A-Za-z`, whose ASCII order is the digit order):
//! a key is an **integer part** whose head character encodes its length (`a`..`z` = 2..27
//! characters for non-negative integers, `Z`..`A` = 2..27 for negative ones), then an
//! optional **fraction** that never ends in `0`. Byte order of the strings is the order
//! of the keys. Appending or prepending increments or decrements the integer part, so
//! keys grow logarithmically with list length (10⁵ appends stay ≤ 5 bytes); only
//! repeated inserts into the *same* gap grow the fraction (one byte per ~6 inserts).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const DIGITS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
/// The smallest integer part, `A` + 26 zeros: it cannot be decremented.
const SMALLEST_INTEGER: &str = "A00000000000000000000000000";
const INTEGER_ZERO: &str = "a0";

/// A fractional-index position. Always valid: built only by [`OrderKey::parse`],
/// [`OrderKey::between`] or deserialisation, which goes through `parse`.
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd, Hash, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OrderKey(String);

impl JsonSchema for OrderKey {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "OrderKey".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^[a-zA-Z][0-9A-Za-z]*$",
            "maxLength": 4096,
            "description": "Fractional index (base 62)"
        })
    }
}

impl TryFrom<String> for OrderKey {
    type Error = OrderKeyError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<OrderKey> for String {
    fn from(k: OrderKey) -> Self {
        k.0
    }
}

/// Why a string is not an order key.
#[derive(Clone, Copy, Eq, PartialEq, Debug, thiserror::Error)]
pub enum OrderKeyError {
    #[error("order key is empty")]
    Empty,
    #[error("order key has a byte outside 0-9A-Za-z")]
    BadDigit,
    #[error("order key's head is not a letter, or its integer part is short")]
    BadInteger,
    #[error("order key's fraction ends in '0'")]
    TrailingZero,
    #[error("order key is the smallest integer, which has no predecessor")]
    Smallest,
    #[error("order key is longer than 4096 bytes")]
    TooLong,
    #[error("lower bound is not below upper bound")]
    NotIncreasing,
    #[error("no key exists beyond the largest integer")]
    Exhausted,
}

fn digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'Z' => Some(b - b'A' + 10),
        b'a'..=b'z' => Some(b - b'a' + 36),
        _ => None,
    }
}

fn integer_len(head: u8) -> Option<usize> {
    match head {
        b'a'..=b'z' => Some(usize::from(head - b'a') + 2),
        b'A'..=b'Z' => Some(usize::from(b'Z' - head) + 2),
        _ => None,
    }
}

fn integer_part(key: &[u8]) -> Result<&[u8], OrderKeyError> {
    let n =
        integer_len(*key.first().ok_or(OrderKeyError::Empty)?).ok_or(OrderKeyError::BadInteger)?;
    key.get(..n).ok_or(OrderKeyError::BadInteger)
}

impl OrderKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parses and validates a key.
    pub fn parse(s: &str) -> Result<Self, OrderKeyError> {
        let b = s.as_bytes();
        if b.is_empty() {
            return Err(OrderKeyError::Empty);
        }
        if b.len() > 4096 {
            return Err(OrderKeyError::TooLong);
        }
        if b.iter().any(|&c| digit(c).is_none()) {
            return Err(OrderKeyError::BadDigit);
        }
        if s == SMALLEST_INTEGER {
            return Err(OrderKeyError::Smallest);
        }
        let int = integer_part(b)?;
        if b.len() > int.len() && b.last() == Some(&b'0') {
            return Err(OrderKeyError::TrailingZero);
        }
        Ok(Self(s.to_owned()))
    }

    /// The first key of an empty list.
    pub fn first() -> Self {
        Self(INTEGER_ZERO.to_owned())
    }

    /// A key strictly between `lo` and `hi` (`None` = the open end of the list).
    pub fn between(lo: Option<&OrderKey>, hi: Option<&OrderKey>) -> Result<Self, OrderKeyError> {
        // Never hand out a key `parse` would refuse (a stored op could not decode).
        let key = |s: Vec<u8>| -> Result<Self, OrderKeyError> {
            if s.len() > 4096 {
                return Err(OrderKeyError::TooLong);
            }
            Ok(Self(
                String::from_utf8(s).map_err(|_| OrderKeyError::BadDigit)?,
            ))
        };
        match (lo, hi) {
            (None, None) => Ok(Self::first()),
            (None, Some(b)) => {
                let b = b.0.as_bytes();
                let ib = integer_part(b)?;
                let fb = &b[ib.len()..];
                if ib == SMALLEST_INTEGER.as_bytes() {
                    let mut out = ib.to_vec();
                    out.extend(midpoint(b"", Some(fb)));
                    return key(out);
                }
                if ib.len() < b.len() {
                    // b has a fraction, so its bare integer part sorts before it.
                    return key(ib.to_vec());
                }
                key(decrement(ib).ok_or(OrderKeyError::Exhausted)?)
            }
            (Some(a), None) => {
                let a = a.0.as_bytes();
                let ia = integer_part(a)?;
                let fa = &a[ia.len()..];
                match increment(ia) {
                    Some(i) => key(i),
                    None => {
                        let mut out = ia.to_vec();
                        out.extend(midpoint(fa, None));
                        key(out)
                    }
                }
            }
            (Some(a), Some(b)) => {
                if a >= b {
                    return Err(OrderKeyError::NotIncreasing);
                }
                let (a, b) = (a.0.as_bytes(), b.0.as_bytes());
                let (ia, ib) = (integer_part(a)?, integer_part(b)?);
                let (fa, fb) = (&a[ia.len()..], &b[ib.len()..]);
                if ia == ib {
                    let mut out = ia.to_vec();
                    out.extend(midpoint(fa, Some(fb)));
                    return key(out);
                }
                let i = increment(ia).ok_or(OrderKeyError::Exhausted)?;
                if i.as_slice() < b {
                    return key(i);
                }
                let mut out = ia.to_vec();
                out.extend(midpoint(fa, None));
                key(out)
            }
        }
    }

    /// `n` keys spread evenly after `lo` (used when seeding a list, e.g. an import).
    pub fn sequence(lo: Option<&OrderKey>, n: usize) -> Result<Vec<Self>, OrderKeyError> {
        let mut out: Vec<OrderKey> = Vec::with_capacity(n);
        let mut prev = lo.cloned();
        for _ in 0..n {
            let k = Self::between(prev.as_ref(), None)?;
            prev = Some(k.clone());
            out.push(k);
        }
        Ok(out)
    }
}

/// Fraction digits strictly between `a` (empty = 0) and `b` (`None` = 1). Requires
/// `a < b` and no trailing zeros; returns no trailing zero.
fn midpoint(a: &[u8], b: Option<&[u8]>) -> Vec<u8> {
    let dv = |c: u8| digit(c).unwrap_or(0);
    if let Some(b) = b {
        // shared prefix (a padded with zeros): keep it and recurse on the rest
        let n = b
            .iter()
            .enumerate()
            .take_while(|&(i, &d)| a.get(i).copied().unwrap_or(b'0') == d)
            .count();
        if n > 0 {
            let mut out = b[..n].to_vec();
            out.extend(midpoint(a.get(n..).unwrap_or(&[]), Some(&b[n..])));
            return out;
        }
    }
    let da = a.first().map_or(0, |&c| dv(c));
    let db = b.map_or(62, |b| b.first().map_or(62, |&c| dv(c)));
    if db - da > 1 {
        let mid = (u16::from(da) + u16::from(db)).div_ceil(2);
        return vec![DIGITS[usize::from(mid)]];
    }
    if let Some(b) = b
        && b.len() > 1
    {
        return vec![b[0]];
    }
    let mut out = vec![DIGITS[usize::from(da)]];
    out.extend(midpoint(a.get(1..).unwrap_or(&[]), None));
    out
}

fn increment(int: &[u8]) -> Option<Vec<u8>> {
    let head = int[0];
    let mut digs = int[1..].to_vec();
    let mut carry = true;
    for d in digs.iter_mut().rev() {
        let v = digit(*d)? + 1;
        if v == 62 {
            *d = b'0';
        } else {
            *d = DIGITS[usize::from(v)];
            carry = false;
            break;
        }
    }
    if !carry {
        let mut out = vec![head];
        out.extend(digs);
        return Some(out);
    }
    match head {
        b'Z' => Some(b"a0".to_vec()),
        b'z' => None,
        _ => {
            let h = head + 1;
            if h > b'a' {
                digs.push(b'0');
            } else {
                digs.pop();
            }
            let mut out = vec![h];
            out.extend(digs);
            Some(out)
        }
    }
}

fn decrement(int: &[u8]) -> Option<Vec<u8>> {
    let head = int[0];
    let mut digs = int[1..].to_vec();
    let mut borrow = true;
    for d in digs.iter_mut().rev() {
        let v = digit(*d)?;
        if v == 0 {
            *d = b'z';
        } else {
            *d = DIGITS[usize::from(v - 1)];
            borrow = false;
            break;
        }
    }
    if !borrow {
        let mut out = vec![head];
        out.extend(digs);
        return Some(out);
    }
    match head {
        b'a' => Some(b"Zz".to_vec()),
        b'A' => None,
        _ => {
            let h = head - 1;
            if h < b'Z' {
                digs.push(b'z');
            } else {
                digs.pop();
            }
            let mut out = vec![h];
            out.extend(digs);
            Some(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn k(s: &str) -> OrderKey {
        OrderKey::parse(s).unwrap()
    }

    #[test]
    fn parse_rejects_bad_keys() {
        assert_eq!(OrderKey::parse(""), Err(OrderKeyError::Empty));
        assert_eq!(OrderKey::parse("a00"), Err(OrderKeyError::TrailingZero));
        assert_eq!(OrderKey::parse("a0-"), Err(OrderKeyError::BadDigit));
        assert_eq!(OrderKey::parse("0"), Err(OrderKeyError::BadInteger));
        assert_eq!(OrderKey::parse("b0"), Err(OrderKeyError::BadInteger));
        assert_eq!(
            OrderKey::parse(SMALLEST_INTEGER),
            Err(OrderKeyError::Smallest)
        );
        assert!(OrderKey::parse("a0").is_ok());
        assert!(OrderKey::parse("a0V").is_ok());
        assert!(OrderKey::parse("Zz").is_ok());
    }

    #[test]
    fn known_sequences() {
        // the reference implementation's published examples
        assert_eq!(OrderKey::between(None, None).unwrap().as_str(), "a0");
        assert_eq!(
            OrderKey::between(Some(&k("a0")), None).unwrap().as_str(),
            "a1"
        );
        assert_eq!(
            OrderKey::between(Some(&k("a1")), None).unwrap().as_str(),
            "a2"
        );
        assert_eq!(
            OrderKey::between(None, Some(&k("a0"))).unwrap().as_str(),
            "Zz"
        );
        assert_eq!(
            OrderKey::between(Some(&k("az")), None).unwrap().as_str(),
            "b00"
        );
        assert_eq!(
            OrderKey::between(None, Some(&k("b00"))).unwrap().as_str(),
            "az"
        );
        assert_eq!(
            OrderKey::between(Some(&k("a0")), Some(&k("a1")))
                .unwrap()
                .as_str(),
            "a0V"
        );
        assert_eq!(
            OrderKey::between(Some(&k("a1")), Some(&k("a2")))
                .unwrap()
                .as_str(),
            "a1V"
        );
        assert_eq!(
            OrderKey::between(Some(&k("a0V")), Some(&k("a1")))
                .unwrap()
                .as_str(),
            "a0l"
        );
        assert_eq!(
            OrderKey::between(Some(&k("Zz")), Some(&k("a0")))
                .unwrap()
                .as_str(),
            "ZzV"
        );
        assert_eq!(
            OrderKey::between(Some(&k("a0")), Some(&k("a0V")))
                .unwrap()
                .as_str(),
            "a0G"
        );
        assert_eq!(
            OrderKey::between(Some(&k("Zz")), None).unwrap().as_str(),
            "a0"
        );
        assert_eq!(
            OrderKey::between(None, Some(&k("a0V"))).unwrap().as_str(),
            "a0"
        );
        assert_eq!(
            OrderKey::between(Some(&k("b125")), Some(&k("b129")))
                .unwrap()
                .as_str(),
            "b127"
        );
    }

    #[test]
    fn between_rejects_reversed_bounds() {
        assert_eq!(
            OrderKey::between(Some(&k("a1")), Some(&k("a0"))),
            Err(OrderKeyError::NotIncreasing)
        );
        assert_eq!(
            OrderKey::between(Some(&k("a1")), Some(&k("a1"))),
            Err(OrderKeyError::NotIncreasing)
        );
    }

    #[test]
    fn between_never_returns_an_unparseable_key() {
        let lo = OrderKey::first();
        let mut hi = OrderKey::between(Some(&lo), None).unwrap();
        loop {
            match OrderKey::between(Some(&lo), Some(&hi)) {
                Ok(k) => {
                    assert!(
                        OrderKey::parse(k.as_str()).is_ok(),
                        "{} {:?}",
                        k.as_str().len(),
                        OrderKey::parse(k.as_str())
                    );
                    hi = k;
                }
                Err(e) => {
                    assert_eq!(e, OrderKeyError::TooLong);
                    break;
                }
            }
        }
    }

    #[test]
    fn deserialising_validates() {
        let ok: OrderKey = serde_json::from_str("\"a0\"").unwrap();
        assert_eq!(ok.as_str(), "a0");
        assert!(serde_json::from_str::<OrderKey>("\"a00\"").is_err());
        assert!(serde_json::from_str::<OrderKey>("\"\"").is_err());
        assert_eq!(serde_json::to_string(&ok).unwrap(), "\"a0\"");
    }

    #[test]
    fn hundred_thousand_appends_and_prepends_stay_short() {
        let mut last = OrderKey::first();
        for _ in 0..100_000 {
            let n = OrderKey::between(Some(&last), None).unwrap();
            assert!(last < n);
            last = n;
        }
        assert!(last.as_str().len() <= 5, "{}", last.as_str());
        let mut first = OrderKey::first();
        for _ in 0..100_000 {
            let n = OrderKey::between(None, Some(&first)).unwrap();
            assert!(n < first);
            first = n;
        }
        assert!(first.as_str().len() <= 5, "{}", first.as_str());
    }

    /// A deterministic xorshift so the 10⁵-insert test is cheap and reproducible.
    fn xorshift(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    /// A02 acceptance: 10⁵ inserts at random positions; every key valid, the list
    /// strictly increasing, and no key over 64 bytes.
    #[test]
    fn hundred_thousand_random_inserts_stay_ordered_and_short() {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut keys: Vec<OrderKey> = Vec::with_capacity(100_001);
        let mut longest = 0;
        for _ in 0..100_000 {
            let n = keys.len();
            let at = (xorshift(&mut seed) % (n as u64 + 1)) as usize;
            let lo = at.checked_sub(1).map(|i| &keys[i]);
            let hi = keys.get(at);
            let key = OrderKey::between(lo, hi).unwrap();
            assert!(OrderKey::parse(key.as_str()).is_ok());
            if let Some(lo) = lo {
                assert!(lo < &key);
            }
            if let Some(hi) = hi {
                assert!(&key < hi);
            }
            longest = longest.max(key.as_str().len());
            keys.insert(at, key);
        }
        assert!(longest <= 64, "longest key {longest} bytes");
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Any sequence of inserts (front, back, between neighbours) keeps the list
        /// strictly ordered and every key valid.
        #[test]
        fn inserts_keep_order(ops in proptest::collection::vec((0u8..3, any::<u16>()), 1..400)) {
            let mut keys: Vec<OrderKey> = Vec::new();
            for (kind, pos) in ops {
                let n = keys.len();
                let at = match kind {
                    0 => 0,
                    1 => n,
                    _ => usize::from(pos) % (n + 1),
                };
                let lo = at.checked_sub(1).map(|i| &keys[i]);
                let hi = keys.get(at);
                let key = OrderKey::between(lo, hi).unwrap();
                prop_assert!(OrderKey::parse(key.as_str()).is_ok());
                keys.insert(at, key);
            }
            prop_assert!(keys.windows(2).all(|w| w[0] < w[1]));
        }
    }
}
