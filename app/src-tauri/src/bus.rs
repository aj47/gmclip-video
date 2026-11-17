//! The UI event bus behind `gm_subscribe` (SPEC §12.1): named scopes (`project:<id>`,
//! `jobs`, `viewer`, `home`), each subscriber with its own bounded queue. A subscriber
//! that falls behind gets `{"kind":"resync"}` instead of its backlog (as the core's
//! event hub does). Delivery to the webview is coalesced per 16 ms by the transport
//! ([`crate::ipc::pump`]). Events are the typed ones of [`crate::events`] (A2.01).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::events::{AgentsEvent, AppEvent, HomeEvent, JobsEvent, ProjectEvent, ViewerEvent};

/// Most events one subscriber may have queued before it is told to resync.
pub const QUEUE: usize = 1_000;

#[derive(Debug, Default)]
struct Queue {
    events: VecDeque<Value>,
    closed: bool,
}

#[derive(Debug, Default)]
struct Shared {
    q: Mutex<Queue>,
    cv: Condvar,
}

/// One subscription's receiving end.
#[derive(Debug, Clone)]
pub struct BusSub {
    pub id: u64,
    pub scope: String,
    s: Arc<Shared>,
}

impl BusSub {
    /// Waits up to `timeout` for at least one event, then drains. `None` once closed.
    pub fn wait(&self, timeout: Duration) -> Option<Vec<Value>> {
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
        if q.closed && q.events.is_empty() {
            return None;
        }
        Some(q.events.drain(..).collect())
    }

    /// Drains what is queued now.
    pub fn drain(&self) -> Vec<Value> {
        let mut q = self
            .s
            .q
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        q.events.drain(..).collect()
    }
}

/// The publisher side.
#[derive(Debug, Default)]
pub struct Bus {
    subs: Mutex<Vec<BusSub>>,
    next: AtomicU64,
}

impl Bus {
    pub fn subscribe(&self, scope: &str) -> BusSub {
        let sub = BusSub {
            id: self.next.fetch_add(1, Ordering::Relaxed) + 1,
            scope: scope.to_owned(),
            s: Arc::new(Shared::default()),
        };
        if let Ok(mut g) = self.subs.lock() {
            g.push(sub.clone());
        }
        sub
    }

    pub fn unsubscribe(&self, id: u64) -> bool {
        let Ok(mut g) = self.subs.lock() else {
            return false;
        };
        let before = g.len();
        g.retain(|s| {
            if s.id == id {
                let mut q =
                    s.s.q
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                q.closed = true;
                s.s.cv.notify_all();
                false
            } else {
                true
            }
        });
        g.len() != before
    }

    /// A `viewer` event.
    pub fn viewer(&self, e: ViewerEvent) {
        self.send("viewer", &e);
    }

    /// A `jobs` event.
    pub fn jobs(&self, e: JobsEvent) {
        self.send("jobs", &e);
    }

    /// An `app` event.
    pub fn app(&self, e: AppEvent) {
        self.send("app", &e);
    }

    /// An `agents` event (agent sessions, decision 0035).
    pub fn agents(&self, e: AgentsEvent) {
        self.send("agents", &e);
    }

    /// A `home` event.
    pub fn home(&self, e: HomeEvent) {
        self.send("home", &e);
    }

    /// A `project:<id>` event.
    pub fn project(&self, project: &str, e: ProjectEvent) {
        self.send(&format!("project:{project}"), &e);
    }

    fn send(&self, scope: &str, e: &impl serde::Serialize) {
        self.publish(scope, serde_json::to_value(e).unwrap_or(Value::Null));
    }

    /// Delivers `event` to every subscriber of `scope` (only the typed senders above
    /// publish: what reaches the webview is what `crate::events` declares).
    fn publish(&self, scope: &str, event: Value) {
        let Ok(g) = self.subs.lock() else { return };
        for s in g.iter().filter(|s| s.scope == scope) {
            let mut q =
                s.s.q
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            if q.events.len() >= QUEUE {
                q.events.clear();
                q.events.push_back(json!({"kind": "resync"}));
            } else {
                q.events.push_back(event.clone());
            }
            s.s.cv.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_delivery_and_overflow_resync() {
        let bus = Bus::default();
        let a = bus.subscribe("jobs");
        let b = bus.subscribe("viewer");
        bus.publish("jobs", json!({"n": 1}));
        assert_eq!(a.drain(), vec![json!({"n": 1})]);
        assert!(b.drain().is_empty());
        for n in 0..QUEUE + 5 {
            bus.publish("viewer", json!({ "n": n }));
        }
        let got = b.drain();
        assert_eq!(got[0], json!({"kind": "resync"}));
        assert!(got.len() < QUEUE);
        assert!(bus.unsubscribe(b.id));
        assert!(b.wait(Duration::from_millis(1)).is_none());
    }
}
