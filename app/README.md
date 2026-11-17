# gmclip for Mac (`app/`)

- `src-tauri/` — the Tauri 2.12.3 shell (crate `gmclip-app`). Its own Cargo workspace for
  now, so other lanes' `cargo build --workspace` does not compile Tauri on the shared disk;
  it depends on `crates/*` by path. Joining the root workspace is a `members` line plus
  `cargo xt deps` (which already has a `gmclip-app` row).
- `ui/` — SolidJS + TypeScript + Vite, pnpm (`.npmrc`: ignore-scripts, frozen lockfile).

```bash
cd app/ui && pnpm install --frozen-lockfile && pnpm exec vite build
cd ../src-tauri && cargo build --features custom-protocol            # the app (serves ui/dist)
cargo test --features custom-protocol,dev-bridge                      # unit + slice tests
cargo test --features custom-protocol --test slice -- --ignored       # lab: preview + export
cargo build --features custom-protocol,dev-bridge && cd ../ui && pnpm exec playwright test
pnpm lint && pnpm lint:fixtures                                       # R-UI-SEC-1 sink ban
```

`target/debug/gmclip-app [--open <project home>] [--s2a]`; `--dev-bridge [--no-window]
[--dev-bridge-origin <origin>]` exists only with the `dev-bridge` feature (refused in
release builds). S2a: `../spikes/S2a/README.md`.
