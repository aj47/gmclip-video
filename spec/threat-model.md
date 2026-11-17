# Threat model: trust boundaries, STRIDE and mitigations (spec v5)

**Status: normative for boundaries** (SPEC v5 §16); mitigations are the requirements named in §1.5, whose text is in `spec/security-requirements.md` §3.5.

**Origin.** The boundaries and threats were found against spec v3 by `critique/round-3/security-redteam.md` (SEC) §1.2–1.4. This file redraws them for v5. "SEC F*n*" refers to that critique's §2 findings and "SEC A*n*" to its §1.1 assets (both informative); "round 1 #12" is `critique/round-1/ai-mcp.md` #12; "M1 F*n*" is finding *n* of `plan/m1-backlog-draft.md` §1. The STRIDE table (§1.3) keeps the attack surface as found; §1.5 says how v5 closes each cell, from which gate, and what risk remains.

### 1.2 Trust-boundary diagram (v5)

```
                         INTERNET / LAN / TAILNET
  ┌───────────────┐  ┌───────────────┐  ┌────────────┐  ┌──────────────┐  ┌────────────┐
  │ malicious web │  │ phone / link  │  │ AI provider│  │ update/model │  │ stream chat│
  │ page (browser)│  │ holder (B5)   │  │ Clippy (B8)│  │ CDN (B8)     │  │ viewers,   │
  └──────┬────────┘  └──────┬────────┘  └─────┬──────┘  └──────┬───────┘  │ downloaded │
         │ DNS rebind /     │ HTTPS only off- │ TLS            │ TLS +    │ VODs, SRTs │
         │ CSRF / LNA       │ loopback; token │                │ signed,  └─────┬──────┘
         │                  │ in URL fragment │                │ expiring       │
  ═══════╪══════════════════╪═════════════════╪════════════════╪═manifest═══════╪═══ host edge
         ▼                  ▼  [M2]           │                │  [M3]          ▼ (B7)
  ┌─────────────────────────────────────────────────────────────────────────────────────┐
  │ gmclip app process (one per user session)                                           │
  │                                                                                     │
  │  ┌──────────── webview (UI JS) [M2] ───────┐  B1: gm_call / gm_subscribe / gm://    │
  │  │ CSP + Trusted Types; untrusted text as  │◄──── ids only (R-IPC-1) ────────┐      │
  │  │ text nodes in a quarantined container   │                                 │      │
  │  └──────┬──────────────────────────────────┘                                 │      │
  │         │ B2: frame socket ws://127.0.0.1:<eph> [M2]: one connection, token  │      │
  │         │ via IPC, exact Origin, viewer-only input records, never a nonce    │      │
  │  ┌──────▼──────────┐   ┌──────────────┐  ┌───────────────┐  ┌──────────────┐ │      │
  │  │ render thread   │   │ project      │  │ MCP tower svc │  │ review HTTP  │ │      │
  │  │ (wgpu, gizmos)  │   │ actors       │◄─┤ (rmcp)        │  │ server (B5)  │ │      │
  │  └──────▲──────────┘   │ (SQLite)     │  └──▲─────────▲──┘  │ own runtime  │ │      │
  │         │              └──────▲───────┘     │         │     └──────────────┘ │      │
  │  ┌──────┴───────────┐  ┌──────┴────────┐    │         │  Owner commands ─────┘      │
  │  │ OS decoders:     │  │ own demux     │    │         │  native Rust sheet, nonce   │
  │  │ macOS VT in proc │  │ (budgets M0), │    │         │  bound to command+args [M2];│
  │  │ (helper SHOULD   │  │ pure-Rust     │    │         │  TTY + code [M1]; OS auth   │
  │  │ M4); Win MF in   │  │ image/SVG [M2]│    │         │  [M3]                       │
  │  │ helper [M5];     │  │ ORT by abs.   │    │         │                             │
  │  │ Linux GStreamer  │  │ path          │    │         │                             │
  │  │ helper [M6]      │  └───────────────┘    │         │                             │
  │  └──────────────────┘                       │         │                             │
  └─────────────────────────────────────────────┼─────────┼─────────────────────────────┘
       B3: Unix socket at a fixed per-user path │         │ B4: Streamable HTTP 127.0.0.1 [M3]
       in a 0700 dir / named pipe (FIRST_PIPE_  │         │ (Host allow, Origin refused, bearer)
       INSTANCE); peer checks both ways;        │         │
       challenge–response, never the secret     │         │
  ══════════════════════════════════════════════╪═════════╪═══════════ same-user edge
  ┌──────────────────────┐   ┌──────────────────┴──┐  ┌───┴──────────────────┐  ┌──────────────────┐
  │ `gmclip mcp` shim,   │   │ launched harness    │  │ user-started agent   │  │ any other same-  │
  │ CLI, headless holder │   │ (B6) via launcher   │  │ session (own perms,  │  │ user process     │
  │ lock file {pid, host,│   │ [M1]: abs. binary,  │  │ other MCP servers);  │  │ (npm postinstall,│
  │ kind}: no socket path│   │ gmclip tools only,  │  │ identity             │  │ VS Code ext.)    │
  └──────────────────────┘   │ no shell/write/web, │  │ local:<client>       │  └──────────────────┘
                             │ env whitelist, TCC  │  └──────────────────────┘
                             │ disclaimed; identity│
                             │ launched:<run>      │
                             └──────────┬──────────┘
                                        ▼ gmclip tools only (no direct file access)
  ┌────────────────────────────────────────────────────────────────────────────────────┐
  │ FILESYSTEM (B9): app data (live DB, lock, audit log), OS cache 0700 (agent shots,  │
  │ temps), project homes in the library, export_dir, Owner-added agent roots, sync    │
  │ folders (packages, profile in iCloud), <profile>/feedback.jsonl (gmclip-owned,     │
  │ human appends only); style file (AJ: his AGENTS.md) READ-ONLY, hash-pinned,        │
  │ changes served only after the human accepts; gmclip never writes it (D16)         │
  └────────────────────────────────────────────────────────────────────────────────────┘
  B10 build/supply chain (crates, npm, CI, signing job, model CDN) → every binary above
  B11 headless Owner (`gmclip owner` TTY [M1]; OS auth or enrolled-key grant files [M3])
  B12 Clippy :8878 HTTP (paired, authenticated hand-off [M4])
```

### 1.3 STRIDE per boundary (attack surface as found; mitigation ids in the last column, detail in §1.5)

S = spoofing, T = tampering, R = repudiation, I = information disclosure, D = denial of service, E = elevation of privilege.

| Boundary | S | T | R | I | D | E | v5 mitigation by cell (ids; detail in §1.5) |
|---|---|---|---|---|---|---|---|
| **B1 webview ↔ core** (`gm_call`, `gm://`, `gm_subscribe`) | XSS script acts as the human (`author: human`) (SEC F4) | XSS edits cuts and history | Human/XSS edits indistinguishable in the op log | `gm://data/<project>` reads any open project; a path-shaped `<project>` reads any SQLite file (SEC F4) | Event flood | **XSS → Owner** through a nonce not bound to its command, or through `--dev-bridge` (SEC F4, F15) | S, T: R-UI-SEC-1/2 · R: residual · I: R-IPC-1 · D: bounded event queues (SPEC §3.10) · E: R-OWN-1b, R-DEV-1, R-UI-SEC-1 |
| **B2 frame socket** | Another local process presents input records as the human (SEC F12) | Injected key records (`d`, ⌫) | Input records carry no origin | Viewer frames include `private` regions | Second client starves the viewer | Input path minting gesture nonces (SEC F12) | S, T, R, E: R-FRAME-1 · I: the viewer is the user's own display · D: R-FRAME-1, R-PROTO-7 |
| **B3 Unix socket / named pipe** | Pipe squatting; a socket path taken from the lock file redirects clients (SEC F8) | MITM of JSON-RPC | Peer identity is a uid only | Shim's secret sent to the wrong server (SEC F8) | Socket path > 104 bytes on macOS; `$XDG_RUNTIME_DIR` unset | Same-user process gets the default scope via the shim (SEC F9) | S, T, I, D: R-ARCH-5a · R: R-ARCH-5a (peer uid/pid/exe logged), R-AGT-5 · E: R-MCP-1c, R-AGT-5 |
| **B4 MCP HTTP** | Token theft from client configs (round 1 #12) | — | Token ≠ process identity | `request_id` replay across callers (SEC F21) | Job flooding | Rug pull of **our** server instructions (`help{style}` serving a changed profile file) (SEC F5) | S: SPEC R-MCP-1 [M3] keychain tokens · R: R-MCP-1c, R-AGT-5 · I: R-PROTO-1/2 · D: SPEC R-COST-2, job semaphores · E: R-PROF-3, R-PROF-5 |
| **B5 review link** | Anyone with the URL is "human, via review link" (SEC F3) | Notes, verdicts and "fix with agent" from any link holder | Link holders not distinguished | Plaintext HTTP on LAN; token in the URL (Referer, history, share sheet); tailnet-wide exposure, or the internet via Funnel (SEC F3) | Multi-range DoS (SEC F3) | Review text → agent brief → tools (SEC F1, F3) | S, R: R-REV-2/3 · T: R-REV-3/4 · I: R-REV-1/2/6 · D: R-REV-6 · E: R-REV-4, R-AGT-3, R-INJ-1 |
| **B6 agent harness** | The harness acts with gmclip's TCC grants (SEC F14) | Writes to the user's repos and dotfiles (if it has a shell) | Harness summary untrusted, but stored | Agent sends source media and transcripts (SEC A1/A2) to its own provider and to other MCP servers | Unattended 8 h batch loops | **Injected transcript → shell** on a harness running with the user's full permissions and aliases (SEC F1; M1 F5–F8) | S: R-AGT-2 · T: R-AGT-1 · R: R-AGT-5, R-INJ-1 · I: R-AGT-1 (gmclip-only MCP), SPEC R-MCP-2 disclosure · D: SPEC §10.7.1 wall clocks, R-PROTO-6 · E: R-AGT-1/3/4/5, R-INJ-1/6/7 |
| **B7 untrusted content** | Filenames and metadata pretending to be instructions | Malicious MP4/MKV/SVG/PNG/SRT/package | — | SVG `href` pulls local files into exports (SEC F11) | Allocation bombs, quadratic resync (SEC F7) | Parser memory corruption in OS decoders **in process** (SEC F7); visual/Unicode injection (SEC F13) | S: R-INJ-1/6 · T: R-MEDIA-SEC-2/3, R-SVG-1, R-STORE-9 · I: R-SVG-1 · D: R-MEDIA-SEC-2 · E: R-MEDIA-SEC-1/4, R-INJ-6/7 |
| **B8 network services** | Manifest host compromise; provider URL repoint (round 1 #2) | Freeze/rollback attacks (SEC F10) | — | Update and model calls leak app usage | — | Updater installer TOCTOU → elevation on Windows (SEC F10) | S: R-UPD-1, SPEC R-OWN-2 · T: R-UPD-1 · I: data-leaving panel (SPEC §16) · E: R-UPD-2/3 |
| **B9 filesystem** | Shared package `owner.json` spoofing | Writes into other agents' instruction files (SEC F2); symlink TOCTOU (SEC F16) | — | Read roots inherited from a malicious project's source paths; UNC paths leak NTLM hashes (SEC F16) | Disk fill via exports | Untrusted SQLite snapshot (SEC F18) | S: R-STORE-9 · T: R-PROF-1/3, R-FS-1 · I: R-FS-1 · D: residual · E: R-STORE-9 |
| **B10 supply chain** | Typosquat crate or npm package | Poisoned dependency or model | — | CI secrets (signing key) | — | Arbitrary code in the signed app (SEC F19) | S, T, E: R-SUP-1/2/3 · I: R-SUP-3 (isolated signing job) |
| **B11 headless Owner** | pty-driven "interactive TTY" (SEC F6) | Forged grant files | Grant log says "user" | — | — | Same-user malware → Owner (SEC F6) | S, T, R, E: R-OWN-3 (audit log) |
| **B12 Clippy local integration** | A fake Clippy on :8878 | Spoofed "published spans" flowing back | — | Export metadata to an unauthenticated local server | — | DNS rebinding on Clippy's local server (SEC F20) | S, T, I, E: R-CLIP-1 |

### 1.4 Assumptions (stated in SPEC §16, R-SEC-DOC-1)

1. **Same-user code execution is inside the trust boundary for confidentiality.** Any process running as the user can read what the user can, and can run `gmclip mcp`. Defences therefore aim at four things: (a) keeping such code from reaching **Owner**, (b) or AI keys in plaintext, (c) keeping it **visible** (connection log), and (d) not letting gmclip *amplify* it. Amplification means turning a stream viewer, a link holder or a web page into same-user code.
2. **Other local users and other tailnet/LAN hosts are outside the boundary.**
3. **All source-derived content is attacker-controlled.** That covers pixels, audio, container metadata, filenames, imported projects, profiles and templates.
4. **The webview is less trusted than the Rust core.** A webview compromise must not reach Owner.

### 1.5 Mitigations and residual risk per boundary (v5)

"From" is the gate at which the mitigation is in the build; before it, the "Before" column says what holds. Requirement text: `spec/security-requirements.md` §3.5.

| Boundary | Mitigations (requirement, gate) | From | Before | Residual risk |
|---|---|---|---|---|
| **B1 webview ↔ core** | Native Owner sheet; nonce minted by the sheet's button, bound to `blake3(command ‖ args ‖ project)`, single-use, 30 s; per-run grants and setup sheets also native (R-OWN-1b, M2). Webview CSP, Trusted Types, HTML-sink ban, no navigation, no plugin permissions (R-UI-SEC-1, lint M1 / enforced M2). Agent and link text quarantined; approval cards show server-computed effects (R-UI-SEC-2, M2). `gm://` by open-project ULID and enumerated kind only (R-IPC-1, M2). `--dev-bridge` compiled out of release and proven absent (R-DEV-1, M2). | M2 | M0–M1 have no webview (headless); Owner is reachable only by the TTY path (R-OWN-3 M1). | An XSS still acts as the human for non-Owner edits (attributed `human`); the op log cannot tell them apart. |
| **B2 frame socket** | One connection, token via `gm_call`, constant-time compare, exact Origin and Host, viewer-only records, never mints nonces, own runtime thread (R-FRAME-1, R-PROTO-7, M2). | M2 | No frame socket before M2. | A same-user process that learns nothing still cannot connect (the token arrives over IPC); one that wins a race to the port before the webview connects only blocks the viewer, which is visible (the webview's own connection is refused and logged). |
| **B3 Unix socket / pipe** | Socket at a fixed per-user path (macOS `$TMPDIR/gmclip-<uid>/s`, asserted to fit `sun_path`, with a pointer file for people only; Linux `$XDG_RUNTIME_DIR/gmclip/s` else `<app data>/run/s`, never `/tmp`, `/proc/self/fd` bind for long paths) in a verified 0700 directory, bound under umask 077; never read from the lock file, which holds `{pid, host, kind}` only (SPEC R-STORE-3); peer verification both ways (uid and executable at M1; macOS team ID from M3; Windows signer at M5); `FIRST_PIPE_INSTANCE`; credentials proved by challenge–response, never sent (R-ARCH-5a, M1; Windows pipe CI from M2, shipped M5). Launched runs carry their own credential and identity `launched:<run>` (R-AGT-5, M1). | M1 | M0 has no socket (in-process CLI). | Same-user code can still connect and use the default scopes (assumption 1); it is visible in the connection log (R-MCP-1c). |
| **B4 MCP HTTP** | Loopback only, Host allow-list, any `Origin` refused, bearer tokens from the keychain via credential helpers (SPEC R-MCP-1, M3). `request_id` keyed per identity and run (R-PROTO-1, M1). Resource reads check binding (R-PROTO-2). Style file served only from the hash-pinned, human-accepted snapshot and marked untrusted (R-PROF-3/5, M1). | M3 (HTTP); M1 (stdio parts) | Only stdio over B3 exists before M3. | Tokens in client configs can be read by same-user code (assumption 1); `read_media` is never on static tokens. |
| **B5 review link** | HTTPS off-loopback only: tailnet (with `whois`, Funnel refused and re-checked every 60 s) or LAN TLS with a pinned self-signed cert (R-REV-1); 256-bit token in the URL fragment, exchanged for a ≤ 12 h cookie; Host, CSRF and Origin checks; 7-day links; revocable (R-REV-2); writes attributed `review-link:<id>[/<login>]`, never `human` (R-REV-3); **"fix with agent" starts a run only on tailnet, from the device owner's login, within the Owner's remote-fix allowance, else queues for an in-app click**; remote runs get no media or private-region grants and their brief is untrusted (R-REV-4); static page with strict CSP and `textContent` (R-REV-5); served by export/moment id only, `O_NOFOLLOW`, hash-checked, single range, own 1-worker runtime (R-REV-6) — all M2. Static share page hardened (R-REV-7, M1). | M2 (server); M1 (static page) | M1's static share page has no server; anyone who can open the folder can watch the files (stated on the page). | A stolen phone with a live cookie can post notes and, on the tailnet as the owner, start budgeted runs until the link is revoked; those runs are isolated launcher runs with default scopes. |
| **B6 agent harness** | One launcher for every harness gmclip starts: client binaries by absolute path (never shell aliases or functions), per-run enumerated allow-list, gmclip-only MCP, built-in tools/shell/web off, user instruction files and plugins excluded (verified per client version with canaries in S7), clean 0700 cwd, env whitelist (R-AGT-1, M1); TCC disclaimed (R-AGT-2, M1); no wildcard allow-entries (R-AGT-4, M1); run credential, identity `launched:<run>`, bound to its cuts, `--media`/`--private-regions` only through the Owner path (R-AGT-5, M1; batches M2); no automatic launches (R-AGT-3, M2); approval caps (R-PROTO-6, M2); injection cases (security-requirements §3.3: reported from M1, gating at M3). | M1 | M0 starts no harness. | A launched agent can still be steered within its allowed gmclip tools on its own cut (bad edits, wasted budget), and sends what it reads to its own provider. A **user-started** agent session keeps the user's full permissions and other MCP servers; the connect dialog says so (SPEC §10.7). Unverified client flags fail S7 and that client version is unsupported. |
| **B7 untrusted content** | Demux budgets and fuzzing (R-MEDIA-SEC-2, FZ1–FZ4, M0); untrusted marking of all source-derived text incl. metadata (R-INJ-1, M1); invisible-character stripping (R-INJ-6, M1); private-class regions masked in `see` (R-INJ-7, M1); pure-Rust image decode with limits (R-MEDIA-SEC-3, M2); SVG without external resources, rasterised at import (R-SVG-1, M2); decode helper (R-MEDIA-SEC-1: Windows M5 MUST, macOS M4 SHOULD; R-MEDIA-SEC-4 Linux M6). | M0 | — | **macOS decodes with VideoToolbox in process until the M4 helper** (hardware decode is already in `VTDecoderXPCService`; CoreMedia format-description parsing is not). Visual prompt injection is reduced (masking, untrusted wrappers) but not eliminated. |
| **B8 network services** | Signed manifests expiring ≤ 14 days, never older than seen, `requireSignedVersion` (R-UPD-1, M3); updater not reachable from the webview (R-UPD-2, M3); Windows per-user install and verify-then-exec with a held handle (R-UPD-3, M5); two-key rotation (R-UPD-4, M4). | M3 | No updater calls before M3 (AJ's builds only). | A network attacker can delay updates up to 21 days before the warning shows. |
| **B9 filesystem** | gmclip writes only its own files, never another agent's instruction file; feedback in `<profile>/feedback.jsonl`, human-only (R-PROF-1/2, M1); style file read-only, hash-pinned, accepted by the human (R-PROF-3/5, M1); authority never read from a profile (R-PROF-4, M1); cap-std roots, source dirs not roots, refused path forms, no byte echo (R-FS-1, M1; FZ10 M2); untrusted project import (R-STORE-9, M2); 0700 caches (R-PROTO-4, M1). | M1 | M0 reads plan and source files named on the command line by AJ. | Same-user code can edit the style file; the change is not served until accepted, but a human can accept a bad diff. |
| **B10 supply chain** | Locked builds, no install scripts, cargo-vet with human audits, release age ≥ 30 days for new crates (R-SUP-1, W0); agents cannot touch supply-chain, CI, capability, entitlement or signing files (R-SUP-2, W0); ephemeral release runners, SLSA provenance, isolated signing, SBOM diff (R-SUP-3, M3). | W0 | — | A malicious update to an already-audited crate within its audit scope; a compromised maintainer account (AJ is the single reviewer). |
| **B11 headless Owner** | TTY + re-typed code (R-OWN-3, M1: scriptable by same-user code with a pty, accepted while only AJ runs builds); OS authentication per invocation or enrolled-device-key grant files, audit log (R-OWN-3, M3). | M1 (weak), M3 | — | Between M1 and M3 same-user malware can reach Owner through a pty. Builds go to no one but AJ before M3. |
| **B12 Clippy local integration** | Paired, authenticated hand-off; Host check; agent hand-off needs approval; published list may only add spans, > 20 % change shown first (R-CLIP-1, M4). | M4 | No hand-off before M4. | Clippy's own local server stays unauthenticated on loopback/tailnet; that is Clippy's boundary, not gmclip's. |
