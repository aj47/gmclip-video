# 0057 — The human's home folder is an agent read root by default

Date: 2026-11-26 · Decided by: AJ ("make the default everything available and ship it") · Status: adopted

## Context
An outside agent (Claude Code in another bb thread, calling `gmclip-app call` with `GMCLIP_SESSION`) was refused `file_roots` opening `~/Movies/tasklet-application/raw/take-a.mp4`: only full-access sessions gmclip launches get the home folder (decision 0039), and a fresh install has no Owner roots. AJ then tried Settings → Agent access… → Folders agents may read: `~/…` was not accepted, `/Movies/tasklet-application` (no home) was saved though no such folder exists, and a folder added with "and write" went only to `write_roots`, so agents still could not read it and the list showed nothing. His file ended as `{"read_roots": ["/Volumes/Untitled"], "write_roots": ["/Movies/tasklet-application", "/Users/ajjoobandi"]}`.

## Decision
- **R-FS-1: the human's home folder is a read root for every agent** (outside agents over `gmclip-app mcp` / `call`, sessions gmclip launches, runs bound to a cut), unless the Owner removed it. `OwnerSettings.home_read` (absent = yes). The app sets the home (`HOME` / `USERPROFILE`); a bare holder derives it from app data.
- **The Owner narrows it.** Settings lists the home folder first among the folders agents may read; Remove (`roots_remove {path: <home>}`) sets `home_read: false` and agents are back to the listed roots; adding the home back (`roots_add`) restores the default. Both go through the native sheet as before.
- **A write root is a read root.** `OwnerSettings::read_roots` includes `write_roots`, so AJ's existing file reads `/Users/ajjoobandi` with no change. Settings lists write roots, tagged "read and write".
- **A root must be a folder.** `roots_add` refuses a path that is not an existing directory, in the app before the sheet asks and in the holder for the CLI's Owner path.
- **`~` in the folder field.** `~` and `~/…` expand to the home folder in the UI; the field shows the absolute path it will add.
- **Unchanged:** the refused path forms (UNC, ADS, device names, `..`, NUL, trailing dot or space), symlinks out of a root, the cap-std handles, the 16-byte echo limit, and the write roots (`export_dir`, the agent-shots cache, Owner-added write roots). Same-user processes are already trusted for confidentiality (threat model §1.4); gmclip reading the home folder gives an agent nothing its own shell does not have.
- R-FS-1's acceptance case (an imported project listing `~/.ssh/known_hosts` grants no read of `~/.ssh`) still holds for the import: a source's directory never becomes a root. With the home folder on, `~/.ssh` is readable because the home is; removing the home folder in Settings closes it.
- The session's first message says the home folder is readable when it is, for gmclip-access sessions too.

## Evidence
`crates/gmclip-mcp/tests/session_grant.rs::the_home_folder_is_readable_by_default_and_the_owner_can_narrow_it` (an outside agent adds `~/Movies/tasklet-application/raw/take-a.mp4` on a fresh config; a symlink out of the home is refused; remove → refused, help no longer lists the home; add back → readable; a missing folder is refused; a write root reads). `app/src-tauri/tests/owner.rs` (`home_readable` reported; a missing folder refused before the sheet), `app/src-tauri/tests/sessions.rs` (a gmclip-access session is told the home folder is readable).

## Sections changed
- `spec/SPEC.md` R-FS-1: the human's home folder is a default read root; write roots are read roots; roots must exist.
- `spec/security-requirements.md` R-FS-1: the same.
