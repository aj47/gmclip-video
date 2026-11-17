# 0005 — Hardware encode is not bit-reproducible; compare decoded frames by SSIM

- **Context.** Lane H (H-10): VideoToolbox's hardware H.264 encoder produces different bitstreams run to run from identical input frames and PCM. Criteria that demand identical decoded frames across exports cannot hold.
- **Decision.** Reproducibility is asserted **before encode** (`frames_hash` of composited frames and the PCM mix, SPEC §5.14) and after encode by decoded-frame **SSIM ≥ 0.995 per frame** against the previous export of the same `render_sig`. Byte-identical comparisons remain for the RenderPlan, text tiles and PCM. A software-encoder path is not added for this.
- **Sections changed.** SPEC §5.14 export record (frames_hash is the reproducibility check — unchanged), R-COMP-1 wording ("bit-identical" applies to composited frames, not encoded files).
