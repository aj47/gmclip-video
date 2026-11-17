//! Sentence embeddings for search by meaning (L.02): all-MiniLM-L6-v2 (Apache-2.0) on the
//! process's one ONNX Runtime (SPEC §9: no new runtime), 384-d, mean-pooled over the
//! attention mask and L2-normalised, as sentence-transformers computes them.
//!
//! The tokenizer is BERT's uncased WordPiece, written here rather than pulled in as a
//! crate: clean (control characters out), CJK characters split out, lowercase and strip
//! accents, split on punctuation, then greedy longest-match WordPiece over the pinned
//! `vocab.txt` (`##` continuations, `[UNK]` for a word with no split or over 100
//! characters). Accent stripping folds the Latin letters (U+00C0–U+024F) and drops
//! combining marks; other scripts pass through as written. Golden token ids and vectors
//! from the reference implementation are in the tests.

use std::collections::HashMap;

use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;

use crate::MlError;
use crate::models::{MINILM_L6_V2, VerifiedModel};
use crate::runtime::OrtRuntime;

/// Embedding width.
pub const DIM: usize = 384;
/// Word pieces per text, `[CLS]` and `[SEP]` included (longer texts are cut).
pub const MAX_TOKENS: usize = 128;

/// BERT's uncased WordPiece over a vocabulary file.
#[derive(Debug, Clone)]
pub struct WordPiece {
    vocab: HashMap<String, u32>,
    cls: u32,
    sep: u32,
    unk: u32,
}

fn is_control(c: char) -> bool {
    if matches!(c, '\t' | '\n' | '\r') {
        return false;
    }
    c.is_control()
        || matches!(c, '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}')
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x20000..=0x2A6DF | 0x2A700..=0x2B73F
        | 0x2B740..=0x2B81F | 0x2B820..=0x2CEAF | 0xF900..=0xFAFF | 0x2F800..=0x2FA1F)
}

fn is_punct(c: char) -> bool {
    let u = c as u32;
    (33..=47).contains(&u)
        || (58..=64).contains(&u)
        || (91..=96).contains(&u)
        || (123..=126).contains(&u)
        // general punctuation (Unicode P*): the ones speech-to-text and copy-paste produce
        || matches!(c, '¡' | '§' | '«' | '¶' | '·' | '»' | '¿')
        || (0x2010..=0x2027).contains(&u)
        || (0x2030..=0x205E).contains(&u)
        || (0x3001..=0x3003).contains(&u)
        || (0x3008..=0x3011).contains(&u)
        || (0xFF01..=0xFF0F).contains(&u)
}

fn is_combining(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

/// A lowercase letter without its accent (Latin; NFD + drop Mn, as BERT does).
fn fold(c: char) -> &'static str {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'ǎ' | 'ǻ' => "a",
        'æ' => "æ",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' | 'ǧ' => "g",
        'ĥ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ǐ' => "i",
        'ĵ' => "j",
        'ķ' | 'ǩ' => "k",
        'ĺ' | 'ļ' | 'ľ' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ō' | 'ŏ' | 'ő' | 'ǒ' | 'ǫ' => "o",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => "s",
        'ţ' | 'ť' | 'ț' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'ǔ' | 'ǖ' | 'ǘ' | 'ǚ' | 'ǜ' => {
            "u"
        }
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => "",
    }
}

impl WordPiece {
    /// From a `vocab.txt` (one piece per line; the line number is the id).
    pub fn from_vocab(text: &str) -> Result<WordPiece, MlError> {
        let vocab: HashMap<String, u32> = text
            .lines()
            .enumerate()
            .map(|(i, l)| (l.to_owned(), i as u32))
            .collect();
        let id = |t: &str| vocab.get(t).copied().ok_or(MlError::Invalid("vocabulary"));
        Ok(WordPiece {
            cls: id("[CLS]")?,
            sep: id("[SEP]")?,
            unk: id("[UNK]")?,
            vocab,
        })
    }

    /// BERT's basic tokenizer (uncased): the words WordPiece then splits.
    pub fn basic(text: &str) -> Vec<String> {
        let mut spaced = String::with_capacity(text.len() + 16);
        for c in text.chars() {
            if c == '\0' || c == '\u{FFFD}' || is_control(c) {
                continue;
            }
            if c.is_whitespace() {
                spaced.push(' ');
            } else if is_cjk(c) {
                spaced.push(' ');
                spaced.push(c);
                spaced.push(' ');
            } else {
                spaced.push(c);
            }
        }
        let mut out = Vec::new();
        for word in spaced.split(' ').filter(|w| !w.is_empty()) {
            let mut cur = String::new();
            for c in word.chars().flat_map(char::to_lowercase) {
                if is_combining(c) {
                    continue;
                }
                if is_punct(c) {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                    out.push(c.to_string());
                    continue;
                }
                match fold(c) {
                    "" => cur.push(c),
                    f => cur.push_str(f),
                }
            }
            if !cur.is_empty() {
                out.push(cur);
            }
        }
        out
    }

    /// Token ids with `[CLS]` and `[SEP]`, at most `max` in all.
    pub fn encode(&self, text: &str, max: usize) -> Vec<u32> {
        let mut ids = vec![self.cls];
        'words: for w in Self::basic(text) {
            if ids.len() + 1 >= max {
                break;
            }
            let chars: Vec<char> = w.chars().collect();
            if chars.len() > 100 {
                ids.push(self.unk);
                continue;
            }
            let mut pieces = Vec::new();
            let mut start = 0;
            while start < chars.len() {
                let mut end = chars.len();
                let mut found = None;
                while start < end {
                    let s: String = chars[start..end].iter().collect();
                    let s = if start > 0 { format!("##{s}") } else { s };
                    if let Some(&id) = self.vocab.get(&s) {
                        found = Some(id);
                        break;
                    }
                    end -= 1;
                }
                match found {
                    Some(id) => pieces.push(id),
                    None => {
                        ids.push(self.unk);
                        continue 'words;
                    }
                }
                start = end;
            }
            for p in pieces {
                if ids.len() + 1 >= max {
                    break 'words;
                }
                ids.push(p);
            }
        }
        ids.push(self.sep);
        ids
    }
}

/// The embedding model session.
pub struct SentenceEmbedder {
    session: Session,
    pub tokenizer: WordPiece,
}

impl std::fmt::Debug for SentenceEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SentenceEmbedder")
    }
}

impl SentenceEmbedder {
    /// Loads the pinned model (`ModelCache::verify(&MINILM_L6_V2)` first).
    pub fn load(rt: &OrtRuntime, model: &VerifiedModel) -> Result<Self, MlError> {
        if model.entry.id != MINILM_L6_V2.id {
            return Err(MlError::Invalid("not the sentence-embedding model"));
        }
        let vocab = std::fs::read_to_string(model.file("vocab.txt"))
            .map_err(|_| MlError::Load("vocabulary"))?;
        let tokenizer = WordPiece::from_vocab(&vocab)?;
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(1, 4));
        let _ort = crate::runtime::inside(true);
        let session = Session::builder(rt.environment())
            .and_then(|b| {
                b.with_optimization_level(GraphOptimizationLevel::Level3)
                    .map_err(ort::Error::<()>::from)
            })
            .and_then(|b| {
                b.with_intra_threads(threads)
                    .map_err(ort::Error::<()>::from)
            })
            .and_then(|mut b| b.commit_from_file(model.file("model.onnx")))
            .map_err(|_| MlError::Backend("loading the embedding model"))?;
        Ok(Self { session, tokenizer })
    }

    /// One unit vector per text (texts batched as given; cut at [`MAX_TOKENS`]).
    pub fn embed(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>, MlError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let enc: Vec<Vec<u32>> = texts
            .iter()
            .map(|t| self.tokenizer.encode(t, MAX_TOKENS))
            .collect();
        let b = enc.len();
        let t = enc.iter().map(Vec::len).max().unwrap_or(2);
        let mut ids = vec![0i64; b * t];
        let mut mask = vec![0i64; b * t];
        for (i, e) in enc.iter().enumerate() {
            for (j, &id) in e.iter().enumerate() {
                ids[i * t + j] = i64::from(id);
                mask[i * t + j] = 1;
            }
        }
        let types = vec![0i64; b * t];
        let shape = [b as i64, t as i64];
        let _ort = crate::runtime::inside(false);
        let tensor = |v: Vec<i64>| {
            Tensor::from_array((shape, v.into_boxed_slice()))
                .map_err(|_| MlError::Backend("embedding input"))
        };
        let outputs = self
            .session
            .run(ort::inputs![
                "input_ids" => tensor(ids)?,
                "attention_mask" => tensor(mask.clone())?,
                "token_type_ids" => tensor(types)?,
            ])
            .map_err(|_| MlError::Backend("embedding"))?;
        let (oshape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|_| MlError::Backend("embedding output"))?;
        if oshape.len() != 3 || oshape[2] as usize != DIM || oshape[1] as usize != t {
            return Err(MlError::Backend("embedding output shape"));
        }
        let mut out = Vec::with_capacity(b);
        for i in 0..b {
            let mut v = vec![0f32; DIM];
            let mut n = 0f32;
            for j in 0..t {
                if mask[i * t + j] == 0 {
                    continue;
                }
                n += 1.0;
                let row = &data[(i * t + j) * DIM..(i * t + j + 1) * DIM];
                for (a, x) in v.iter_mut().zip(row) {
                    *a += x;
                }
            }
            let n = n.max(1.0);
            let mut norm = 0f32;
            for a in &mut v {
                *a /= n;
                norm += *a * *a;
            }
            let norm = norm.sqrt().max(1e-12);
            for a in &mut v {
                *a /= norm;
            }
            out.push(v);
        }
        Ok(out)
    }
}

/// The dot product of two unit vectors (their cosine).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_tokenizer_lowercases_splits_punctuation_and_folds_accents() {
        assert_eq!(
            WordPiece::basic("GPU prices went up again, I can't believe it!"),
            [
                "gpu", "prices", "went", "up", "again", ",", "i", "can", "'", "t", "believe", "it",
                "!"
            ]
        );
        assert_eq!(
            WordPiece::basic("Café naïve résumé — “quoted” text… 67\u{200B}seconds"),
            [
                "cafe",
                "naive",
                "resume",
                "—",
                "“",
                "quoted",
                "”",
                "text",
                "…",
                "67seconds"
            ]
        );
        assert_eq!(WordPiece::basic("日本 ok"), ["日", "本", "ok"]);
    }

    #[test]
    fn wordpiece_greedy_longest_match() {
        let vocab = "[PAD]\n[UNK]\n[CLS]\n[SEP]\ngpu\n##s\nprice\n##s\nun\n##aff\n##able\n";
        let wp = WordPiece::from_vocab(vocab).unwrap();
        // ids are line numbers; the second `##s` (line 7) overrides the first in the map
        assert_eq!(
            wp.encode("GPUs prices unaffable xyz", 128),
            [2, 4, 7, 6, 7, 8, 9, 10, 1, 3]
        );
        // cut at max (CLS + 2 + SEP)
        assert_eq!(wp.encode("GPUs prices", 4), [2, 4, 7, 3]);
    }

    /// Golden ids and vectors from the reference (HF `tokenizers` BertWordPiece +
    /// onnxruntime + mean pooling) on the pinned files. `--features lab`.
    #[cfg(feature = "lab")]
    #[test]
    fn matches_the_reference_on_the_pinned_model() {
        use crate::models::ModelCache;
        let cache = ModelCache::default_cache().unwrap();
        let rt = OrtRuntime::init(&cache).unwrap();
        let m = cache.verify(&MINILM_L6_V2).unwrap();
        let mut e = SentenceEmbedder::load(rt, &m).unwrap();
        let cases: [(&str, &[u32], [f32; 6]); 3] = [
            (
                "Hello world",
                &[101, 7592, 2088, 102],
                [-0.03448, 0.03102, 0.00674, 0.02611, -0.03936, -0.1603],
            ),
            (
                "GPU prices went up again, I can't believe it!",
                &[
                    101, 14246, 2226, 7597, 2253, 2039, 2153, 1010, 1045, 2064, 1005, 1056, 2903,
                    2009, 999, 102,
                ],
                [-0.05332, -0.02955, 0.00849, 0.06469, 0.0146, -0.10944],
            ),
            (
                "Café naïve résumé — “quoted” text… 67 seconds",
                &[
                    101, 7668, 15743, 13746, 1517, 1523, 9339, 1524, 3793, 1529, 6163, 3823, 102,
                ],
                [-0.06418, 0.03774, 0.01032, 0.05124, 0.06652, 0.0755],
            ),
        ];
        let texts: Vec<&str> = cases.iter().map(|c| c.0).collect();
        let vs = e.embed(&texts).unwrap();
        for ((t, ids, head), v) in cases.iter().zip(&vs) {
            assert_eq!(e.tokenizer.encode(t, MAX_TOKENS), *ids, "{t}");
            for (a, b) in v.iter().zip(head) {
                assert!((a - b).abs() < 2e-4, "{t}: {:?} vs {head:?}", &v[..6]);
            }
        }
        // meaning: the GPU sentence is nearer a paraphrase than an unrelated one
        let q = e
            .embed(&["graphics cards got more expensive", "my cat sleeps all day"])
            .unwrap();
        assert!(cosine(&vs[1], &q[0]) > cosine(&vs[1], &q[1]) + 0.2);
    }
}
