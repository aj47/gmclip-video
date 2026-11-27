// POST /api/e: counts where a visit came from and which download or GitHub link it clicked
// (a.js sends them; see ../ANALYTICS.md). Nothing about the person is stored: the IP address
// is hashed with the hour only to cap a flood, and that hash is deleted after a day.
const EVENTS = ['visit', 'cta', 'signup'];
const BOT = /bot|crawl|spider|slurp|preview|headless|lighthouse|facebookexternalhit/i;
const text = (v, max = 120) => (typeof v === 'string' && v ? v.slice(0, max) : null);

export async function onRequest({ request, env, waitUntil }) {
  // sendBeacon posts text/plain, so there is no preflight and nothing in the reply is read.
  const done = new Response(null, { status: 204, headers: { 'Cache-Control': 'no-store' } });
  if (request.method !== 'POST' || !env.DB) return done;
  const self = new URL(request.url).origin;
  if (request.headers.get('Origin') !== self || BOT.test(request.headers.get('User-Agent') || '')) return done;
  let e;
  try {
    const raw = await request.text();
    if (raw.length > 4096) return done;
    e = JSON.parse(raw);
  } catch { return done; }
  if (!e || !EVENTS.includes(e.n)) return done;
  let page;
  try { page = new URL(e.u); } catch { return done; }
  if (page.origin !== self) return done;
  let ref = null;
  try {
    const r = new URL(e.r).hostname;
    if (!r.endsWith('gmclip.com')) ref = r;
  } catch { /* none */ }
  waitUntil((async () => {
    const hour = Math.floor(Date.now() / 3600000);
    const ip = request.headers.get('CF-Connecting-IP') || 'local';
    const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(`e:${hour}:${ip}`));
    const key = 'e:' + Array.from(new Uint8Array(digest), n => n.toString(16).padStart(2, '0')).join('');
    const limit = await env.DB.prepare('INSERT INTO signup_limits (key, hour, count) VALUES (?, ?, 1) ON CONFLICT(key) DO UPDATE SET count = count + 1 RETURNING count').bind(key, hour).first();
    if (limit.count > 120) return;
    if (limit.count === 1) await env.DB.prepare('DELETE FROM signup_limits WHERE hour < ?').bind(hour - 24).run();
    await env.DB.prepare('INSERT INTO events (day, name, path, ref, source, medium, campaign, content, term, detail) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)').bind(
      new Date().toISOString().slice(0, 10), e.n, text(page.pathname, 200), text(ref),
      text(e.s), text(e.m), text(e.c), text(e.ct), text(e.t), text(e.d)).run();
  })().catch(err => console.error(`[count] ${err.message}`)));
  return done;
}
