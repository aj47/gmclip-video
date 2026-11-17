//! Events (SPEC §3.10): one per committed transaction, fanned out to per-subscriber
//! bounded queues (1 000 events or 4 MB). A subscriber that falls behind loses its
//! queue and gets a single `Resync{rev}`: re-read the snapshot, then carry on. A ring
//! of recent events serves `project_watch{since, max}`.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use gmclip_proto::Event;

pub const QUEUE_EVENTS: usize = 1_000;
pub const QUEUE_BYTES: usize = 4 << 20;
const RING: usize = 1_000;

#[derive(Debug, Default)]
struct Queue {
    events: VecDeque<(Event, usize)>,
    bytes: usize,
    closed: bool,
}

#[derive(Debug, Default)]
struct Shared {
    q: Mutex<Queue>,
    cv: Condvar,
}

/// A subscriber's end.
#[derive(Debug, Clone)]
pub struct Subscription {
    s: Arc<Shared>,
}

impl Subscription {
    /// Everything queued now.
    pub fn drain(&self) -> Vec<Event> {
        let mut q = self
            .s
            .q
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        q.bytes = 0;
        q.events.drain(..).map(|(e, _)| e).collect()
    }

    /// Waits up to `timeout` for at least one event, then drains.
    pub fn wait(&self, timeout: Duration) -> Vec<Event> {
        let q = self
            .s
            .q
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut q, _) = self
            .s
            .cv
            .wait_timeout_while(q, timeout, |q| q.events.is_empty() && !q.closed)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        q.bytes = 0;
        q.events.drain(..).map(|(e, _)| e).collect()
    }
}

/// The publisher side, owned by the actor.
#[derive(Debug, Default)]
pub struct EventHub {
    subs: Vec<Arc<Shared>>,
    ring: VecDeque<(Event, usize)>,
    ring_bytes: usize,
    /// The highest seq evicted from the ring: `since` older than this needs a resync.
    evicted: u64,
    seq: u64,
}

fn size_of(e: &Event) -> usize {
    serde_json::to_vec(e).map_or(256, |v| v.len())
}

impl EventHub {
    pub fn subscribe(&mut self) -> Subscription {
        let s = Arc::new(Shared::default());
        self.subs.push(Arc::clone(&s));
        Subscription { s }
    }

    pub fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// Delivers one event to every live subscriber (dropping ones whose handle is gone).
    pub fn publish(&mut self, e: Event, rev: u64) {
        let n = size_of(&e);
        self.subs.retain(|s| Arc::strong_count(s) > 1);
        let mut resync_seq = None;
        for s in &self.subs {
            let mut q =
                s.q.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            if q.events.len() >= QUEUE_EVENTS || q.bytes + n > QUEUE_BYTES {
                // overflow: drop the backlog, tell the subscriber to re-read
                q.events.clear();
                let seq = *resync_seq.get_or_insert_with(|| {
                    self.seq += 1;
                    self.seq
                });
                let r = Event::Resync { seq, rev };
                let rn = size_of(&r);
                q.events.push_back((r, rn));
                q.bytes = rn;
            } else {
                q.events.push_back((e.clone(), n));
                q.bytes += n;
            }
            drop(q);
            s.cv.notify_all();
        }
        self.ring.push_back((e, n));
        self.ring_bytes += n;
        while self.ring.len() > RING || self.ring_bytes > QUEUE_BYTES {
            let Some((old, b)) = self.ring.pop_front() else {
                break;
            };
            self.ring_bytes -= b;
            self.evicted = self.evicted.max(old.seq());
        }
    }

    /// `project_watch{since, max}`: events with `seq > since`, oldest first; `None`
    /// when `since` is older than the ring, or newer than anything published (a `since`
    /// from another holder's lifetime: the project moved, R-ARCH-7) — the caller must
    /// re-read: a resync.
    pub fn since(&self, since: u64, max: usize) -> Option<Vec<Event>> {
        if since < self.evicted || since > self.seq {
            return None;
        }
        Some(
            self.ring
                .iter()
                .map(|(e, _)| e)
                .filter(|e| e.seq() > since)
                .take(max)
                .cloned()
                .collect(),
        )
    }

    pub fn close(&mut self) {
        for s in &self.subs {
            let mut q =
                s.q.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            q.closed = true;
            drop(q);
            s.cv.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64) -> Event {
        Event::Committed {
            seq,
            rev: seq,
            by: "human".into(),
            command: "part_trim".into(),
            triples: vec![],
            display: String::new(),
        }
    }

    /// A13: a 1 000-op burst into a bounded queue → `Resync`, never unbounded growth.
    #[test]
    fn a_burst_overflows_into_resync() {
        let mut hub = EventHub::default();
        let slow = hub.subscribe();
        let fast = hub.subscribe();
        for _ in 0..1_500 {
            let seq = hub.next_seq();
            hub.publish(ev(seq), seq);
            if seq % 10 == 0 {
                assert!(fast.drain().len() <= 10);
            }
        }
        let got = slow.drain();
        assert!(got.len() <= QUEUE_EVENTS);
        assert!(matches!(got[0], Event::Resync { .. }), "{:?}", got[0]);
        // after the resync the stream continues in order
        assert!(got.windows(2).all(|w| w[0].seq() < w[1].seq()));
        assert!(
            fast.drain()
                .iter()
                .all(|e| matches!(e, Event::Committed { .. }))
        );
    }

    #[test]
    fn watch_since_serves_the_ring() {
        let mut hub = EventHub::default();
        for _ in 0..5 {
            let seq = hub.next_seq();
            hub.publish(ev(seq), seq);
        }
        let got = hub.since(2, 10).unwrap();
        assert_eq!(
            got.iter().map(Event::seq).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        assert_eq!(hub.since(2, 1).unwrap().len(), 1);
        for _ in 0..2_000 {
            let seq = hub.next_seq();
            hub.publish(ev(seq), seq);
        }
        assert!(hub.since(2, 10).is_none(), "too old: resync");
        let now = 2_005; // 5 + 2 000 published
        assert!(
            hub.since(now, 10).is_some_and(|v| v.is_empty()),
            "up to date"
        );
        // a `since` from another holder's lifetime (the project moved, R-ARCH-7)
        assert!(hub.since(now + 1, 10).is_none(), "from the future: resync");
        assert!(hub.since(u64::MAX, 10).is_none(), "no overflow");
    }
}
