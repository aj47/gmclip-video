//! D.04 acceptance for the caption term gate, on AJ's own words (lab: reads his caption
//! terms and three streams' transcripts from the lab media folder, read-only):
//! - the HL incident: "XD.dev" is reported as "exe.dev";
//! - 20 seeded misspellings of AJ's terms (one edit each, none a listed variant) dropped
//!   into real transcripts are all reported, with the right spelling;
//! - on the unseeded transcripts, near-miss reports stay ≤ 1 per 1 000 words.

use gmclip_core::terms::{FindingKind, GateWord, gate, key};
use gmclip_model::entities::Term;
use serde_json::Value;

const STREAMS: [&str; 3] = ["loc_4fadb756", "loc_086da141", "loc_bd89f38b"];

fn terms() -> Option<Vec<Term>> {
    let v: Value = serde_json::from_slice(
        &std::fs::read(gmclip_model::lab::media("caption-terms.json")).ok()?,
    )
    .ok()?;
    Some(
        v.as_object()?
            .iter()
            .filter(|(k, _)| !k.starts_with('_'))
            .map(|(k, v)| {
                let list = v.as_array().or_else(|| v["variants"].as_array());
                Term {
                    written: k.clone(),
                    heard_as: list
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default(),
                }
            })
            .collect(),
    )
}

fn words(vid: &str) -> Vec<String> {
    let Ok(b) = std::fs::read(gmclip_model::lab::media(format!("asr/{vid}/out.json"))) else {
        return Vec::new();
    };
    let v: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
    v["sentences"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|s| s["words"].as_array().cloned().unwrap_or_default())
        .filter_map(|w| w["word"].as_str().map(str::to_owned))
        .collect()
}

fn gw(ws: &[String]) -> Vec<GateWord> {
    ws.iter()
        .enumerate()
        .map(|(i, t)| GateWord {
            ordinal: i as u32,
            text: t.clone(),
        })
        .collect()
}

/// One-edit misspellings of a term's single-token canonical form (not a listed variant).
fn misspell(t: &Term, k: usize) -> Option<String> {
    let w = &t.written;
    if w.contains(' ') || key(w).chars().count() < 5 {
        return None;
    }
    let c: Vec<char> = w.chars().collect();
    let i = 1 + k % (c.len() - 2);
    let mut v = c.clone();
    match k % 3 {
        0 => {
            v.remove(i);
        }
        1 => v.swap(i, i + 1),
        _ => v.insert(i, c[i]),
    }
    let s: String = v.into_iter().collect();
    let listed = t.heard_as.iter().any(|h| key(h) == key(&s));
    (key(&s) != key(w) && !listed).then_some(s)
}

#[test]
fn xd_dev_incident_seeded_misspellings_and_false_positives() {
    let Some(terms) = terms() else {
        eprintln!("skipped: no caption terms in the lab media");
        return;
    };
    // the incident
    let f = gate(&gw(&["on".into(), "XD.dev".into(), "today".into()]), &terms);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].written, "exe.dev");

    let mut all: Vec<String> = Vec::new();
    for s in STREAMS {
        all.extend(words(s));
    }
    if all.len() < 5_000 {
        eprintln!("skipped: transcripts not here");
        return;
    }
    // false positives: near-miss reports on the clean words
    let clean = gate(&gw(&all), &terms);
    let near: Vec<&gmclip_core::terms::Finding> = clean
        .iter()
        .filter(|f| f.kind == FindingKind::Near)
        .collect();
    let rate = near.len() as f64 * 1000.0 / all.len() as f64;
    eprintln!(
        "{} words; {} variant reports (true misspellings in the ASR), {} near reports ({rate:.2} per 1 000): {:?}",
        all.len(),
        clean.len() - near.len(),
        near.len(),
        near.iter()
            .map(|f| format!("{} → {}", f.shown, f.written))
            .collect::<Vec<_>>()
    );
    assert!(rate <= 1.0, "{rate:.2} near reports per 1 000 words");

    // 20 seeded misspellings
    let mut seeds: Vec<(String, String)> = Vec::new();
    let mut k = 0;
    while seeds.len() < 20 && k < 400 {
        let t = &terms[k % terms.len()];
        if let Some(m) = misspell(t, k / terms.len())
            && !seeds.iter().any(|(s, _)| *s == m)
        {
            seeds.push((m, t.written.clone()));
        }
        k += 1;
    }
    assert_eq!(seeds.len(), 20, "{seeds:?}");
    let mut seeded = all.clone();
    let step = seeded.len() / 21;
    let at: Vec<usize> = (1..=20).map(|i| i * step).collect();
    for (p, (m, _)) in at.iter().zip(&seeds) {
        seeded[*p] = m.clone();
    }
    let found = gate(&gw(&seeded), &terms);
    let mut caught = 0;
    for (p, (m, w)) in at.iter().zip(&seeds) {
        let hit = found
            .iter()
            .any(|f| f.ordinals.contains(&(*p as u32)) && f.written == *w);
        if hit {
            caught += 1;
        } else {
            eprintln!("missed: {m} → {w}");
        }
    }
    eprintln!("seeded: {caught}/20 caught");
    assert_eq!(caught, 20);
}
