# Contributing

Bug reports and pull requests are welcome. For anything larger than a fix, open an issue
first: the spec is frozen and changes to it go through a note in `decisions/`.

## Before you open a pull request

Run what CI runs (`.github/workflows/ci.yml`):

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked
cargo test --doc --workspace --locked
cargo xt deps
cargo xt check-registry
cargo deny --locked check
cargo vet --locked
```

For the app, from `app/ui`: `pnpm lint && pnpm lint:fixtures && pnpm typecheck`, and
from `app/src-tauri`: `cargo test --features custom-protocol,dev-bridge`.

## Rules that are checked

- **No codec code.** gmclip does not decode or encode coded picture or audio data itself;
  that goes through the OS. Nothing shipped may link ffmpeg, x264, x265, or GPL or AGPL
  code. Changes under a `codec/` directory need the maintainer's review (`CODEOWNERS`).
- **Supply chain.** A new crate needs a `cargo vet` audit and must pass `deny.toml`. A new
  build-script or proc-macro crate fails `cargo xt supply`.
- **Crate dependencies point one way.** `cargo xt deps` enforces the direction and the
  rule that only `gmclip-gpu` uses `unsafe`.
- **Logs carry no content.** No paths under the home folder and no caption or transcript
  text in logs or error messages.

## Licence

Contributions are dual licensed under MIT and Apache-2.0, as the README says.
