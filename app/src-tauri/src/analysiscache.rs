//! What the analysis host heard, kept across launches (dev-bridge builds; the e2e runner).
//! Every e2e spec launches the app with a throwaway HOME, so each one transcribed and
//! measured the same test video again: a minute or more per spec, several when specs run
//! side by side. `GMCLIP_ANALYSIS_CACHE` names a folder that outlives those HOMEs; the
//! host's `transcribe` and `analyze` results are stored there and handed back, so a
//! video is heard once per change to what hearing it depends on.
//!
//! The key is the file's bytes (sha256), its voice track, the span, the pinned models
//! ([`gmclip_ml::models::REGISTER`]) and a hash of the analysis code taken at build time
//! (`build.rs`: the CLI's analysis host, gmclip-analysis, gmclip-ml, gmclip-media), so an
//! edit there hears again. Only what the host returns is kept: chunking, the term-list
//! correction and storing on the project run as before. Errors are never kept.
#![doc(hidden)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use gmclip_core::host::{
    AnalysisBlob, AnalysisHost, AudioSource, EdgeRule, Heard, HeardWord, HostError, MeasuredEdge,
    SpanLoudness, SweepCandidates, Transcribed,
};
use gmclip_ml::sha256::Sha256;
use gmclip_model::Flicks;
use serde_json::{Value, json};

/// The analysis code's hash (`build.rs`).
const CODE: &str = env!("GMCLIP_ANALYSIS_CODE_HASH");

/// A file as last seen: path, size, mtime.
type FileId = (PathBuf, u64, Option<SystemTime>);

/// An [`AnalysisHost`] that keeps `transcribe` and `analyze` results in a folder.
#[derive(Debug)]
pub struct CachedAnalysis {
    inner: Arc<dyn AnalysisHost>,
    dir: PathBuf,
    /// A file's sha256 by (path, size, mtime): each file is hashed once per launch.
    files: Mutex<BTreeMap<FileId, String>>,
}

impl CachedAnalysis {
    /// `inner` behind the folder `GMCLIP_ANALYSIS_CACHE` names (absolute), if it names one.
    pub fn from_env(inner: Arc<dyn AnalysisHost>) -> Option<CachedAnalysis> {
        let dir = PathBuf::from(std::env::var_os("GMCLIP_ANALYSIS_CACHE")?);
        if !dir.is_absolute() || std::fs::create_dir_all(&dir).is_err() {
            return None;
        }
        Some(CachedAnalysis {
            inner,
            dir,
            files: Mutex::new(BTreeMap::new()),
        })
    }

    fn file_sha(&self, path: &Path) -> Option<String> {
        let meta = std::fs::metadata(path).ok()?;
        let id = (path.to_path_buf(), meta.len(), meta.modified().ok());
        if let Some(sha) = self.files.lock().ok()?.get(&id) {
            return Some(sha.clone());
        }
        let (sha, _) = gmclip_ml::sha256::file_hex(path).ok()?;
        self.files.lock().ok()?.insert(id, sha.clone());
        Some(sha)
    }

    /// The entry's name for `what` (`asr` and its span, or `analyze`) on `a`; `None` when
    /// the file cannot be read (the call then goes to the host uncached).
    fn key(&self, a: &AudioSource, what: &str) -> Option<String> {
        let sha = self.file_sha(&a.path)?;
        let mut h = Sha256::new();
        for e in gmclip_ml::models::REGISTER {
            h.update(e.id.as_bytes());
            for f in e.files {
                h.update(f.sha256.as_bytes());
            }
        }
        let track = format!("{:?}", a.track);
        for part in [self.inner.name(), CODE, &sha, &track, what] {
            h.update(&[0]);
            h.update(part.as_bytes());
        }
        Some(h.finish_hex())
    }

    /// A fresh path beside `to` to write into before renaming it into place (specs run
    /// side by side and may both miss: the first rename wins, the other is dropped).
    fn scratch(&self, to: &Path) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let mut name = to.file_name().unwrap_or_default().to_owned();
        name.push(format!(".tmp-{}-{n}", std::process::id()));
        to.with_file_name(name)
    }

    fn publish(tmp: &Path, to: &Path) {
        if std::fs::rename(tmp, to).is_err() {
            let _ = std::fs::remove_dir_all(tmp);
            let _ = std::fs::remove_file(tmp);
        }
    }

    fn read_transcribed(path: &Path) -> Option<Transcribed> {
        let v: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        let words = v["words"]
            .as_array()?
            .iter()
            .map(|w| {
                Some(HeardWord {
                    text: w[0].as_str()?.to_owned(),
                    start: Flicks(w[1].as_i64()?),
                    end: Flicks(w[2].as_i64()?),
                    confidence: w[3].as_f64().map(|c| c as f32),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Transcribed {
            revision: v["revision"].as_str()?.to_owned(),
            words,
        })
    }

    fn write_transcribed(&self, path: &Path, t: &Transcribed) {
        let words: Vec<Value> = t
            .words
            .iter()
            .map(|w| json!([w.text, w.start.0, w.end.0, w.confidence]))
            .collect();
        let body = json!({"revision": t.revision, "words": words}).to_string();
        let tmp = self.scratch(path);
        if std::fs::write(&tmp, body).is_ok() {
            Self::publish(&tmp, path);
        }
    }

    /// The blobs stored at `dir`, in the order the host made them.
    fn read_blobs(dir: &Path) -> Option<Vec<AnalysisBlob>> {
        let index: Value =
            serde_json::from_slice(&std::fs::read(dir.join("index.json")).ok()?).ok()?;
        index
            .as_array()?
            .iter()
            .enumerate()
            .map(|(i, b)| {
                Some(AnalysisBlob {
                    kind: b["kind"].as_str()?.to_owned(),
                    revision: b["revision"].as_str()?.to_owned(),
                    body: std::fs::read(dir.join(format!("{i}.bin"))).ok()?,
                })
            })
            .collect()
    }

    fn write_blobs(&self, dir: &Path, blobs: &[AnalysisBlob]) {
        let tmp = self.scratch(dir);
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(&tmp)?;
            for (i, b) in blobs.iter().enumerate() {
                std::fs::write(tmp.join(format!("{i}.bin")), &b.body)?;
            }
            let index: Vec<Value> = blobs
                .iter()
                .map(|b| json!({"kind": b.kind, "revision": b.revision}))
                .collect();
            std::fs::write(tmp.join("index.json"), Value::from(index).to_string())
        };
        if write().is_ok() {
            Self::publish(&tmp, dir);
        } else {
            let _ = std::fs::remove_dir_all(&tmp);
        }
    }
}

impl AnalysisHost for CachedAnalysis {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn analyze(
        &self,
        a: &AudioSource,
        cancel: &AtomicBool,
    ) -> Result<Vec<AnalysisBlob>, HostError> {
        let mut out = Vec::new();
        self.analyze_each(a, cancel, &mut |b| {
            out.push(b);
            Ok(())
        })?;
        Ok(out)
    }

    fn analyze_each(
        &self,
        a: &AudioSource,
        cancel: &AtomicBool,
        each: &mut dyn FnMut(AnalysisBlob) -> Result<(), HostError>,
    ) -> Result<(), HostError> {
        let Some(dir) = self.key(a, "analyze").map(|k| self.dir.join(k)) else {
            return self.inner.analyze_each(a, cancel, each);
        };
        if let Some(blobs) = Self::read_blobs(&dir) {
            // the voice a transcription kept for this analysis is not needed now
            self.inner.keep_voice(a, false);
            return blobs.into_iter().try_for_each(each);
        }
        let mut made = Vec::new();
        self.inner.analyze_each(a, cancel, &mut |b| {
            made.push(b.clone());
            each(b)
        })?;
        self.write_blobs(&dir, &made);
        Ok(())
    }

    fn keep_voice(&self, a: &AudioSource, keep: bool) {
        self.inner.keep_voice(a, keep);
    }

    fn transcribe(
        &self,
        a: &AudioSource,
        span: Option<(Flicks, Flicks)>,
        cancel: &AtomicBool,
    ) -> Result<Transcribed, HostError> {
        let what = span.map_or_else(|| "asr".to_owned(), |(f, t)| format!("asr {} {}", f.0, t.0));
        let Some(path) = self
            .key(a, &what)
            .map(|k| self.dir.join(format!("{k}.json")))
        else {
            return self.inner.transcribe(a, span, cancel);
        };
        if let Some(t) = Self::read_transcribed(&path) {
            return Ok(t);
        }
        let t = self.inner.transcribe(a, span, cancel)?;
        self.write_transcribed(&path, &t);
        Ok(t)
    }

    fn listen(&self, a: &AudioSource, from: Flicks, to: Flicks) -> Result<Heard, HostError> {
        self.inner.listen(a, from, to)
    }

    fn loudness(
        &self,
        a: &AudioSource,
        from: Flicks,
        to: Flicks,
    ) -> Result<SpanLoudness, HostError> {
        self.inner.loudness(a, from, to)
    }

    fn edge(
        &self,
        a: &AudioSource,
        rule: EdgeRule,
        w1: (&str, Flicks, Flicks),
        w2: Option<(&str, Flicks, Flicks)>,
    ) -> Result<MeasuredEdge, HostError> {
        self.inner.edge(a, rule, w1, w2)
    }

    fn pauses(
        &self,
        a: &AudioSource,
        spans: &[(Flicks, Flicks)],
        min: Flicks,
        keep: Flicks,
        words: &[HeardWord],
    ) -> Result<SweepCandidates, HostError> {
        self.inner.pauses(a, spans, min, keep, words)
    }

    fn fillers(
        &self,
        a: &AudioSource,
        spans: &[(Flicks, Flicks)],
        words: &[HeardWord],
    ) -> Result<SweepCandidates, HostError> {
        self.inner.fillers(a, spans, words)
    }

    fn blob_summary(
        &self,
        kind: &str,
        body: &[u8],
        from: Flicks,
        to: Flicks,
        max_points: usize,
    ) -> Result<Value, HostError> {
        self.inner.blob_summary(kind, body, from, to, max_points)
    }
}
