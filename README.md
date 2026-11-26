# gmclip landing page

A landing page with a Cloudflare Pages Function and D1-backed early-access signup.

- The landing page uses `assets/shots/v0.1.0/`: real captures of the 0.1.0 app through the e2e dev bridge, made by `app/ui/tests/e2e/siteshots.spec.ts` with [AJ’s exe.dev Short](https://www.youtube.com/shorts/p9TabsKWsqI) loaded (`GMCLIP_SITE_SHOTS=<folder> GMCLIP_SITE_VIDEO=<the Short> GMCLIP_SITE_NAME="exe.dev — VMs with a prompt" node scripts/e2e.mjs siteshots` in `app/ui`, then `cwebp -q 80`). 1280 × 800 at a 1.5625 device scale gives 2000 × 1250. The demo project has eight parts, "exe.dev" re-spelled where the speech model heard "XC.dev", a text hook, three topics, a long video and a short from them. gmclip's own captions stay off because the Short has its captions burned in. `social/screenshots.json` records provenance. `docs/screenshots/` holds 1600-wide copies for the README.
- Copy claims only what is built (see STATUS.md); nothing about pricing, Linux, signed builds or the local highlight finder. Download is the main call to action (the v0.1.0 release assets); the only signup is "Get updates" lower on the page.
- All early-access buttons open a native modal dialog with the email form. Escape, the close button, and the backdrop dismiss it; focus returns to the opener. Signups are normalized, deduplicated, and stored with a timestamp and consent version in the private `gmclip-signups` D1 database. No confirmation email is sent.
- Social previews use `assets/og-gmclip-v4.jpg` (1200 × 630), with absolute Open Graph and Twitter image URLs. The layout source is `social/og.html`; regenerate with `node render-og.mjs` using the existing Playwright dependency in `app/ui`. The card uses exact brand assets/fonts and the 0.1.0 `editor-vertical-light` screenshot, which already shows the Short at 00:35. (`assets/og-exe-dev-still.jpg` was the still v2 composited in; v3 does not use it.) Bump the image filename and metadata together for future revisions.
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
