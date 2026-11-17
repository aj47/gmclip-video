# gmclip brand: Sticker Playful

Locked in 2026-10-17 by AJ, chosen from five directions. `brand-kit.png` is the board and `ui-mockup.png` the matching editor window (AI-generated mockups; the hex codes and font names below come from the board).

- Mark: a smiling sun drawn as one cream hairline on a sky-blue rounded square (locked 2026-10-17: v2, the line drawing "F" at hairline weight). Every proportion is a power of φ: tile corner = tile ÷ φ⁴, sun radius r = tile ÷ 2φ², twelve rays from r·√φ to r·φ, line weight r ÷ φ⁵. Tagline: "Bright ideas. Great clips."
- Palette: sky blue `#4A9BE8`, sun yellow `#FFD23F`, coral `#FF6B5B`, teal `#2EC4B6`, violet `#7B61FF`, ink `#2D2A4A`. In the app these are `--brand-*` in `app/ui/src/design/tokens.css`.
- Type: Fredoka (headings, `--font-display`) and Nunito Sans (UI, `--font-ui`), the OFL variable fonts in `assets/fonts`, loaded by `app/ui/src/design/brand.css`.
- Style: thick ink outlines, chunky rounded shapes, sticker-like flat color.

## In the app

- Two themes, both from `tokens.css`: cream (`data-theme="light"`) and night ink (the base set). New installs follow the system (`theme: "system"` in `app/src-tauri/src/uiprefs.rs`); Settings › Appearance still picks Dark or Light.
- The sticker: `--outline` (ink on cream, near-black on night ink), `--outline-w` (2 px) and `--lift` / `--lift-lg` (a solid offset under the thing). Plain, secondary, primary and danger buttons, Home's cards, menus, popovers, dialogs, toasts and the video frame wear it; ghost, tab, segment and menu-row buttons stay flat so toolbars stay quiet.
- The accent is the sky blue (selection, playhead, primary action), with ink text on it (white would be 2.8:1). Play is a sun-yellow sticker; switches turn teal; the timeline's lanes are tinted blue (video), coral (hook), violet (text) and teal (captions), each clip outlined in ink.
- The top bar shows the icon (`app/ui/src/design/sun-64.png`).

## Files (transparent PNGs, real alpha)

- `icon.svg`: the app icon (the source of truth); every PNG below and in `app/src-tauri/icons/` is rendered from it.
- `icon-1024.png`: the app icon at 1024 px. `app/src-tauri/icons/` holds `icon.png` (512), `32x32.png`, `128x128.png`, `128x128@2x.png` and a multi-size `icon.ico`, all cut from it. `app/scripts/bundle-mac.sh` builds the `.icns` from `icon.png`.
- `logo-lockup.png` (ink wordmark, for light backgrounds) and `logo-lockup-light.png` (cream wordmark, for dark ones), the wordmark set in Fredoka Bold.
- `wordmark.png` and `wordmark-light.png`: the "gmclip" wordmark alone.
- `icon-reference.png`: the original screenshot the icon was redrawn from.
