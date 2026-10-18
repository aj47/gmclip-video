const reply = (data, status = 200) => Response.json(data, {
  status, headers: { 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' },
});

export async function onRequest({ request, env }) {
  if (request.method !== 'POST') return reply({ error: 'Use the signup form to join.' }, 405);
  const origin = request.headers.get('Origin');
  if (origin && origin !== new URL(request.url).origin) return reply({ error: 'Please sign up on gmclip.com.' }, 403);
  if (request.headers.get('Content-Type')?.split(';')[0] !== 'application/json') return reply({ error: 'Invalid request.' }, 415);
  let body;
  try {
    // Bound actual bytes, including requests without Content-Length.
    const reader = request.body?.getReader();
    let size = 0;
    const chunks = [];
    if (!reader) throw new Error('empty');
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > 4096) { await reader.cancel(); return reply({ error: 'Request is too large.' }, 413); }
      chunks.push(value);
    }
    body = JSON.parse(await new Blob(chunks).text());
  } catch { return reply({ error: 'Invalid request.' }, 400); }
  if (!body || typeof body !== 'object') return reply({ error: 'Invalid request.' }, 400);
  if (body.website) return reply({ ok: true });
  const email = typeof body.email === 'string' ? body.email.trim().toLowerCase() : '';
  if (email.length > 254 || !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) return reply({ error: 'Please enter a valid email address.' }, 400);
  try {
    const hour = Math.floor(Date.now() / 3600000);
    const ip = request.headers.get('CF-Connecting-IP') || 'local';
    const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(`${hour}:${ip}`));
    const key = Array.from(new Uint8Array(digest), n => n.toString(16).padStart(2, '0')).join('');
    const limit = await env.DB.prepare('INSERT INTO signup_limits (key, hour, count) VALUES (?, ?, 1) ON CONFLICT(key) DO UPDATE SET count = count + 1 RETURNING count').bind(key, hour).first();
    if (limit.count > 10) return reply({ error: 'Too many attempts. Please try again in an hour.' }, 429);
    await env.DB.batch([
      env.DB.prepare('DELETE FROM signup_limits WHERE hour < ?').bind(hour - 24),
      env.DB.prepare("INSERT INTO signups (email, consent_version) VALUES (?, 'early-access-v1') ON CONFLICT(email) DO NOTHING").bind(email),
    ]);
    // Same response for existing addresses; do not expose membership.
    return reply({ ok: true });
  } catch {
    return reply({ error: 'We couldn’t save your email. Please try again shortly.' }, 503);
  }
}
