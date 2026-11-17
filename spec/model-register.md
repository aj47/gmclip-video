# Model register (SPEC §15.3)

One row per runtime file gmclip loads. The SHA-256 and size of every file are pinned in
`crates/gmclip-ml/src/models.rs` (`REGISTER`); a test (`the_spec_register_lists_every_pin`)
fails if a pin there is missing here. Models are **never committed to git**: the lab cache
(`~/Library/Caches/gmclip/`) is filled by `tools/fetch_models.sh`, which checks every hash;
release builds will download from our CDN (R-ML-3, B13) with the same pins.

Counsel sign-off is an M3 item (§15.3); the "Sign-off" column stays empty until then.

| Model | File | SHA-256 | Size (bytes) | Weights licence | Code licence | Training data and terms | Commercial use of derived weights | Source (URL + commit) | Export script | Sign-off |
|---|---|---|---|---|---|---|---|---|---|---|
| Parakeet TDT 0.6B v3, int8 | `encoder-model.int8.onnx` | `6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09` | 652183999 | CC-BY-4.0 (NVIDIA) | parakeet-rs 0.4.1: MIT OR Apache-2.0 | NVIDIA Granary (per the nvidia/parakeet-tdt-0.6b-v3 model card: ~670k h incl. YTC, YODAS, LibriLight and others; per-dataset terms to be listed for counsel) | Yes under CC-BY-4.0 with attribution (to confirm with counsel, Q-list) | huggingface.co/nvidia/parakeet-tdt-0.6b-v3; ONNX export huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx @ `8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce` | istupakov's (onnx-asr); ours replaces it when the CDN lands (R-ML-3 NOTICE) | — |
| | `decoder_joint-model.int8.onnx` | `eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70` | 18202004 | CC-BY-4.0 | 〃 | 〃 | 〃 | 〃 | 〃 | — |
| | `vocab.txt` | `d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d` | 93939 | CC-BY-4.0 | 〃 | 〃 | 〃 | 〃 | 〃 | — |
| | `config.json` | `666903c76b9798caf2c210afd4f6cd60b08a8dbf9800ec8d7a3bc0d2148ac466` | 97 | CC-BY-4.0 | 〃 | 〃 | 〃 | 〃 | 〃 | — |
| Parakeet TDT 0.6B v3, aligner (decision 0006) | `nemo128.onnx` (NeMo preprocessor) | `a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f` | 139764 | CC-BY-4.0 | gmclip (own TDT forward algorithm) | as above | as above | istupakov/parakeet-tdt-0.6b-v3-onnx @ `8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce` | istupakov's | — |
| | `decoder_joint-model.onnx` (fp32) | `e978ddf6688527182c10fde2eb4b83068421648985ef23f7a86be732be8706c1` | 72520893 | CC-BY-4.0 | 〃 | 〃 | 〃 | 〃 | 〃 | — |
| Silero VAD v6.2.1 | `silero_vad.onnx` | `1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3` | 2327524 | MIT | MIT | Silero's own corpora (not itemised upstream; ask) | Yes (MIT) | github.com/snakers4/silero-vad tag `v6.2.1`, `src/silero_vad/data/silero_vad.onnx` | upstream ONNX, unmodified | — |
| | `LICENSE` | `2e63e9a38b6e8fc0c7bc37ce174caca1862870856c6daf5697cfb785e925520b` | 1075 | — | — | — | — | 〃 `LICENSE` | — | — |
| ONNX Runtime 1.30.0 (macOS arm64) | `libonnxruntime.1.30.0.dylib` | `bcc9110f9d638a119de2db7afb3ba9a1da8085f0cb3401e1c48ae1caf450b6fa` | 43879424 | n/a | MIT (+ `ThirdPartyNotices.txt`) | n/a | n/a | github.com/microsoft/onnxruntime release `v1.30.0` (commit `f2c39fe2f838cf35ce7da92824f5a5e3ee6e88a7`), `onnxruntime-osx-arm64-1.30.0.tgz` sha256 `6ebb5062a934537c352937821f9fe9718e7de1a2db1122a93dd363ffd53a7012` | Microsoft's release build; must be re-signed with ours inside the bundle (R-LOAD-1) | — |
| ONNX Runtime 1.30.0 (Windows x64, CPU) | `onnxruntime.dll` | `7e39e2bdbba836d98071ef28620735ba36a47c554cf794585269aecc50fab0da` | 16462648 | n/a | MIT (+ `ThirdPartyNotices.txt`) | n/a | n/a | github.com/microsoft/onnxruntime release `v1.30.0` (commit `f2c39fe2f838cf35ce7da92824f5a5e3ee6e88a7`), `onnxruntime-win-x64-1.30.0.zip` sha256 `c6ba983baf5681af108599675d2a89c2d145512d02de28aed0bff177cd0ba949` | Microsoft's release build, Authenticode-signed by Microsoft Corporation (checked when pinned, 2026-10-16) | — |
| all-MiniLM-L6-v2 (search by meaning, L.02) | `model.onnx` (fp32) | `6fd5d72fe4589f189f8ebc006442dbb529bb7ce38f8082112682524616046452` | 90405214 | Apache-2.0 (sentence-transformers) | gmclip (own WordPiece tokenizer, mean pooling) | ~1B sentence pairs per the model card: Reddit comments 2015–18, S2ORC, WikiAnswers, PAQ, Stack Exchange, MS MARCO, GooAQ, Yahoo Answers, CodeSearchNet, SearchQA, ELI5, SNLI + MultiNLI, WikiHow, Natural Questions, TriviaQA, Flickr 30k, QQP, SPECTER, AltLex, Simple Wiki, sentence compression; base `nreimers/MiniLM-L6-H384-uncased` (Microsoft MiniLM, MIT); per-dataset terms to be listed for counsel | Yes under Apache-2.0 (dataset terms to confirm with counsel, Q-list) | huggingface.co/sentence-transformers/all-MiniLM-L6-v2 @ `1110a243fdf4706b3f48f1d95db1a4f5529b4d41` (`onnx/model.onnx`, sha256 = the repo's LFS oid) | upstream ONNX, unmodified | — |
| | `vocab.txt` | `07eced375cec144d27c900241f3e339478dec958f92fddbc551f295c992038a3` | 231508 | Apache-2.0 | 〃 | 〃 | 〃 | 〃 (git blob `fb140275c155a9c7c5a3b3e0e77a9e839594a938`) | — | — |

Pending rows: ORT for x86_64 macOS, Windows arm64 and Linux (built from source in CI
where no official binary exists, SPEC §9); Whisper (after M5); MediaPipe selfie, BlazeFace
(M2+).
