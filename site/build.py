"""Stage only public assets for Cloudflare Pages, and build the article pages.

Each pages/<slug>.html starts with a comment of `key: value` lines (title, description, h1,
lede, updated, and optionally nav: a short label for the footer) followed by the article body.
build.py wraps it in the site's head, nav, download box and footer, writes dist/<slug>.html
(served at /<slug>), and writes sitemap.xml and robots.txt.
"""
from html import escape
from pathlib import Path
import re
import shutil

root = Path(__file__).resolve().parent
out = root / "dist"
SITE = "https://gmclip.com"
OG = f"{SITE}/assets/og-gmclip-v4.jpg"
RELEASE = "https://github.com/aj47/gmclip-video/releases/download/v0.1.0"

out.mkdir(exist_ok=True)
for name in ("index.html", "styles.css", "signup.js", "a.js"):
    shutil.copy2(root / name, out / name)
shutil.copytree(root / "assets", out / "assets", dirs_exist_ok=True)
(out / "_routes.json").write_text('{"version":1,"include":["/api/*"],"exclude":[]}\n')


def parse(path):
    text = path.read_text()
    m = re.match(r"<!--\n(.*?)\n-->\n", text, re.S)
    meta = dict(line.split(": ", 1) for line in m.group(1).splitlines() if line.strip())
    meta["body"] = text[m.end():]
    meta["slug"] = path.stem
    return meta


pages = [parse(p) for p in sorted((root / "pages").glob("*.html"))]
footer_links = "\n".join(
    f'        <a href="/{p["slug"]}">{escape(p.get("nav", p["h1"]))}</a>' for p in pages
)

SHELL = """<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>{title}</title>
  <meta name="description" content="{description}" />
  <link rel="canonical" href="{url}" />
  <meta property="og:title" content="{title}" />
  <meta property="og:description" content="{description}" />
  <meta property="og:type" content="article" />
  <meta property="og:site_name" content="gmclip" />
  <meta property="og:url" content="{url}" />
  <meta property="og:image" content="{og}" />
  <meta property="og:image:width" content="1200" />
  <meta property="og:image:height" content="630" />
  <meta name="twitter:card" content="summary_large_image" />
  <meta name="twitter:title" content="{title}" />
  <meta name="twitter:description" content="{description}" />
  <meta name="twitter:image" content="{og}" />
  <meta name="theme-color" content="#fff8ec" />
  <link rel="icon" type="image/svg+xml" href="/assets/icon.svg" />
  <link rel="icon" type="image/png" href="/assets/favicon.png" />
  <link rel="apple-touch-icon" href="/assets/apple-touch-icon.png" />
  <link rel="stylesheet" href="/styles.css" />
  <script type="application/ld+json">{ld}</script>
</head>
<body class="article-page">
  <header class="nav">
    <div class="wrap">
      <a class="nav-logo" href="/" aria-label="gmclip home"><img src="/assets/logo.png" alt="gmclip" width="1039" height="360" /></a>
      <nav class="nav-links" aria-label="Site">
        <a href="/#features">Features</a>
        <a href="/#agents">Agents</a>
        <a href="https://github.com/aj47/gmclip-video" rel="noopener">GitHub</a>
      </nav>
      <a class="btn btn-primary" href="#download" data-cta="nav">Download</a>
    </div>
  </header>
  <main>
    <article class="article wrap">
      <p class="crumbs"><a href="/">gmclip</a> / {crumb}</p>
      <h1>{h1}</h1>
      <p class="lede">{lede}</p>
      <p class="updated">Updated {updated}</p>
{body}
      <aside class="article-cta" id="download">
        <img src="/assets/icon-512.png" alt="" width="120" height="120" />
        <div>
          <h2>Try gmclip</h2>
          <p>Free and open source for macOS and Windows. No account, and your footage stays on your machine.</p>
          <div class="hero-ctas">
            <a class="btn btn-primary" href="{release}/gmclip-0.1.0-macos-arm64.dmg" data-cta="article">Mac <span class="btn-sub">Apple chip</span></a>
            <a class="btn btn-sun" href="{release}/gmclip-0.1.0-macos-x64.dmg" data-cta="article">Mac <span class="btn-sub">Intel</span></a>
            <a class="btn" href="{release}/gmclip-0.1.0-windows-x64.msi" data-cta="article">Windows <span class="btn-sub">x64</span></a>
          </div>
          <p class="hero-note">The installers aren’t signed yet; the <a href="https://github.com/aj47/gmclip-video#download" rel="noopener">README</a> shows how to open them the first time.</p>
        </div>
      </aside>
    </article>
  </main>
  <footer>
    <div class="wrap">
      <img src="/assets/logo.png" alt="gmclip" width="1039" height="360" />
      <nav class="footer-links" aria-label="Guides">
{footer_links}
      </nav>
      <span>© 2026 gmclip · MIT or Apache-2.0</span>
    </div>
  </footer>
  <script src="/a.js" defer></script>
</body>
</html>
"""

import json
for p in pages:
    url = SITE + "/" + p["slug"]
    ld = json.dumps({
        "@context": "https://schema.org", "@type": "Article", "headline": p["h1"],
        "description": p["description"], "dateModified": p["updated"], "url": url,
        "image": OG, "publisher": {"@type": "Organization", "name": "gmclip", "url": SITE + "/"},
        "about": {"@type": "SoftwareApplication", "name": "gmclip", "applicationCategory": "MultimediaApplication",
                  "operatingSystem": "macOS, Windows", "offers": {"@type": "Offer", "price": "0", "priceCurrency": "USD"}},
    }, ensure_ascii=False)
    html = SHELL.format(
        title=escape(p["title"]), description=escape(p["description"]), url=url, og=OG, ld=ld,
        crumb=escape(p.get("nav", p["h1"])), h1=escape(p["h1"]), lede=p["lede"], updated=p["updated"],
        body=p["body"].rstrip(), release=RELEASE, footer_links=footer_links)
    (out / f"{p['slug']}.html").write_text(html)

# The home page lists the guides in its footer too.
index = (out / "index.html").read_text().replace("<!-- guides -->", footer_links)
(out / "index.html").write_text(index)

urls = [SITE + "/"] + [f"{SITE}/{p['slug']}" for p in pages]
(out / "sitemap.xml").write_text(
    '<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n'
    + "".join(f"  <url><loc>{u}</loc></url>\n" for u in urls) + "</urlset>\n")
(out / "robots.txt").write_text(f"User-agent: *\nAllow: /\n\nSitemap: {SITE}/sitemap.xml\n")
print(out)
