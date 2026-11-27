#!/bin/sh
# What worked: visits, download clicks and update sign-ups by source over the last DAYS days
# (default 28), from the events table /api/e fills, plus GitHub's own download counts.
# Run from site/:
#   scripts/report.sh            # live data
#   scripts/report.sh 7 --local  # a week, from `wrangler pages dev`'s local D1
# See ../ANALYTICS.md.
set -e
DAYS=${1:-28}
WHERE=${2:---remote}
SINCE="date('now', '-$DAYS days')"
q() {
  printf '\n## %s\n' "$1"
  npx wrangler d1 execute gmclip-signups "$WHERE" --json --command "$2" 2>/dev/null | node -e '
    const rows = JSON.parse(require("fs").readFileSync(0, "utf8"))[0].results
    if (!rows.length) { console.log("(nothing yet)"); process.exit() }
    const cols = Object.keys(rows[0]), w = cols.map((c) => Math.max(c.length, ...rows.map((r) => String(r[c] ?? "").length)))
    const line = (cells) => cells.map((v, i) => String(v ?? "").padEnd(w[i])).join("  ")
    console.log(line(cols)); console.log(line(w.map((n) => "-".repeat(n)))); rows.forEach((r) => console.log(line(cols.map((c) => r[c]))))'
}

q "By source: visits, download clicks, sign-ups" "
SELECT coalesce(source,'?') source, coalesce(medium,'?') medium, coalesce(campaign,'') campaign,
  sum(name='visit') visits, sum(name='cta' AND detail LIKE 'download:%') downloads,
  sum(name='cta' AND detail IN ('github','release')) github, sum(name='signup') signups
FROM events WHERE day >= $SINCE GROUP BY 1,2,3 ORDER BY visits DESC LIMIT 40"

q "Week by week" "
SELECT strftime('%Y-W%W', day) week, sum(name='visit') visits,
  sum(name='cta' AND detail LIKE 'download:%') downloads, sum(name='signup') signups
FROM events WHERE day >= $SINCE GROUP BY 1 ORDER BY 1"

q "Landing pages (where visits start)" "
SELECT path landing, sum(name='visit') visits FROM events
WHERE day >= $SINCE AND name='visit' GROUP BY 1 ORDER BY 2 DESC LIMIT 20"

q "Clicks by link and page" "
SELECT detail link, path page, count(*) clicks FROM events WHERE name='cta' AND day >= $SINCE
GROUP BY 1,2 ORDER BY 3 DESC LIMIT 20"

q "Referring sites" "
SELECT ref, count(*) visits FROM events
WHERE name='visit' AND ref IS NOT NULL AND day >= $SINCE GROUP BY 1 ORDER BY 2 DESC LIMIT 20"

printf '\n## GitHub release downloads (all time)\n'
gh api repos/aj47/gmclip-video/releases --jq '.[] | .tag_name as $t | .assets[] | "\($t)  \(.download_count)  \(.name)"' 2>/dev/null || echo "(gh not signed in)"
