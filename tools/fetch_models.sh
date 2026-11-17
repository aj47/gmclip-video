#!/bin/sh
# Dev-only: fill the lab model cache (~/Library/Caches/gmclip) with the pinned files of
# spec/model-register.md, checking every SHA-256. Never used by the app (R-ML-3: release
# builds download from our CDN). Idempotent; a file with the wrong hash is deleted.
set -eu
ROOT="${GMCLIP_CACHE:-$HOME/Library/Caches/gmclip}"
HF=https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce
SV=https://raw.githubusercontent.com/snakers4/silero-vad/v6.2.1

fetch() { # dir name url sha256
  mkdir -p "$ROOT/$1"
  f="$ROOT/$1/$2"
  if [ -f "$f" ] && [ "$(shasum -a 256 "$f" | cut -d' ' -f1)" = "$4" ]; then
    echo "ok      $1/$2"; return
  fi
  rm -f "$f"   # a wrong-hash file never stays behind, even if the fetch fails
  curl -fsSL -o "$f.partial" "$3"
  got=$(shasum -a 256 "$f.partial" | cut -d' ' -f1)
  if [ "$got" != "$4" ]; then rm -f "$f.partial"; echo "HASH MISMATCH $1/$2: $got" >&2; exit 1; fi
  mv "$f.partial" "$f"; echo "fetched $1/$2"
}

P=models/parakeet-tdt-0.6b-v3-int8
fetch $P encoder-model.int8.onnx "$HF/encoder-model.int8.onnx" 6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09
fetch $P decoder_joint-model.int8.onnx "$HF/decoder_joint-model.int8.onnx" eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70
fetch $P vocab.txt "$HF/vocab.txt" d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d
fetch $P config.json "$HF/config.json" 666903c76b9798caf2c210afd4f6cd60b08a8dbf9800ec8d7a3bc0d2148ac466
A=models/parakeet-tdt-0.6b-v3-aligner
fetch $A nemo128.onnx "$HF/nemo128.onnx" a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f
fetch $A decoder_joint-model.onnx "$HF/decoder_joint-model.onnx" e978ddf6688527182c10fde2eb4b83068421648985ef23f7a86be732be8706c1
fetch models/silero-vad-v6.2.1 silero_vad.onnx "$SV/src/silero_vad/data/silero_vad.onnx" 1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3
fetch models/silero-vad-v6.2.1 LICENSE "$SV/LICENSE" 2e63e9a38b6e8fc0c7bc37ce174caca1862870856c6daf5697cfb785e925520b

# all-MiniLM-L6-v2 (Apache-2.0): search by meaning (L.02).
ML=https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/1110a243fdf4706b3f48f1d95db1a4f5529b4d41
fetch models/all-minilm-l6-v2 model.onnx "$ML/onnx/model.onnx" 6fd5d72fe4589f189f8ebc006442dbb529bb7ce38f8082112682524616046452
fetch models/all-minilm-l6-v2 vocab.txt "$ML/vocab.txt" 07eced375cec144d27c900241f3e339478dec958f92fddbc551f295c992038a3

# ONNX Runtime 1.30.0 (macOS arm64): the dylib only, from Microsoft's release archive.
D="$ROOT/models/onnxruntime-1.30.0"; LIB=libonnxruntime.1.30.0.dylib
LIBSHA=bcc9110f9d638a119de2db7afb3ba9a1da8085f0cb3401e1c48ae1caf450b6fa
if [ -f "$D/$LIB" ] && [ "$(shasum -a 256 "$D/$LIB" | cut -d' ' -f1)" = "$LIBSHA" ]; then
  echo "ok      models/onnxruntime-1.30.0/$LIB"
else
  [ "$(uname -s)-$(uname -m)" = "Darwin-arm64" ] || { echo "no ORT pinned for $(uname -s)-$(uname -m)" >&2; exit 1; }
  tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
  curl -fsSL -o "$tmp/ort.tgz" https://github.com/microsoft/onnxruntime/releases/download/v1.30.0/onnxruntime-osx-arm64-1.30.0.tgz
  [ "$(shasum -a 256 "$tmp/ort.tgz" | cut -d' ' -f1)" = 6ebb5062a934537c352937821f9fe9718e7de1a2db1122a93dd363ffd53a7012 ] || { echo "ORT archive hash mismatch" >&2; exit 1; }
  tar -xzf "$tmp/ort.tgz" -C "$tmp" "./onnxruntime-osx-arm64-1.30.0/lib/$LIB" ./onnxruntime-osx-arm64-1.30.0/LICENSE ./onnxruntime-osx-arm64-1.30.0/ThirdPartyNotices.txt
  mkdir -p "$D"
  [ "$(shasum -a 256 "$tmp/onnxruntime-osx-arm64-1.30.0/lib/$LIB" | cut -d' ' -f1)" = "$LIBSHA" ] || { echo "ORT dylib hash mismatch" >&2; exit 1; }
  mv "$tmp/onnxruntime-osx-arm64-1.30.0/lib/$LIB" "$D/$LIB"
  mv "$tmp/onnxruntime-osx-arm64-1.30.0/LICENSE" "$tmp/onnxruntime-osx-arm64-1.30.0/ThirdPartyNotices.txt" "$D/"
  echo "fetched models/onnxruntime-1.30.0/$LIB"
fi
du -sh "$ROOT"
