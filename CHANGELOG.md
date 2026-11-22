# Changelog

## 0.1.0 — 2026-11-22

The first release with installers. gmclip turns long recordings into vertical and
horizontal cuts that you edit by editing their words.

### New

- The yellow smiling sun is back as the app icon.
- A simpler cut screen. The top bar is labelled (Agents, Home, Search, Settings). Words and
  Captions are the side panel's tabs, and Look is in ⌘K. One sidebar tree holds your
  videos, topics, long videos and shorts.
- The Words panel reads as prose, with topics as headings.
- Captions are a list of switches: captions, highlighted words, text hook, section label
  and stopwatch. Type a hook and press Enter.
- Home shows each project as one card with its cuts. Settings has sections, Agents lets
  you pick your agent, the keys sheet shows the essentials first, and Search is always in
  the top bar.
- New shorts are named from their hook, else their topic, else their first words.
- The Volume lane (formerly Gain) shows when a cut has levels, and the timeline toolbar
  has a toggle for it.
- 4K YouTube export.
- Installers: a `.dmg` for Macs with Apple chips, a `.dmg` for Intel Macs and an `.msi`
  for Windows x64, each with a `.sha256`.

### Fixed

- Windows: the Start menu shortcut opens gmclip without a console window, and the speech
  model's ONNX Runtime downloads from NuGet when its GitHub release is unavailable
  (from [mattfarmerai](https://github.com/mattfarmerai)).
- Windows: gmclip no longer refuses its own private folders when you sign in as the
  built-in Administrator.
- Linux: agents that gmclip launches can reach it.
- A timeline label no longer trips the app's content security policy, the timeline
  toolbar wraps in narrow windows, and a hidden timeline no longer throws while drawing.
- Security updates: SQLite 3.54 (a WAL file CVE) and rmcp 3.7.

### Known limits

- The installers are unsigned. On macOS, open System Settings → Privacy & Security and
  click Open Anyway the first time. On Windows, click More info, then Run anyway, when
  SmartScreen warns.
- There is no Linux installer yet.
- The Intel Mac build is less tested than the Apple silicon one.
