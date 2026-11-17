//! Run clocks (SPEC §4.3 `counter`; AGENTS.md "GENERATING 0:23" → "DONE IN 1:07"): a
//! counter overlay becomes one text event per shown value.
//!
//! - `start`/`stop` are a measurement (its `from`/`to`) or anchors, resolved to source
//!   time; on the `src` clock the elapsed time is the *source* time since the start, so a
//!   jump cut makes the clock jump the way the run did; on the `out` clock it counts
//!   output time.
//! - Before the start it shows `running_label 0:00`; from the stop it freezes on
//!   `done_label` and the total.
//! - `format` may hold `{label}` and `{t}` (`m:ss`); empty means `"{label} {t}"`.

use gmclip_model::anchor::Anchor;
use gmclip_model::entities::{Counter, CounterClock, CounterStart, Cut, MeasureEnd, Measurement};
use gmclip_model::{Doc, Flicks, Id};

const SEC: i64 = 705_600_000;

fn src_of_end(doc: &Doc, cut: Id<Cut>, e: &MeasureEnd) -> Result<Flicks, String> {
    let a = match e {
        MeasureEnd::Anchor(a) => a.clone(),
        MeasureEnd::Frame(f) => Anchor::Frame(*f),
        MeasureEnd::Event(_) => return Err("a run clock on a visual event (not yet)".into()),
    };
    crate::anchors::resolve(doc, Some(cut), &a, "/counter")
        .map(|r| r.src)
        .map_err(|_| "a run clock end that does not resolve".into())
}

/// `(start, stop)` source times of a counter.
pub fn span(doc: &Doc, cut: Id<Cut>, c: &Counter) -> Result<(Flicks, Option<Flicks>), String> {
    let measurement = |id: &Id<Measurement>| -> Result<&Measurement, String> {
        doc.get(*id)
            .ok_or_else(|| "a run clock's measurement is gone".to_owned())
    };
    let start = match &c.start {
        CounterStart::Measurement(m) => src_of_end(doc, cut, &measurement(m)?.from)?,
        CounterStart::Anchor(a) => crate::anchors::resolve(doc, Some(cut), a, "/counter")
            .map(|r| r.src)
            .map_err(|_| "a run clock start that does not resolve".to_owned())?,
    };
    let stop = match (&c.stop, &c.start) {
        (Some(CounterStart::Measurement(m)), _) => {
            let me = measurement(m)?;
            Some(src_of_end(doc, cut, me.to.as_ref().unwrap_or(&me.from))?)
        }
        (Some(CounterStart::Anchor(a)), _) => Some(
            crate::anchors::resolve(doc, Some(cut), a, "/counter")
                .map(|r| r.src)
                .map_err(|_| "a run clock stop that does not resolve".to_owned())?,
        ),
        (None, CounterStart::Measurement(m)) => match &measurement(m)?.to {
            Some(e) => Some(src_of_end(doc, cut, e)?),
            None => None,
        },
        (None, CounterStart::Anchor(_)) => None,
    };
    Ok((start, stop))
}

fn mss(t: i64) -> String {
    let s = t.max(0) / SEC;
    format!("{}:{:02}", s / 60, s % 60)
}

fn text(c: &Counter, label: &str, t: i64) -> String {
    let f = if c.format.trim().is_empty() {
        "{label} {t}"
    } else {
        c.format.as_str()
    };
    f.replace("{label}", label)
        .replace("{t}", &mss(t))
        .trim()
        .to_owned()
}

/// The events of a counter shown over output frames `[a, z)`: `(from, to, text)`.
/// `src_at(k)` is the source time output frame `k` shows; `start_out` the output frame of
/// the start on the `out` clock.
#[allow(clippy::too_many_arguments)]
pub fn segments(
    c: &Counter,
    start: Flicks,
    stop: Option<Flicks>,
    a: u64,
    z: u64,
    fpf: i64,
    src_at: &dyn Fn(u64) -> Option<i64>,
    start_out: Option<u64>,
) -> Vec<(u64, u64, String)> {
    let mut out: Vec<(u64, u64, String)> = Vec::new();
    for k in a..z {
        let txt = match c.clock {
            CounterClock::Src => {
                let t = src_at(k).unwrap_or(start.0);
                match stop {
                    Some(s) if t >= s.0 => text(c, &c.done_label, s.0 - start.0),
                    _ => text(c, &c.running_label, t - start.0),
                }
            }
            CounterClock::Out => {
                let k0 = start_out.unwrap_or(a);
                let el = (k.saturating_sub(k0) as i64) * fpf;
                let total = stop.map(|s| s.0 - start.0);
                match total {
                    Some(tt) if el >= tt => text(c, &c.done_label, tt),
                    _ => text(c, &c.running_label, el),
                }
            }
        };
        match out.last_mut() {
            Some(l) if l.2 == txt && l.1 == k => l.1 = k + 1,
            _ => out.push((k, k + 1, txt)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_text() {
        let c = Counter {
            start: CounterStart::Anchor(Anchor::Raw(gmclip_model::anchor::RawTime {
                time: gmclip_model::clock::ModelTime::Src {
                    source: None,
                    group: None,
                    t: Flicks(0),
                },
                reason: "test".into(),
            })),
            stop: None,
            clock: CounterClock::Src,
            format: String::new(),
            running_label: "GENERATING".into(),
            done_label: "DONE IN".into(),
        };
        let fpf = SEC / 30;
        // 3 s of output that plays source 0–1 s then 60–62 s (a jump), stop at 61.5 s.
        let src = |k: u64| {
            Some(if k < 30 {
                k as i64 * fpf
            } else {
                60 * SEC + (k as i64 - 30) * fpf
            })
        };
        let seg = segments(
            &c,
            Flicks(0),
            Some(Flicks(61 * SEC + SEC / 2)),
            0,
            90,
            fpf,
            &src,
            None,
        );
        let texts: Vec<&str> = seg.iter().map(|s| s.2.as_str()).collect();
        assert_eq!(
            texts,
            [
                "GENERATING 0:00",
                "GENERATING 1:00",
                "GENERATING 1:01",
                "DONE IN 1:01"
            ]
        );
        assert_eq!(seg[0].0, 0);
        assert_eq!(seg.last().unwrap().1, 90);
    }
}
