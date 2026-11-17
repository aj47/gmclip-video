# 0019 — Windows local IPC: an owner-only named pipe, and Windows FFI in gmclip-platform

Date: 2026-10-16 · Decided by: agent (lane-w; brief: "named pipes with a DACL restricted to the current user SID, peer identity via `GetNamedPipeClientProcessId`, if it can be done well") · Status: **proposed, needs AJ** (security-relevant)

## Context
gmclip had never compiled for Windows. The static review from the 5080 PC (`milestones/m5/windows-static-review-2026-10-16.md`) found `gmclip-mcp` Unix-only with no gating: `UnixListener`, umask, uid, `getppid`, `/bin/ps`, the tty. That took `gmclip-cli` and the app down with it. The launcher had no Windows client discovery and no Windows run-dir ACL. R-ARCH-5 already names the Windows transport ("a Windows pipe created with `FIRST_PIPE_INSTANCE` and an owner-only DACL"), but the FFI it needs cannot live where SPEC §3.1 allows `unsafe`: gmclip-gpu would pull wgpu into mcp and the launcher, against the layering.

## Decision
1. **Transport** (R-ARCH-5, as written). The pipe is `\\.\pipe\gmclip-<user SID>`, a pure function of the user like the Unix socket path. Its first instance is created with `FILE_FLAG_FIRST_PIPE_INSTANCE`, the descriptor `O:<sid>D:P(A;;FA;;;<sid>)`, `PIPE_REJECT_REMOTE_CLIENTS` and byte mode, and a new instance is made as each one connects. The pipe's DACL takes the place of the 0700 directory and umask 077. The handshake (nonces, HMAC proofs, never the credential) is the same code on every OS (`gmclip-mcp/src/transport.rs`).
2. **Peer identity, both ways.** The server takes the client's pid from `GetNamedPipeClientProcessId`, opens it with `PROCESS_QUERY_LIMITED_INFORMATION`, and requires its token user SID to equal ours; any other user is audited ("wrong uid") and dropped. The client checks the server the same way with `GetNamedPipeServerProcessId`, and also requires the server image (`QueryFullProcessImageNameW`) to be the expected gmclip executable, as `lsof` does on macOS. The client keeps tokio's `SECURITY_IDENTIFICATION` QoS, so a server can identify the client but not impersonate it.
3. **Squatting.** If another account creates the name first, the holder's first-instance create fails and it refuses to start, saying why. That is denial of service only, as a hostile `/tmp` would be on Linux. A client that reaches a squatter fails the SID and image check before it sends anything derived from a credential.
4. **Second launch and handover.** `bind` first tries a verified control `Forward`. A pipe goes away with its process, so there is nothing stale to clean up. The app waits for a handed-over holder to go by *listing* `\\.\pipe\`: opening the name to stat it would take an instance from the holder.
5. **Owner path.** Windows has no tty device to write the Owner code to, so Owner challenges fail closed ("needs an interactive terminal"). The native Owner sheet is still AppKit-only, so on Windows it answers "cancelled". A console or app-UI channel is M5 work.
6. **Private files and directories.** They are created with the owner-only descriptor (`CreateDirectoryW` / `CreateFileW` with `SECURITY_ATTRIBUTES`), so there is no window between create and chmod. An existing directory must be owned by our SID, and its DACL, read as SDDL, must not allow any other trustee: `SY`, `BA`, `WD`, `OW` and other SIDs all fail. App data is `%LOCALAPPDATA%\gmclip`. Run directories are `%SystemDrive%\gmclip-<user SID>\agent-runs` (amended 2026-10-16 after the first real Windows run: `%TEMP%` sits inside the profile, which holds `.claude\` and `.codex\`, so the ancestor check refused every run; at the drive root the only ancestor is the root, and a folder of that name made by another account fails the owner check). The ancestor instruction-file check runs to the drive or share root of the `\\?\` path and matches names without case.
7. **Launcher.** It looks for `<name>.exe` and `<name>.cmd` in `PATHEXT` order, then in `%USERPROFILE%\.local\bin`, `%APPDATA%\npm`, bun, Volta, opencode, nvm-windows and `%ProgramFiles%\nodejs`.
   - A `.cmd` is never run, because of BatBadBut (CVE-2024-24576). An npm shim is parsed for its `"%dp0%\…"` target, which must stay inside the shim's folder. The Unix rules then apply: the Codex wrapper resolves to its native binary, pi runs under the `node.exe` beside the shim (else `%ProgramFiles%\nodejs`), and Claude's `cli.js` is refused, as on Unix.
   - The environment whitelist adds the Windows profile and system variables, matched without case. `ComSpec` is not added.
   - The wall clock stops the client's tree with `taskkill /T`, then `/F`.
8. **`unsafe` (amends SPEC §3.1).** gmclip-platform may contain `unsafe` **on Windows only**.
   - Its root says `#![cfg_attr(not(windows), forbid(unsafe_code))]`.
   - Its own lint table sets `unsafe_code = "deny"`, with `#[allow]` on the `windows` module alone, and `undocumented_unsafe_blocks = "deny"`.
   - `cargo xt deps` enforces all of this. gmclip-mcp and gmclip-ai may now depend on gmclip-platform (a Windows-only target dependency).
   - `windows-sys 0.61.2` was already locked through tokio and mio. It becomes a direct dependency of gmclip-platform. Neither lockfile gains a crate.

**Known limits.**
- The built-in Administrator account (RID 500) prints as `LA` in SDDL, so the private-directory check fails closed for that account.
- On Windows, a parent pid is only a hint (pids are reused). It labels the connection log and is never used as authority.
- The SID check opens the peer's process and token. A non-elevated holder may not be able to read an *elevated* client's token, so that client is dropped (fails closed). Reading the token by `ImpersonateNamedPipeClient` would avoid this, but it needs a read first, which would change the handshake order. That is noted for the PC run.
- MCP children a client leaves running after it exits on its own are not reached. That needs a job object (M5).
- All of this is compile-checked from the Mac only. The `cfg(windows)` tests first run on the PC.

**If AJ says no:** keep the pipe out and fall back to the interim. That means `#[cfg(unix)]` on `server` / `client` / `holder` / `paths`, with a Windows stub returning "the local MCP host is not on Windows yet (M5)". The launcher and app parts do not depend on the pipe.

## Evidence
- `tools/check_windows.sh` runs `cargo check` and `clippy -D warnings`, all targets, for the workspace and the app on `x86_64-pc-windows-msvc`. It is clean on the Mac.
- macOS fmt, clippy and tests are unchanged.
- Windows tests:
  - `gmclip-platform` `windows::tests` (SID, ACLs, pipe peers)
  - `gmclip-mcp` `paths::tests::windows_private_dirs_are_created_owner_only`
  - `gmclip-ai` `rundir::windows_tests`, `resolve::windows_tests`, `env::tests::windows_children_get_the_profile_and_system_root`
- Pure logic tested on every OS: `gmclip-platform` `sddl`, `winpath`; `gmclip-ai` `launcher::winshim`.

## Sections changed (on adoption)
- `spec/SPEC.md` §3.1: "`gmclip-gpu` (only crate with `unsafe` wgpu-hal)" → "… and `gmclip-platform` on Windows only (Win32 ACL, pipe and process FFI; decision 0019)"
- `xtask/src/deps.rs`: `UNSAFE_ON_WINDOWS`, plus the mcp/ai → platform edges (already in this branch)
