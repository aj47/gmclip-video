# 0010 — The frame socket re-arms when the window shows again

Date: 2026-10-15 · Decided by: coordinator (amending R-FRAME-1 as U.01b landed) · Status: adopted

## Context
R-FRAME-1 closes the frame socket when the window has been hidden or minimised for more than 60 s, and it allowed exactly one connection per launch. Together those meant that after AJ minimised the app for a minute the preview stayed dead until he relaunched it. That is unacceptable UX for an editor he keeps open all day.

## Decision
- The close after 60 s hidden stays as it was: the session closes the socket, logs it, and nothing renders, because the viewer renders only for a connected client.
- When the window shows again after such a close, the core **re-arms** the session (`frame::FrameSession::rearm`):
  - It mints a fresh 256-bit token and bumps a generation counter.
  - It allows exactly one new connection. The old token is replaced and can never be used again.
- The UI notices the close and polls `gm_call app_frame_socket` once a second. When the generation is newer, it connects with the new token.
- On connect, the core redraws the current frame for the new client, as it does for any first client. No relaunch is needed.
- Only a hide-close re-arms. Any other close still needs a relaunch: a dropped connection, a page reload, or a protocol violation.

## Security properties kept
- **One live connection.** A second connection is refused while one is live. The accept path now takes the slot atomically, so two clients racing the same token cannot both get it.
- **A token is never reusable.** Each re-arm replaces it. Re-arming happens only after the previous session has closed.
- **Unchanged:** token delivery over `gm_call` only, the constant-time compare, exact Origin and Host, and viewer-only records.

## Evidence
- `app/src-tauri/src/frame.rs`, test `hidden_window_closes_the_socket` (0.3 s limit). It covers each property in turn:
  - a short hide keeps the socket;
  - a long hide closes it;
  - the old token is refused before and after the re-arm;
  - the new token gets frames;
  - a third connection is refused.
- `app/ui/tests/e2e/reconnect.spec.ts`: drives the hide → close → show cycle through the dev-bridge-only `dev_frame_cycle`. The viewer shows "paused" and then comes back live by itself, with frames flowing. `dev_frame_cycle` is a marker in `cargo xt assert-no-dev-bridge`.

## Sections changed
- `spec/security-requirements.md` R-FRAME-1: "one connection per launch" becomes "one live connection; a fresh token after a hide-close (decision 0010)".
