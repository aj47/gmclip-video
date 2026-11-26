// Notes which link or site started this visit (utm_* tags, or the referring site) and which
// download, GitHub or updates link it clicked, at /api/e. No cookies, nothing about the person.
// See ANALYTICS.md.
(() => {
  const KEY = 'gm-src';
  const q = new URLSearchParams(location.search);
  let src = null;
  try { src = JSON.parse(sessionStorage.getItem(KEY)); } catch { /* private mode */ }
  let ref = '';
  try { ref = document.referrer ? new URL(document.referrer).hostname : ''; } catch { /* none */ }
  // A new tag or an outside referrer starts a new visit; moving between our pages keeps it.
  const starts = !!(q.get('utm_source') || (ref && !ref.endsWith('gmclip.com')) || !src);
  if (starts) {
    const search = /(^|\.)(google|bing|duckduckgo|yahoo|ecosia|brave|yandex|baidu|kagi)\./.test(ref);
    src = {
      s: q.get('utm_source') || ref || '(direct)',
      m: q.get('utm_medium') || (search ? 'organic' : ref ? 'referral' : '(none)'),
      c: q.get('utm_campaign') || '', ct: q.get('utm_content') || location.pathname, t: q.get('utm_term') || '',
    };
    try { sessionStorage.setItem(KEY, JSON.stringify(src)); } catch { /* private mode */ }
  }
  const send = (n, d) => {
    const body = JSON.stringify({ n, d, u: location.href, r: document.referrer, ...src });
    if (!navigator.sendBeacon?.('/api/e', body)) fetch('/api/e', { method: 'POST', body, keepalive: true }).catch(() => {});
  };
  window.gmCount = send;
  if (starts) send('visit');

  // detail: download:<file kind>, github, release or updates
  document.addEventListener('click', (ev) => {
    const el = ev.target.closest?.('a[href], [data-signup-open]');
    if (!el) return;
    if (el.matches('[data-signup-open]')) return send('cta', 'updates');
    const href = el.getAttribute('href');
    let d = null;
    if (/\/releases\/download\//.test(href)) d = 'download:' + (/arm64/.test(href) ? 'mac-arm' : /x64\.dmg/.test(href) ? 'mac-intel' : /windows/.test(href) ? 'windows' : 'other');
    else if (/\/releases/.test(href)) d = 'release';
    else if (/github\.com\//.test(href)) d = 'github';
    if (d) send('cta', d + (el.dataset.cta ? ':' + el.dataset.cta : ''));
  }, true);
})();
