"""Dev-only: Silero VAD speech probabilities with Silero's own OnnxWrapper framing
(64-sample context + 512, state carried) in Python onnxruntime, for lab_vad's check.

usage: python -I tools/silero_ref.py <silero_vad.onnx> <audio|pcm.f32> <seconds> <out.json>
A `.f32` input is raw 16 kHz mono float32 (lab_vad writes the exact PCM it fed the model
to $GMCLIP_SILERO_DUMP); anything else is decoded with /opt/homebrew/bin/ffmpeg.
"""
import json, subprocess, sys
import numpy as np
import onnxruntime as ort

model, audio, secs, out = sys.argv[1], sys.argv[2], float(sys.argv[3]), sys.argv[4]
pcm = open(audio, "rb").read()[: int(secs * 16000) * 4] if audio.endswith(".f32") else subprocess.run(["/opt/homebrew/bin/ffmpeg", "-v", "error", "-nostdin", "-t", str(secs), "-i", audio,
                      "-map", "0:a:0", "-ac", "1", "-ar", "16000", "-f", "f32le", "-"],
                     check=True, capture_output=True).stdout
x = np.frombuffer(pcm, dtype=np.float32)
s = ort.InferenceSession(model)
state = np.zeros((2, 1, 128), dtype=np.float32)
ctx = np.zeros((1, 64), dtype=np.float32)
probs = []
for i in range(0, len(x), 512):
    c = x[i:i + 512]
    c = np.pad(c, (0, 512 - len(c)))[None, :]
    inp = np.concatenate([ctx, c], axis=1)
    o, state = s.run(None, {"input": inp, "state": state, "sr": np.array(16000, dtype=np.int64)})
    ctx = inp[:, -64:]
    probs.append(float(o[0, 0]))
json.dump(probs, open(out, "w"))
