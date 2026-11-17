//! Parakeet TDT run directly on ORT for **forced alignment** (decision 0006): the word
//! arbiter scores candidate cut points by how well the known words on each side align
//! to the audio on that side, instead of decoding short probes freely (which Parakeet v3
//! does erratically).
//!
//! - Features: NeMo's own preprocessor exported to ONNX (`nemo128.onnx`, 128 mels).
//! - Encoder: the int8 encoder the ASR uses (pinned in the Parakeet row).
//! - Decoder/joint: the **fp32** export. The int8 one quantises activations dynamically
//!   over the whole call, so a joint grid computed in one call differs from frame-by-frame
//!   steps by several nats; the fp32 grid equals the steps exactly.
//! - [`TdtAligner::greedy`] is the TDT greedy decode (token ids with frames), used to get
//!   the model's own tokenisation of the words around an edge.
//! - [`TdtAligner::align_ll`] is the TDT forward algorithm: log P(tokens | audio) summed
//!   over every alignment of tokens and durations (0–4 frames), one joint-grid call.

use std::sync::Mutex;

use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;

use crate::MlError;
use crate::models::{PARAKEET_ALIGNER, PARAKEET_TDT_V3_INT8, VerifiedModel};
use crate::runtime::OrtRuntime;

/// TDT durations (frames) of Parakeet v3.
pub const DURATIONS: [usize; 5] = [0, 1, 2, 3, 4];
/// Samples per encoder frame at 16 kHz.
pub const FRAME_SAMPLES: usize = 1280;
const HIDDEN: usize = 640;
/// Largest joint grid scored in one call (floats): 256 MB. A normal edge is ~20 MB.
pub const MAX_GRID: usize = 64 << 20;
const ENC_DIM: usize = 1024;

/// One emitted token.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TokenHit {
    pub id: u32,
    pub frame: usize,
    pub duration: usize,
}

/// Encoder output: `[ENC_DIM][frames]`, row-major as ORT returns it.
#[derive(Clone, Debug)]
pub struct Encoded {
    pub data: Vec<f32>,
    pub frames: usize,
    /// The frame count of the tensor (≥ `frames`, the valid ones).
    pub stride: usize,
}

/// Joint output: logits `[frames][targets][width]`, width, and the two LSTM states.
type JointOut = (Vec<f32>, usize, Vec<f32>, Vec<f32>);

struct Sessions {
    pre: Session,
    enc: Session,
    joint: Session,
}

/// The aligner's three sessions and the vocabulary.
pub struct TdtAligner {
    s: Mutex<Sessions>,
    vocab: Vec<String>,
    blank: u32,
}

impl std::fmt::Debug for TdtAligner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TdtAligner")
            .field("vocab", &self.vocab.len())
            .finish_non_exhaustive()
    }
}

fn session(rt: &OrtRuntime, path: &std::path::Path, threads: usize) -> Result<Session, MlError> {
    let _ort = crate::runtime::inside(true);
    Session::builder(rt.environment())
        .and_then(|b| {
            b.with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(ort::Error::<()>::from)
        })
        .and_then(|b| {
            b.with_intra_threads(threads)
                .map_err(ort::Error::<()>::from)
        })
        .and_then(|mut b| b.commit_from_file(path))
        .map_err(|_| MlError::Backend("loading the aligner"))
}

fn log_softmax(x: &[f32]) -> Vec<f64> {
    let m = x.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));
    let s: f64 = x.iter().map(|&v| f64::from(v - m).exp()).sum();
    let l = s.ln();
    x.iter().map(|&v| f64::from(v - m) - l).collect()
}

fn logaddexp(a: f64, b: f64) -> f64 {
    if a == f64::NEG_INFINITY {
        return b;
    }
    if b == f64::NEG_INFINITY {
        return a;
    }
    let m = a.max(b);
    m + ((a - m).exp() + (b - m).exp()).ln()
}

impl TdtAligner {
    /// `asr` is the verified Parakeet row (encoder, vocabulary), `aligner` the verified
    /// aligner row (`nemo128.onnx`, fp32 decoder/joint).
    pub fn load(
        rt: &OrtRuntime,
        asr: &VerifiedModel,
        aligner: &VerifiedModel,
        threads: usize,
    ) -> Result<Self, MlError> {
        if asr.entry.id != PARAKEET_TDT_V3_INT8.id || aligner.entry.id != PARAKEET_ALIGNER.id {
            return Err(MlError::Invalid(
                "aligner needs the Parakeet and aligner rows",
            ));
        }
        let text = std::fs::read_to_string(asr.file("vocab.txt"))
            .map_err(|_| MlError::Load("vocabulary"))?;
        let mut vocab: Vec<String> = Vec::new();
        for line in text.lines() {
            let Some((tok, id)) = line.rsplit_once(' ') else {
                continue;
            };
            let id: usize = id.parse().map_err(|_| MlError::Load("vocabulary"))?;
            if id >= vocab.len() {
                vocab.resize(id + 1, String::new());
            }
            vocab[id] = tok.to_owned();
        }
        let blank = vocab
            .iter()
            .position(|t| t == "<blk>")
            .ok_or(MlError::Load("vocabulary has no blank"))? as u32;
        let s = Sessions {
            pre: session(rt, &aligner.file("nemo128.onnx"), 1)?,
            enc: session(rt, &asr.file("encoder-model.int8.onnx"), threads)?,
            joint: session(rt, &aligner.file("decoder_joint-model.onnx"), threads)?,
        };
        Ok(Self {
            s: Mutex::new(s),
            vocab,
            blank,
        })
    }

    pub fn piece(&self, id: u32) -> &str {
        self.vocab.get(id as usize).map_or("", String::as_str)
    }

    pub fn blank(&self) -> u32 {
        self.blank
    }

    /// Features + encoder for 16 kHz mono PCM (already at the input gain).
    pub fn encode(&self, pcm: &[f32]) -> Result<Encoded, MlError> {
        if pcm.len() < 1600 {
            return Err(MlError::Invalid("audio shorter than 0.1 s"));
        }
        let mut s = self.s.lock().unwrap_or_else(|p| p.into_inner());
        let _ort = crate::runtime::inside(false);
        let wav = Tensor::from_array(([1usize, pcm.len()], pcm.to_vec()))
            .map_err(|_| MlError::Backend("features input"))?;
        let lens = Tensor::from_array(([1usize], vec![pcm.len() as i64]))
            .map_err(|_| MlError::Backend("features input"))?;
        let out = s
            .pre
            .run(ort::inputs!["waveforms" => wav, "waveforms_lens" => lens])
            .map_err(|_| MlError::Backend("features"))?;
        let (shape, feats) = out["features"]
            .try_extract_tensor::<f32>()
            .map_err(|_| MlError::Backend("features output"))?;
        let t = *shape.get(2).ok_or(MlError::Backend("features shape"))? as usize;
        let (_, flen) = out["features_lens"]
            .try_extract_tensor::<i64>()
            .map_err(|_| MlError::Backend("features output"))?;
        let flen = flen.first().copied().unwrap_or(t as i64);
        let feats = Tensor::from_array(([1usize, 128, t], feats.to_vec()))
            .map_err(|_| MlError::Backend("encoder input"))?;
        let len = Tensor::from_array(([1usize], vec![flen]))
            .map_err(|_| MlError::Backend("encoder input"))?;
        drop(out);
        let out = s
            .enc
            .run(ort::inputs!["audio_signal" => feats, "length" => len])
            .map_err(|_| MlError::Backend("encoder"))?;
        let (shape, data) = out["outputs"]
            .try_extract_tensor::<f32>()
            .map_err(|_| MlError::Backend("encoder output"))?;
        if shape.get(1).copied() != Some(ENC_DIM as i64) {
            return Err(MlError::Backend("encoder shape"));
        }
        let stride = *shape.get(2).ok_or(MlError::Backend("encoder shape"))? as usize;
        let (_, elen) = out["encoded_lengths"]
            .try_extract_tensor::<i64>()
            .map_err(|_| MlError::Backend("encoder output"))?;
        let frames = (elen.first().copied().unwrap_or(stride as i64).max(0) as usize).min(stride);
        Ok(Encoded {
            data: data.to_vec(),
            frames,
            stride,
        })
    }

    /// Joint logits for `targets` (first one = the start symbol, blank) against the given
    /// frames: `[frames][targets][vocab + durations]`.
    fn joint(
        &self,
        enc: &[f32],
        frames: usize,
        targets: &[u32],
        states: Option<(&[f32], &[f32])>,
    ) -> Result<JointOut, MlError> {
        let mut s = self.s.lock().unwrap_or_else(|p| p.into_inner());
        let _ort = crate::runtime::inside(false);
        let e = Tensor::from_array(([1usize, ENC_DIM, frames], enc.to_vec()))
            .map_err(|_| MlError::Backend("joint input"))?;
        let tg = Tensor::from_array((
            [1usize, targets.len()],
            targets.iter().map(|&t| t as i32).collect::<Vec<i32>>(),
        ))
        .map_err(|_| MlError::Backend("joint input"))?;
        let tl = Tensor::from_array(([1usize], vec![targets.len() as i32]))
            .map_err(|_| MlError::Backend("joint input"))?;
        let zeros = vec![0f32; 2 * HIDDEN];
        let (h, c) = states.unwrap_or((&zeros, &zeros));
        let h = Tensor::from_array(([2usize, 1, HIDDEN], h.to_vec()))
            .map_err(|_| MlError::Backend("joint input"))?;
        let c = Tensor::from_array(([2usize, 1, HIDDEN], c.to_vec()))
            .map_err(|_| MlError::Backend("joint input"))?;
        let out = s
            .joint
            .run(ort::inputs![
                "encoder_outputs" => e,
                "targets" => tg,
                "target_length" => tl,
                "input_states_1" => h,
                "input_states_2" => c
            ])
            .map_err(|_| MlError::Backend("joint"))?;
        let (shape, logits) = out["outputs"]
            .try_extract_tensor::<f32>()
            .map_err(|_| MlError::Backend("joint output"))?;
        let width = *shape.get(3).ok_or(MlError::Backend("joint shape"))? as usize;
        let (_, h1) = out["output_states_1"]
            .try_extract_tensor::<f32>()
            .map_err(|_| MlError::Backend("joint output"))?;
        let (_, c1) = out["output_states_2"]
            .try_extract_tensor::<f32>()
            .map_err(|_| MlError::Backend("joint output"))?;
        Ok((logits.to_vec(), width, h1.to_vec(), c1.to_vec()))
    }

    fn column(enc: &Encoded, t: usize) -> Vec<f32> {
        (0..ENC_DIM).map(|d| enc.data[d * enc.stride + t]).collect()
    }

    /// TDT greedy decode (max 10 tokens per frame), token ids with frames.
    pub fn greedy(&self, enc: &Encoded) -> Result<Vec<TokenHit>, MlError> {
        let v = self.blank as usize + 1;
        let mut out = Vec::new();
        let mut h = vec![0f32; 2 * HIDDEN];
        let mut c = vec![0f32; 2 * HIDDEN];
        let mut last = self.blank;
        let (mut t, mut emitted) = (0usize, 0usize);
        while t < enc.frames {
            let col = Self::column(enc, t);
            let (logits, width, h1, c1) = self.joint(&col, 1, &[last], Some((&h, &c)))?;
            if width < v + DURATIONS.len() {
                return Err(MlError::Backend("joint width"));
            }
            let row = &logits[..width];
            let tok = row[..v]
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map_or(self.blank, |(i, _)| i as u32);
            let dur = row[v..v + DURATIONS.len()]
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map_or(0, |(i, _)| DURATIONS[i]);
            if tok != self.blank {
                out.push(TokenHit {
                    id: tok,
                    frame: t,
                    duration: dur,
                });
                last = tok;
                h = h1;
                c = c1;
                emitted += 1;
            }
            if dur > 0 {
                t += dur;
                emitted = 0;
            } else if tok == self.blank || emitted >= 10 {
                t += 1;
                emitted = 0;
            }
        }
        Ok(out)
    }

    /// log P(`tokens` | audio) under TDT, summed over all alignments (forward
    /// algorithm). `-inf` when no alignment fits (more tokens than the frames allow).
    pub fn align_ll(&self, enc: &Encoded, tokens: &[u32]) -> Result<f64, MlError> {
        let t_n = enc.frames;
        let u_n = tokens.len();
        if t_n == 0 {
            return Ok(if u_n == 0 { 0.0 } else { f64::NEG_INFINITY });
        }
        let v = self.blank as usize + 1;
        if t_n * (u_n + 1) * (v + DURATIONS.len()) > MAX_GRID {
            return Err(MlError::Invalid("alignment grid too large"));
        }
        let mut targets = Vec::with_capacity(u_n + 1);
        targets.push(self.blank);
        targets.extend_from_slice(tokens);
        // the valid frames, contiguous
        let enc_valid: Vec<f32> = if t_n == enc.stride {
            enc.data.clone()
        } else {
            (0..ENC_DIM)
                .flat_map(|d| {
                    enc.data[d * enc.stride..d * enc.stride + t_n]
                        .iter()
                        .copied()
                })
                .collect()
        };
        let (logits, width, _, _) = self.joint(&enc_valid, t_n, &targets, None)?;
        if width < v + DURATIONS.len() || logits.len() < t_n * (u_n + 1) * width {
            return Err(MlError::Backend("joint grid"));
        }
        Ok(forward(&logits, width, v, self.blank as usize, t_n, tokens))
    }
}

/// The TDT forward algorithm over a joint grid `[t_n][tokens + 1][width]` (vocabulary
/// `v` logits then the duration logits), NeMo's way: moves land strictly inside the
/// audio (`t + d < T`) and the path ends with a blank whose duration reaches exactly `T`
/// once every token is out. Returns log P(tokens | audio).
pub fn forward(
    logits: &[f32],
    width: usize,
    v: usize,
    blank: usize,
    t_n: usize,
    tokens: &[u32],
) -> f64 {
    let u_n = tokens.len();
    // NeMo's TDT likelihood: moves land strictly inside the audio (t + d < T), and
    // the path ends with a blank whose duration reaches exactly T after all tokens.
    let ninf = f64::NEG_INFINITY;
    let mut alpha = vec![ninf; t_n * (u_n + 1)];
    let at = |t: usize, u: usize| t * (u_n + 1) + u;
    alpha[at(0, 0)] = 0.0;
    let mut ll = ninf;
    for t in 0..t_n {
        for u in 0..=u_n {
            let a = alpha[at(t, u)];
            if a == ninf {
                continue;
            }
            let row = &logits[(t * (u_n + 1) + u) * width..][..width];
            let lt = log_softmax(&row[..v]);
            let ld = log_softmax(&row[v..v + DURATIONS.len()]);
            for (k, &d) in DURATIONS.iter().enumerate() {
                let t2 = t + d;
                // blank always advances (a duration-0 blank would not move)
                if d > 0 {
                    let step = a + lt[blank] + ld[k];
                    if t2 < t_n {
                        let i = at(t2, u);
                        alpha[i] = logaddexp(alpha[i], step);
                    } else if t2 == t_n && u == u_n {
                        ll = logaddexp(ll, step);
                    }
                }
                if u < u_n && t2 < t_n {
                    let i = at(t2, u + 1);
                    alpha[i] = logaddexp(alpha[i], a + lt[tokens[u] as usize] + ld[k]);
                }
            }
        }
    }
    ll
}

/// Tokens grouped into words on `▁` (a bare `▁` starts the next word), with sample spans
/// (16 kHz) relative to the encoded audio.
pub fn words_of<'a>(
    piece: impl Fn(u32) -> &'a str,
    hits: &[TokenHit],
) -> Vec<(String, Vec<u32>, usize, usize)> {
    let mut out: Vec<(String, Vec<u32>, usize, usize)> = Vec::new();
    let mut pending = false;
    for h in hits {
        let p = piece(h.id);
        let marked = p.starts_with('▁') || pending;
        let text = p.trim_start_matches('▁');
        let (a, b) = (
            h.frame * FRAME_SAMPLES,
            (h.frame + h.duration.max(1)) * FRAME_SAMPLES,
        );
        if text.is_empty() {
            pending = true;
            // the marker token still belongs to the next word's tokens
            match out.last_mut() {
                Some(w) if w.0.is_empty() => w.1.push(h.id),
                _ => out.push((String::new(), vec![h.id], a, b)),
            }
            continue;
        }
        let starts = marked && !out.last().is_some_and(|w| w.0.is_empty());
        let placeholder = out.last().is_some_and(|w| w.0.is_empty());
        match out.last_mut() {
            Some(w) if placeholder && !p.starts_with('▁') => {
                w.0.push_str(text);
                w.1.push(h.id);
                w.3 = b;
            }
            Some(w) if !starts => {
                w.0.push_str(text);
                w.1.push(h.id);
                w.3 = w.3.max(b);
            }
            _ => out.push((text.to_owned(), vec![h.id], a, b)),
        }
        pending = false;
    }
    // a placeholder followed by a ▁-word: its marker ids go to that word
    let mut merged: Vec<(String, Vec<u32>, usize, usize)> = Vec::with_capacity(out.len());
    let mut carry: Vec<u32> = Vec::new();
    for mut w in out {
        if w.0.is_empty() {
            carry.extend(w.1);
            continue;
        }
        if !carry.is_empty() {
            let mut ids = std::mem::take(&mut carry);
            ids.extend(w.1);
            w.1 = ids;
        }
        merged.push(w);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: u32, frame: usize) -> TokenHit {
        TokenHit {
            id,
            frame,
            duration: 1,
        }
    }

    #[test]
    fn words_group_on_markers_and_keep_every_token() {
        let pieces = ["▁Hi", ",", "▁", "5", "0", "▁", "▁x", "▁so"];
        let p = |id: u32| pieces[id as usize];
        let w = words_of(
            p,
            &[
                hit(0, 0),
                hit(1, 1),
                hit(2, 2),
                hit(3, 2),
                hit(4, 3),
                hit(5, 4),
                hit(6, 5),
                hit(7, 6),
            ],
        );
        let texts: Vec<&str> = w.iter().map(|w| w.0.as_str()).collect();
        assert_eq!(texts, ["Hi,", "50", "x", "so"]);
        let ids: Vec<u32> = w.iter().flat_map(|w| w.1.iter().copied()).collect();
        assert_eq!(ids, (0..8).collect::<Vec<u32>>(), "no token id lost");
        assert_eq!((w[1].2, w[1].3), (2 * FRAME_SAMPLES, 4 * FRAME_SAMPLES));
    }

    /// The forward sum equals brute-force enumeration of every TDT path.
    #[test]
    fn forward_equals_enumeration() {
        // vocabulary {a=0, b=1, blank=2}, durations 0..4; deterministic pseudo-random logits
        let (v, blank, width) = (3usize, 2usize, 3 + DURATIONS.len());
        for (t_n, tokens) in [
            (1usize, vec![]),
            (2, vec![0u32]),
            (3, vec![0, 1]),
            (5, vec![1, 0, 1]),
            (2, vec![0, 1, 0]),
        ] {
            let u_n = tokens.len();
            let mut seed = 7u32;
            let logits: Vec<f32> = (0..t_n * (u_n + 1) * width)
                .map(|_| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                    ((seed >> 16) % 1000) as f32 / 250.0 - 2.0
                })
                .collect();
            let lp = |t: usize, u: usize| {
                let row = &logits[(t * (u_n + 1) + u) * width..][..width];
                (log_softmax(&row[..v]), log_softmax(&row[v..]))
            };
            // enumerate paths recursively
            fn walk(
                t: usize,
                u: usize,
                acc: f64,
                n: (usize, usize, usize),
                toks: &[u32],
                lp: &dyn Fn(usize, usize) -> (Vec<f64>, Vec<f64>),
                out: &mut Vec<f64>,
            ) {
                let (t_n, u_n, blank) = n;
                let (lt, ld) = lp(t, u);
                for (k, &d) in DURATIONS.iter().enumerate() {
                    if d > 0 {
                        let s = acc + lt[blank] + ld[k];
                        if t + d < t_n {
                            walk(t + d, u, s, n, toks, lp, out);
                        } else if t + d == t_n && u == u_n {
                            out.push(s);
                        }
                    }
                    if u < u_n && t + d < t_n {
                        walk(
                            t + d,
                            u + 1,
                            acc + lt[toks[u] as usize] + ld[k],
                            n,
                            toks,
                            lp,
                            out,
                        );
                    }
                }
            }
            let mut paths = Vec::new();
            walk(0, 0, 0.0, (t_n, u_n, blank), &tokens, &lp, &mut paths);
            let want = paths
                .iter()
                .fold(f64::NEG_INFINITY, |a, &b| logaddexp(a, b));
            let got = forward(&logits, width, v, blank, t_n, &tokens);
            if want.is_finite() {
                assert!(
                    (got - want).abs() < 1e-9,
                    "T {t_n} U {u_n}: {got} vs {want}"
                );
            } else {
                assert_eq!(got, f64::NEG_INFINITY);
            }
        }
    }
}
