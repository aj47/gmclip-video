//! `trace_id`s (A-02): a ULID minted once per CLI invocation, at the edge, before
//! anything else runs. It is on every log line of the run (the `cmd` span) and in every
//! JSON error, so a user's error output finds its log lines.
//!
//! Randomness: std's `RandomState` keys come from the OS RNG; hashing the clock, pid and
//! a counter under fresh keys gives 80 unpredictable-enough bits without a new crate.
//! A trace id is a correlation handle, not a secret.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use gmclip_model::Ulid;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A new ULID: the current millisecond and 80 random bits.
pub fn mint() -> Ulid {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let ms = u64::try_from(now.as_millis()).unwrap_or(u64::MAX) & ((1 << 48) - 1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut random: u128 = 0;
    for lane in 0..2u8 {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(now.as_nanos());
        h.write_u32(std::process::id());
        h.write_u64(n);
        h.write_u8(lane);
        random = (random << 64) | u128::from(h.finish());
    }
    Ulid::from_parts(ms, random)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_well_formed_unique_and_time_ordered() {
        let ids: Vec<Ulid> = (0..1000).map(|_| mint()).collect();
        let set: std::collections::BTreeSet<_> = ids.iter().collect();
        assert_eq!(set.len(), ids.len());
        for id in &ids {
            let s = id.to_string();
            assert_eq!(s.len(), 26);
            assert_eq!(Ulid::parse(&s).unwrap(), *id);
        }
        assert!(ids.last().unwrap().timestamp_ms() >= ids[0].timestamp_ms());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        assert!(now.abs_diff(ids[0].timestamp_ms()) < 60_000);
    }
}
