# gmclip landing page

A landing page with a Cloudflare Pages Function and D1-backed early-access signup.

- The landing page uses `assets/shots/exe-dev-v1/`: real captures through the e2e dev bridge with [AJ’s exe.dev Short](https://www.youtube.com/shorts/p9TabsKWsqI) loaded, paused at 00:35. Captured at 1280 × 800 with a 1.5625 device scale for 2000 × 1250 output, then encoded as WebP. Both themes include the current icon, matching transcript, waveform, and project title. The isolated demo project has eight continuous parts and corrected caption spellings of exe.dev. Its 16:9 framing contains the full portrait video with sidebars. `social/screenshots.json` records capture provenance.
- Copy claims only what is built (see STATUS.md); nothing about pricing, Linux, signed builds or the local highlight finder.
- All early-access buttons open a native modal dialog with the email form. Escape, the close button, and the backdrop dismiss it; focus returns to the opener. Signups are normalized, deduplicated, and stored with a timestamp and consent version in the private `gmclip-signups` D1 database. No confirmation email is sent.
- Social previews use `assets/og-gmclip-v2.jpg` (1200 × 630), with absolute Open Graph and Twitter image URLs. The layout source is `social/og.html`; regenerate with `node render-og.mjs` using the existing Playwright dependency in `app/ui`. The card uses exact brand assets/fonts and the app screenshot as a layout, with its video preview composited from a still at 00:35 of [AJ’s exe.dev Short](https://www.youtube.com/shorts/p9TabsKWsqI). `assets/og-exe-dev-still.jpg` is the extracted 1080 × 1920 source frame; the full video is not deployed. Bump the image filename and metadata together for future revisions.
- The endpoint accepts same-origin JSON POSTs, validates emails, caps payloads, uses a honeypot, and limits each hashed IP to ten valid attempts per hour. Raw IPs are not stored. Rate-limit records are cleaned up after 24 hours as requests arrive.

From `site/`:

```sh
python3 build.py
wrangler d1 migrations apply gmclip-signups --local
wrangler pages dev
```

Deploy after validating locally:

```sh
wrangler d1 migrations apply gmclip-signups --remote
wrangler pages deploy --branch main
```

`build.py` stages only public assets in ignored `dist/`; source, migrations, and this README are not uploaded. Preview deployments have no production DB binding.

Read signups through the Cloudflare D1 dashboard, or export locally (contains personal data; do not commit):

```sh
wrangler d1 execute gmclip-signups --remote --command 'SELECT email, created_at, consent_version FROM signups ORDER BY created_at DESC' --json > /tmp/gmclip-signups.json
```
