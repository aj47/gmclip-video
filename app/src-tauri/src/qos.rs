//! Background work yields to playback (SPEC §3.11, backlog D.01): ASR on import runs at
//! utility QoS and pauses **only** when the playback scheduler reports a dropped frame in
//! the last 2 s, resuming 5 s after the last drop. While nothing drops (the app idle, or
//! playing smoothly) it never waits, so its throughput is the M1 measurement's (R-ML-1).
//!
//! The render thread reports each drop ([`QosGate::frame_dropped`]); a background job asks
//! between its steps ([`QosGate::wait`]), which blocks while paused and records a trace of
//! every pause and resume ([`QosGate::trace`]).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// A drop this recent pauses the job.
pub const RECENT: Duration = Duration::from_secs(2);
/// The job resumes this long after the last drop.
pub const RESUME_AFTER: Duration = Duration::from_secs(5);
/// Trace entries kept (the newest).
const TRACE_CAP: usize = 256;

/// One step of the trace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QosEvent {
    /// The job paused: the last drop was `since` before it asked.
    Paused { at: Instant, since: Duration },
    /// The job resumed, `after` the last drop.
    Resumed { at: Instant, after: Duration },
}

#[derive(Debug, Default)]
struct State {
    last_drop: Option<Instant>,
    drops: u64,
    paused: bool,
    trace: Vec<QosEvent>,
}

/// What a job should do now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Run,
    /// Paused until this instant (later drops move it).
    PauseUntil(Instant),
}

/// The pause rule, shared by every background job of the app.
#[derive(Debug)]
pub struct QosGate {
    recent: Duration,
    resume_after: Duration,
    st: Mutex<State>,
}

impl Default for QosGate {
    fn default() -> Self {
        Self::with_windows(RECENT, RESUME_AFTER)
    }
}

impl QosGate {
    /// A gate with other windows (tests run the rule on a shorter clock).
    pub fn with_windows(recent: Duration, resume_after: Duration) -> QosGate {
        QosGate {
            recent,
            resume_after,
            st: Mutex::new(State::default()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.st
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The scheduler dropped a frame at `at` (the render thread, while playing).
    pub fn frame_dropped(&self, at: Instant) {
        let mut s = self.state();
        s.drops += 1;
        if s.last_drop.is_none_or(|d| at > d) {
            s.last_drop = Some(at);
        }
    }

    /// Drops reported so far.
    pub fn drops(&self) -> u64 {
        self.state().drops
    }

    /// Whether a job is paused now (as of its last [`QosGate::decide`]).
    pub fn paused(&self) -> bool {
        self.state().paused
    }

    /// The rule at `now`: running jobs pause when the last drop is within `recent`; a
    /// paused job resumes once `resume_after` has passed since the last drop.
    pub fn decide(&self, now: Instant) -> Decision {
        let mut s = self.state();
        let Some(d) = s.last_drop else {
            return Decision::Run;
        };
        let since = now.saturating_duration_since(d);
        if !s.paused && since <= self.recent {
            s.paused = true;
            push(&mut s.trace, QosEvent::Paused { at: now, since });
        }
        if s.paused {
            if since >= self.resume_after {
                s.paused = false;
                push(
                    &mut s.trace,
                    QosEvent::Resumed {
                        at: now,
                        after: since,
                    },
                );
            } else {
                return Decision::PauseUntil(d + self.resume_after);
            }
        }
        Decision::Run
    }

    /// Blocks while the rule says pause, checking `cancel` every 50 ms. Calls `on` with
    /// `true` when it starts waiting and `false` when it resumes. Returns whether it
    /// waited at all.
    pub fn wait(&self, cancel: &AtomicBool, mut on: impl FnMut(bool)) -> bool {
        let mut waited = false;
        while let Decision::PauseUntil(until) = self.decide(Instant::now()) {
            if !waited {
                waited = true;
                on(true);
            }
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let left = until.saturating_duration_since(Instant::now());
            std::thread::sleep(left.min(Duration::from_millis(50)));
        }
        if waited {
            on(false);
        }
        waited
    }

    /// The pauses and resumes so far (oldest first, the newest [`TRACE_CAP`]).
    pub fn trace(&self) -> Vec<QosEvent> {
        self.state().trace.clone()
    }
}

fn push(t: &mut Vec<QosEvent>, e: QosEvent) {
    if t.len() >= TRACE_CAP {
        t.remove(0);
    }
    t.push(e);
}

/// Runs the calling thread at utility QoS (macOS `QOS_CLASS_UTILITY`): background work
/// below the UI and the render thread. A no-op elsewhere until M5's EcoQoS.
pub fn utility_thread() {
    #[cfg(target_os = "macos")]
    gmclip_gpu::apple::qos::set_utility();
}

/// Back to the default QoS (work after ASR whose cancel is coarse: the app waits for it
/// on quit, so it must not crawl on a loaded machine).
pub fn default_thread() {
    #[cfg(target_os = "macos")]
    gmclip_gpu::apple::qos::set_default();
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn runs_until_a_recent_drop_and_resumes_five_seconds_after_it() {
        let g = QosGate::default();
        let t0 = Instant::now();
        // nothing dropped: never pauses
        assert_eq!(g.decide(t0), Decision::Run);
        // a drop 3 s before the job asks is not recent: no pause
        g.frame_dropped(t0);
        assert_eq!(g.decide(t0 + 3 * S), Decision::Run);
        assert!(g.trace().is_empty());
        // a drop 1 s before: paused until 5 s after it
        g.frame_dropped(t0 + 10 * S);
        assert_eq!(g.decide(t0 + 11 * S), Decision::PauseUntil(t0 + 15 * S));
        assert!(g.paused());
        // still paused at 4.9 s, even though the drop is no longer "recent"
        assert_eq!(
            g.decide(t0 + 14 * S + Duration::from_millis(900)),
            Decision::PauseUntil(t0 + 15 * S)
        );
        // another drop while paused moves the resume
        g.frame_dropped(t0 + 14 * S);
        assert_eq!(g.decide(t0 + 15 * S), Decision::PauseUntil(t0 + 19 * S));
        // resumed 5 s after the last drop
        assert_eq!(g.decide(t0 + 19 * S), Decision::Run);
        assert!(!g.paused());
        assert_eq!(
            g.trace(),
            vec![
                QosEvent::Paused {
                    at: t0 + 11 * S,
                    since: S
                },
                QosEvent::Resumed {
                    at: t0 + 19 * S,
                    after: 5 * S
                },
            ]
        );
        assert_eq!(g.drops(), 3);
    }

    #[test]
    fn wait_blocks_for_the_window_and_reports_it() {
        let g = QosGate::with_windows(Duration::from_millis(100), Duration::from_millis(250));
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();
        assert!(!g.wait(&cancel, |p| seen.push(p)));
        g.frame_dropped(Instant::now());
        let t = Instant::now();
        assert!(g.wait(&cancel, |p| seen.push(p)));
        let took = t.elapsed();
        assert!(took >= Duration::from_millis(200), "{took:?}");
        assert!(took < Duration::from_millis(1000), "{took:?}");
        assert_eq!(seen, vec![true, false]);
        // cancel ends a wait at once
        g.frame_dropped(Instant::now());
        cancel.store(true, Ordering::Relaxed);
        let t = Instant::now();
        g.wait(&cancel, |_| {});
        assert!(t.elapsed() < Duration::from_millis(100));
    }
}
