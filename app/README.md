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

## Installers

- macOS: `app/scripts/bundle-mac.sh [OUT_DIR]` builds an ad-hoc signed `gmclip.app`.
- Windows: `app\scripts\bundle-win.ps1 [-Out DIR] [-Wix DIR]` builds
  `gmclip-<version>-x64.msi` with WiX 3.14. Its Start menu shortcut runs
  `gmclip-launch.exe`, which starts `gmclip-app.exe` (a console program: the MCP shim and
  `call`) without a console window. If a build stops with "The paging file is too small"
  (os error 1455), add `-j 8` to cargo.
- `.github/workflows/release.yml` runs both on a `v*` tag: `.dmg` for Apple silicon
  (macos-15) and Intel (macos-15-intel), `.msi` for x64, each with a `.sha256`, attached
  to a draft release. A pull request touching the packaging builds them as artifacts.
- Linux has no installer yet: the media pipeline the app needs (export, preview, proxy)
  is macOS and Windows only until M6's GStreamer helper. CI builds and tests the core
  crates on Linux.
