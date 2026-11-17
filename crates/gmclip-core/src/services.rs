//! Deterministic services: the clock and the ULID minter (R-MODEL-1). The actor owns
//! both; handlers see the commit time through `Cx::services` and mint through
//! `Tx::mint`, and every minted id is stored in the forward op, so replay never mints.

use std::fmt::Debug;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicI64, Ordering};

use gmclip_model::{MintId, Ulid};

/// A source of wall-clock time in ms since the Unix epoch.
pub trait Clock: Send + Sync + Debug {
    fn now_ms(&self) -> i64;
}

/// The system clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }
}

/// A settable clock for tests.
#[derive(Debug, Default)]
pub struct TestClock(pub AtomicI64);

impl TestClock {
    pub fn new(ms: i64) -> Self {
        Self(AtomicI64::new(ms))
    }
    pub fn advance(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Monotonic ULIDs: a new millisecond draws fresh random bits, the same millisecond
/// increments the last id, so ids minted by one actor sort in mint order (the split
/// edit relies on that to keep copied visual splits in order).
#[derive(Debug, Clone)]
pub struct UlidGen {
    last_ms: u64,
    last_rand: u128,
    state: u64,
    now_ms: u64,
}

impl UlidGen {
    /// Seeded from `seed` (tests) or from the process's random hasher state.
    pub fn new(seed: Option<u64>) -> Self {
        let state = seed.unwrap_or_else(|| {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u64(SystemClock.now_ms().unsigned_abs());
            h.write_u32(std::process::id());
            h.finish()
        });
        Self {
            last_ms: 0,
            last_rand: 0,
            state,
            now_ms: 0,
        }
    }

    /// Sets the time the next ids are minted at (the transaction's commit time).
    pub fn at(&mut self, now_ms: i64) {
        self.now_ms = u64::try_from(now_ms).unwrap_or(0) & ((1 << 48) - 1);
    }

    fn next_u64(&mut self) -> u64 {
        // splitmix64
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub fn next_ulid(&mut self) -> Ulid {
        let mask: u128 = (1 << 80) - 1;
        if self.now_ms > self.last_ms || self.last_rand >= mask {
            // a clock that goes back never breaks monotonicity
            self.last_ms = self
                .now_ms
                .max(self.last_ms + u64::from(self.now_ms <= self.last_ms));
            // 79 random bits: headroom so same-ms increments cannot overflow
            let r = (u128::from(self.next_u64()) << 16) | u128::from(self.next_u64() & 0xffff);
            self.last_rand = r & (mask >> 1);
        } else {
            self.last_rand += 1;
        }
        Ulid::from_parts(self.last_ms, self.last_rand)
    }
}

impl MintId for UlidGen {
    fn mint(&mut self) -> Ulid {
        self.next_ulid()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulids_are_monotonic_and_seeded() {
        let mut g = UlidGen::new(Some(7));
        g.at(1_000);
        let a: Vec<Ulid> = (0..1_000).map(|_| g.next_ulid()).collect();
        assert!(a.windows(2).all(|w| w[0] < w[1]));
        g.at(999);
        let b = g.next_ulid();
        assert!(b > a[999]);
        let mut h = UlidGen::new(Some(7));
        h.at(1_000);
        assert_eq!(h.next_ulid(), a[0]);
        assert_eq!(a[0].timestamp_ms(), 1_000);
    }
}
