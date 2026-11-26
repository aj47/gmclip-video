# Analytics

How we know what brings people to gmclip.com and which of them download. The rule, as on
tenniscamera.app: count **which link or site brought people**, nothing about the people. No
IP addresses, visitor ids, cookies or countries are stored. `pages/privacy.html` says so.

| Question | Where to look |
|---|---|
| Which searches show us, and where we rank | Google Search Console for gmclip.com (not set up yet) |
| Which posts, sites and pages bring visits, download clicks and update sign-ups | `scripts/report.sh` |
| How many times each installer was downloaded | GitHub release asset counts (the last section of `scripts/report.sh`) |
| Total traffic, including people with JavaScript off | Cloudflare dashboard > Workers & Pages > gmclip > Metrics |

## Our counts

`a.js` on every page sends one event to `POST /api/e` (`functions/api/e.js`), which writes a
row to the `events` table in the `gmclip-signups` D1 database (`migrations/0002_events.sql`).

| name | when | detail |
|---|---|---|
| `visit` | the first page of a visit (a new `utm_` tag or outside referrer also starts one) | |
| `cta` | a click on a download, release, GitHub or "Get updates" link | `download:mac-arm`, `download:mac-intel`, `download:windows`, `release`, `github` or `updates`, plus `:<data-cta>` when the link has one (`nav`, `article`) |
| `signup` | a successful "Get updates" sign-up | |

Each row has the day, the page path, the outside referrer's host, and the source (`source`,
`medium`, `campaign`, `content`, `term`). The source comes from `utm_*` tags, else the
referrer (search engines become `medium=organic`), else `(direct)`, and is kept in
sessionStorage while the visitor moves between pages. The collector drops other origins,
other event names and bot user agents (including headless Chrome, so local screenshot runs
don't count), and caps each hashed IP at 120 events an hour using `signup_limits`.

Tag links you post: `https://gmclip.com/?utm_source=youtube&utm_medium=video&utm_campaign=<video>`.

After adding a migration: `wrangler d1 migrations apply gmclip-signups --remote`.
