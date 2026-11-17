## What and why

<!-- One paragraph: the ticket id(s), what changes, and the evidence (numbers). -->

## Checklist (SPEC D3, §15; tick or say why not)

- [ ] **No coded picture or audio data is decoded or encoded by gmclip code** (SPEC D3). Decode and encode go through the OS (VideoToolbox / AudioToolbox, Media Foundation, MediaCodec, the user's GStreamer); only Opus, MP3, FLAC and Vorbis decode in process (symphonia, **AAC feature off**).
- [ ] **No ffmpeg, x264/x265, GPL or AGPL** in anything shipped (crates, the app, bundled tools). Only dev-only `tools/` may call ffmpeg/MP4Box to make references (SPEC §3.1); nothing links it.
- [ ] **`codec/` reads only the fields in `codec/ALLOWED_FIELDS.md`** (SPEC §15.0), each citing its clause; it never writes coded data except splitting VP9 superframes and synthesising `vpcC`/`av1C` from those fields; no slice-level parsing; OS-parsed values preferred where the OS provides them. Any change under a `codec/` directory or to a `codec.rs` needs AJ (CODEOWNERS).
- [ ] **Supply chain** (SR R-SUP-1/2): no new crate without a `cargo vet` audit or an exemption listed under "Needs AJ"; no new build-script or proc-macro crate (`cargo xt supply`); `Cargo.lock` committed.
- [ ] **Checks, as CI runs them** (`.github/workflows/ci.yml`): `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo nextest run --workspace --locked`, `cargo test --doc --workspace --locked`, `cargo xt deps`, `cargo xt check-registry`, `cargo deny --locked check`, `cargo vet --locked`, `cargo xt fixtures --require-all`, `cargo xt codeowners --check`; `lab` tests on the lab Mac when the change touches them.
- [ ] **Logs and errors**: content fields are shape-only, with no paths under `$HOME` and no caption or transcript text in logs (SR R-PROTO-3, `spec/security-requirements.md`); errors echo ≤ 16 bytes of caller input (SPEC R-CMD-6).
- [ ] **STATUS.md**: one line under "Build"; anything AJ must do under "Needs AJ"; `cargo xt status --check` passes.
- [ ] **Review**: a second agent reviewed the diff; for an interface ticket **(I)** the reviewer's summary ends in go/wait for AJ.
