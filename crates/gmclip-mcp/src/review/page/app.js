'use strict';
// gmclip review page (SPEC §5.14.1; SR R-REV-2/5; decision 0015). Every text from the
// server is set with textContent: no HTML is ever parsed from data. The link token arrives
// in the URL fragment, is exchanged once for an HttpOnly cookie, and is stripped from the
// address bar before anything else happens.
//
// Phone first: each cut is a card with its latest export, comment pins (a time and an
// optional box drawn on the frame) on the video and as markers under it, threads with
// replies and resolve, two versions side by side or A/B with synced playback, Approve
// (to Clippy's calendar, under the Owner's rules), review notes; an editor's link
// suggests edits instead (trim, strike, caption height), which wait for the owner.
(function () {
  var statusEl = document.getElementById('status');
  var main = document.getElementById('main');
  var csrf = '';
  var role = 'reviewer';
  // what a write said, kept across the re-render it triggers (by cut and place)
  var flash = {};
  // the kind of edit picked per cut: a reload (after a comment) draws the form again, and
  // a kind picked meanwhile used to fall back to "trim" under the person's finger
  var kinds = {};
  // how many times the page has drawn what /api/home said (tests wait on it)
  var loads = 0;

  function el(tag, text, cls) {
    var e = document.createElement(tag);
    if (text !== undefined && text !== null) e.textContent = String(text);
    if (cls) e.className = cls;
    return e;
  }
  function button(text, cls, onClick) {
    var b = el('button', text, cls);
    b.type = 'button';
    b.addEventListener('click', onClick);
    return b;
  }
  function say(t) { statusEl.textContent = t; }
  function clock(s) {
    s = Math.max(0, s || 0);
    var m = Math.floor(s / 60);
    var r = (s - m * 60).toFixed(1);
    return m + ':' + (r.length < 4 ? '0' + r : r);
  }
  function who(a) {
    if (!a) return '';
    if (a === 'human') return 'the editor';
    if (a.indexOf('review-link:') === 0) return 'review link ' + a.slice(12, 20);
    if (a.indexOf('launched:') === 0) return 'agent run';
    return a;
  }

  function post(path, body) {
    return fetch(path, {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json', 'X-CSRF': csrf },
      body: JSON.stringify(body)
    });
  }
  function postJson(path, body) {
    return post(path, body).then(function (r) {
      return r.json().then(function (j) { return { ok: r.ok, status: r.status, body: j }; },
        function () { return { ok: r.ok, status: r.status, body: {} }; });
    });
  }

  function video(src) {
    var v = document.createElement('video');
    // the page draws its own controls (player below): the browser's go away in fullscreen
    // on phones, and they would cover the frame the reviewer is looking at
    v.controls = false;
    v.preload = 'metadata';
    v.playsInline = true;
    v.setAttribute('playsinline', '');
    v.setAttribute('webkit-playsinline', '');
    v.src = src;
    return v;
  }

  // ---- the player -----------------------------------------------------------------
  // Overlay controls that never shrink the picture. Tap shows or hides them; double-tap
  // the left or right third skips 5 s (keep tapping to skip more); double-tap the middle
  // goes fullscreen; drag sideways to scrub; in fullscreen, swipe down to leave. Keys when
  // focused: space/k play, ←/→ 5 s, j/l 10 s, , and . a frame, m mute, f fullscreen.
  // Fullscreen is the player's own box (pins and controls come along); where a page may
  // not go fullscreen (iPhone Safari) the box fills the window instead.

  var SVGNS = 'http://www.w3.org/2000/svg';
  var ICONS = {
    play: [{ d: 'M8 5.5v13l10.5-6.5z' }],
    pause: [{ d: 'M7 5h3.5v14H7zM13.5 5H17v14h-3.5z' }],
    back: [{ d: 'M12 5V2L7.5 6 12 10V7a5.5 5.5 0 1 1-5.5 5.5H4.5A7.5 7.5 0 1 0 12 5z' }],
    sound: [{ d: 'M3.5 9.5v5h3.5l4.5 4v-13l-4.5 4z' }, { d: 'M15 9a4 4 0 0 1 0 6M17.5 6.5a7.5 7.5 0 0 1 0 11', stroke: true }],
    muted: [{ d: 'M3.5 9.5v5h3.5l4.5 4v-13l-4.5 4z' }, { d: 'M15.5 9.5l5 5M20.5 9.5l-5 5', stroke: true }],
    full: [{ d: 'M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5', stroke: true }],
    unfull: [{ d: 'M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5', stroke: true }],
    comment: [{ d: 'M4 5h16v11H10l-5 4v-4H4z', stroke: true }],
    help: [{ d: 'M9.5 9.5a2.5 2.5 0 1 1 3.5 2.3c-.6.3-1 .9-1 1.6v.6', stroke: true }, { d: 'M12 17.5v.01', stroke: true }]
  };
  function icon(name) {
    var s = document.createElementNS(SVGNS, 'svg');
    s.setAttribute('viewBox', '0 0 24 24');
    s.setAttribute('width', '24');
    s.setAttribute('height', '24');
    s.setAttribute('aria-hidden', 'true');
    ICONS[name].forEach(function (p) {
      var e = document.createElementNS(SVGNS, 'path');
      e.setAttribute('d', p.d);
      if (p.stroke) {
        e.setAttribute('fill', 'none');
        e.setAttribute('stroke', 'currentColor');
        e.setAttribute('stroke-width', '2');
        e.setAttribute('stroke-linecap', 'round');
        e.setAttribute('stroke-linejoin', 'round');
      } else {
        e.setAttribute('fill', 'currentColor');
      }
      s.appendChild(e);
    });
    return s;
  }
  function setIcon(b, name, label) {
    b.textContent = '';
    b.appendChild(icon(name));
    b.setAttribute('aria-label', label);
  }
  function clockShort(s) {
    s = Math.max(0, Math.floor(s || 0));
    var m = Math.floor(s / 60), r = s - m * 60;
    return m + ':' + (r < 10 ? '0' : '') + r;
  }
  function fullscreenEl() { return document.fullscreenElement || document.webkitFullscreenElement || null; }

  function player(v, opts) {
    opts = opts || {};
    var root = el('div', null, 'player');
    root.tabIndex = 0;
    root.setAttribute('role', 'group');
    root.setAttribute('aria-label', 'Video player');
    root.appendChild(v);
    var hit = el('div', null, 'p-hit');
    var flashEl = el('div', null, 'p-flash');
    var bubble = el('div', null, 'p-bubble');
    var chrome = el('div', null, 'p-chrome');

    // top (fullscreen only): the title and a way back to the comment box
    var top = el('div', null, 'p-top');
    top.appendChild(el('span', opts.title || '', 'p-title'));
    if (opts.onComment) {
      var cb = el('button', null, 'p-comment');
      cb.type = 'button';
      cb.appendChild(icon('comment'));
      cb.appendChild(el('span', 'Comment here'));
      cb.setAttribute('aria-label', 'Leave fullscreen and comment at this moment');
      cb.addEventListener('click', function () { v.pause(); exitFs(); opts.onComment(); });
      top.appendChild(cb);
    }
    var shut = el('button', null, 'p-close');
    shut.type = 'button';
    setIcon(shut, 'unfull', 'Leave fullscreen');
    shut.addEventListener('click', function () { exitFs(); });
    top.appendChild(shut);

    // middle: back 5, play, ahead 5
    var mid = el('div', null, 'p-center');
    function skipBtn(dir) {
      var b = el('button', null, 'p-skip' + (dir > 0 ? ' ahead' : ''));
      b.type = 'button';
      b.appendChild(icon('back'));
      b.appendChild(el('span', '5'));
      b.setAttribute('aria-label', dir > 0 ? 'Ahead 5 seconds' : 'Back 5 seconds');
      b.addEventListener('click', function () { skip(dir * 5); });
      return b;
    }
    var playBtn = el('button', null, 'p-play');
    playBtn.type = 'button';
    playBtn.addEventListener('click', function () { toggle(); });
    mid.appendChild(skipBtn(-1));
    mid.appendChild(playBtn);
    mid.appendChild(skipBtn(1));

    // bottom: the scrub bar with comment ticks, then time, speed, sound, fullscreen
    var bottom = el('div', null, 'p-bottom');
    var track = el('div', null, 'p-track');
    track.setAttribute('role', 'slider');
    track.setAttribute('aria-label', 'Seek');
    track.tabIndex = -1;
    var rail = el('div', null, 'p-rail');
    var buf = el('div', null, 'p-buf');
    var played = el('div', null, 'p-played');
    var ticks = el('div', null, 'p-ticks');
    var thumb = el('div', null, 'p-thumb');
    rail.appendChild(buf);
    rail.appendChild(played);
    track.appendChild(rail);
    track.appendChild(ticks);
    track.appendChild(thumb);
    var row = el('div', null, 'p-row');
    var time = el('span', '0:00', 'p-time');
    var speed = el('button', '1×', 'p-speed');
    speed.type = 'button';
    speed.setAttribute('aria-label', 'Speed 1×');
    var SPEEDS = [1, 1.5, 2, 0.5];
    speed.addEventListener('click', function () {
      var i = SPEEDS.indexOf(v.playbackRate);
      v.playbackRate = SPEEDS[(i + 1) % SPEEDS.length];
    });
    var mute = el('button', null, 'p-mute');
    mute.type = 'button';
    mute.addEventListener('click', function () { v.muted = !v.muted; });
    var fsBtn = el('button', null, 'p-fs');
    fsBtn.type = 'button';
    fsBtn.addEventListener('click', function () { toggleFs(); });
    row.appendChild(time);
    row.appendChild(speed);
    row.appendChild(mute);
    row.appendChild(fsBtn);
    var helpBtn = el('button', null, 'p-helpbtn');
    helpBtn.type = 'button';
    setIcon(helpBtn, 'help', 'Gestures and keys');
    helpBtn.addEventListener('click', function () { help(true); });
    row.insertBefore(helpBtn, speed);

    // the gestures card: inside the player, so it works in fullscreen too
    var helpEl = el('div', null, 'p-help');
    helpEl.setAttribute('role', 'dialog');
    helpEl.setAttribute('aria-label', 'Gestures and keys');
    var card = el('div', null, 'p-help-card');
    card.appendChild(el('h4', 'Gestures'));
    function list(rows, cls) {
      var dl = el('dl', null, cls);
      rows.forEach(function (r) { dl.appendChild(el('dt', r[0])); dl.appendChild(el('dd', r[1])); });
      return dl;
    }
    card.appendChild(list([
      ['Tap', 'Show or hide the controls'],
      ['Double-tap left / right', 'Back or ahead 5 s (keep tapping for more)'],
      ['Double-tap the middle', 'Fullscreen on or off'],
      ['Drag sideways', 'Scrub through the clip'],
      ['Swipe down', 'Leave fullscreen'],
      ['Drag the bar', 'Jump to a moment; yellow ticks are comments']
    ], 'p-help-touch'));
    var keysH = el('h4', 'Keys', 'p-help-keys');
    card.appendChild(keysH);
    card.appendChild(list([
      ['Space / K', 'Play or pause'],
      ['← / →', 'Back or ahead 5 s'],
      ['J / L', 'Back or ahead 10 s'],
      [', / .', 'One frame back or ahead'],
      ['M', 'Mute'],
      ['F', 'Fullscreen'],
      ['?', 'This card']
    ], 'p-help-keys'));
    var done = el('button', 'Got it', 'p-help-done');
    done.type = 'button';
    done.addEventListener('click', function () { help(false); });
    card.appendChild(done);
    helpEl.appendChild(card);
    helpEl.addEventListener('click', function (e) { if (e.target === helpEl) help(false); });
    function help(on) {
      if (on) { hint(false); v.pause(); }
      root.classList.toggle('helping', on);
      if (on) done.focus({ preventScroll: true }); else { root.focus({ preventScroll: true }); show(); }
    }

    // the first play in this browser says where the gestures are
    var hintEl = el('div', 'Double-tap a side to skip · drag to scrub · ? for all gestures', 'p-hint');
    var hintT = null;
    function hint(on) {
      clearTimeout(hintT);
      hintEl.classList.toggle('on', on);
      if (on) hintT = setTimeout(function () { hint(false); }, 4500);
    }
    function firstPlay() {
      var seen = true;
      try { seen = localStorage.getItem('gmclip.review.gestures') === '1'; localStorage.setItem('gmclip.review.gestures', '1'); } catch (_) { /* no storage: no hint */ }
      if (!seen) hint(true);
      v.removeEventListener('play', firstPlay);
    }
    v.addEventListener('play', firstPlay);
    bottom.appendChild(track);
    bottom.appendChild(row);

    chrome.appendChild(top);
    chrome.appendChild(mid);
    chrome.appendChild(bottom);
    root.appendChild(hit);
    root.appendChild(flashEl);
    root.appendChild(bubble);
    root.appendChild(chrome);
    root.appendChild(hintEl);
    root.appendChild(helpEl);

    function dur() { return (v.duration && isFinite(v.duration)) ? v.duration : (opts.duration || 0); }
    function clampT(t) { return Math.max(0, Math.min(dur() || 0, t)); }
    function seek(t) {
      t = clampT(t);
      v.currentTime = t;
      paint(t);
    }
    function skip(d) {
      seek(v.currentTime + d);
      show();
    }
    function toggle() {
      if (v.paused || v.ended) { var p = v.play(); if (p && p.catch) p.catch(function () {}); } else v.pause();
    }

    // what shows
    var hideT = null;
    function show() {
      root.classList.add('show');
      root.classList.remove('idle');
      clearTimeout(hideT);
      if (!v.paused && !dragging && !root.classList.contains('helping')) hideT = setTimeout(hide, 2600);
    }
    function hide() {
      clearTimeout(hideT);
      root.classList.remove('show');
      if (!v.paused) root.classList.add('idle');
    }
    var flashT = null;
    function flash(text, where) {
      flashEl.textContent = text;
      flashEl.className = 'p-flash on ' + where;
      clearTimeout(flashT);
      flashT = setTimeout(function () { flashEl.classList.remove('on'); }, 550);
    }
    function say2(text) {
      bubble.textContent = text;
      bubble.classList.toggle('on', !!text);
    }

    function paint(t) {
      if (t === undefined) t = v.currentTime;
      var d = dur();
      var f = d ? Math.max(0, Math.min(1, t / d)) : 0;
      played.style.width = (f * 100) + '%';
      thumb.style.left = (f * 100) + '%';
      var b = 0;
      try {
        for (var i = 0; i < v.buffered.length; i++) {
          if (v.buffered.start(i) <= t + 0.5) b = Math.max(b, v.buffered.end(i));
        }
      } catch (_) { /* nothing buffered yet */ }
      buf.style.width = (d ? Math.min(100, 100 * b / d) : 0) + '%';
      time.textContent = clockShort(t) + ' / ' + clockShort(d);
      track.setAttribute('aria-valuemin', '0');
      track.setAttribute('aria-valuemax', String(Math.round(d)));
      track.setAttribute('aria-valuenow', String(Math.round(t)));
      track.setAttribute('aria-valuetext', clockShort(t) + ' of ' + clockShort(d));
    }
    var tickList = [];
    function drawTicks() {
      ticks.textContent = '';
      var d = dur();
      if (!d) return;
      tickList.forEach(function (k) {
        var e = el('div', null, 'p-tick' + (k.resolved ? ' resolved' : ''));
        e.style.left = Math.min(100, Math.max(0, 100 * k.t / d)) + '%';
        ticks.appendChild(e);
      });
    }
    function state() {
      var paused = v.paused || v.ended;
      setIcon(playBtn, paused ? 'play' : 'pause', paused ? 'Play' : 'Pause');
      root.classList.toggle('paused', paused);
      setIcon(mute, v.muted ? 'muted' : 'sound', v.muted ? 'Sound on' : 'Mute');
      speed.textContent = v.playbackRate + '×';
      speed.setAttribute('aria-label', 'Speed ' + v.playbackRate + '×');
      setIcon(fsBtn, isFs() ? 'unfull' : 'full', isFs() ? 'Leave fullscreen' : 'Fullscreen');
    }
    var raf = 0;
    function loop() {
      paint();
      raf = v.paused ? 0 : requestAnimationFrame(loop);
    }
    v.addEventListener('play', function () { state(); show(); if (!raf) raf = requestAnimationFrame(loop); });
    v.addEventListener('pause', function () { state(); show(); });
    v.addEventListener('ended', function () { state(); show(); });
    v.addEventListener('volumechange', state);
    v.addEventListener('ratechange', function () { state(); flash(v.playbackRate + '×', 'mid'); });
    ['timeupdate', 'seeked', 'progress', 'durationchange', 'loadedmetadata'].forEach(function (k) {
      v.addEventListener(k, function () { if (!dragging) paint(); if (k === 'durationchange' || k === 'loadedmetadata') drawTicks(); });
    });

    // fullscreen: the real one where allowed, else the box fills the window
    function isFs() { return root.classList.contains('fs'); }
    // a short box (a wide clip on a phone) keeps only the play button in the middle
    function fit() { root.classList.toggle('compact', !isFs() && root.clientHeight > 0 && root.clientHeight < 300); }
    function layout() { fit(); state(); paint(); if (opts.onLayout) setTimeout(opts.onLayout, 0); }
    if (window.ResizeObserver) new ResizeObserver(fit).observe(root);
    else { window.addEventListener('resize', fit); v.addEventListener('loadedmetadata', fit); }
    function enterFs() {
      if (isFs()) return;
      root.classList.add('fs');
      document.documentElement.classList.add('p-locked');
      var req = root.requestFullscreen || root.webkitRequestFullscreen;
      if (req) {
        try {
          var p = req.call(root, { navigationUI: 'hide' });
          // a wide clip turns the phone sideways (only allowed once really fullscreen)
          if (p && p.then) p.then(function () {
            if (v.videoWidth > v.videoHeight && screen.orientation && screen.orientation.lock) {
              screen.orientation.lock('landscape').catch(function () {});
            }
          }, function () {});
        } catch (_) { /* the window-filling box stands in */ }
      }
      root.focus({ preventScroll: true });
      show();
      layout();
    }
    function exitFs() {
      if (!isFs()) return;
      root.classList.remove('fs');
      document.documentElement.classList.remove('p-locked');
      if (fullscreenEl() === root) {
        var x = document.exitFullscreen || document.webkitExitFullscreen;
        if (x) { try { var p = x.call(document); if (p && p.catch) p.catch(function () {}); } catch (_) { /* already out */ } }
      }
      try { if (screen.orientation && screen.orientation.unlock) screen.orientation.unlock(); } catch (_) { /* nothing locked */ }
      layout();
    }
    function toggleFs() { if (isFs()) exitFs(); else enterFs(); }
    var wasReal = false;
    function onFsChange() {
      var real = fullscreenEl() === root;
      // left by the system (Esc, the back gesture): the window-filling box goes too
      if (wasReal && !real) exitFs();
      wasReal = real;
      layout();
    }
    document.addEventListener('fullscreenchange', onFsChange);
    document.addEventListener('webkitfullscreenchange', onFsChange);

    // gestures on the picture
    var g = null, last = null, dragging = false;
    hit.addEventListener('pointerdown', function (e) {
      if (e.button > 0) return;
      g = { id: e.pointerId, x: e.clientX, y: e.clientY, t0: v.currentTime, mode: null };
    });
    hit.addEventListener('pointermove', function (e) {
      if (e.pointerType === 'mouse' && !g) { show(); return; }
      if (!g || e.pointerId !== g.id) return;
      var dx = e.clientX - g.x, dy = e.clientY - g.y;
      if (!g.mode) {
        if (Math.abs(dx) > 12 && Math.abs(dx) > Math.abs(dy)) {
          g.mode = 'scrub';
          g.wasPlaying = !v.paused;
          dragging = true;
          v.pause();
          try { hit.setPointerCapture(e.pointerId); } catch (_) { /* fine without it */ }
        } else if (Math.abs(dy) > 12) {
          g.mode = 'swipe';
        }
      }
      if (g.mode === 'scrub') {
        // the width of the picture is the whole clip, up to two minutes of it
        var span = Math.min(dur() || 0, 120);
        var t = clampT(g.t0 + (dx / (hit.clientWidth || 1)) * span);
        var dt = t - g.t0;
        if (v.fastSeek && Math.abs(t - v.currentTime) > 0.5) v.fastSeek(t); else v.currentTime = t;
        paint(t);
        say2(clockShort(t) + '  (' + (dt >= 0 ? '+' : '−') + Math.abs(dt).toFixed(1) + ' s)');
        root.classList.add('show');
      }
    });
    function endDrag(cancel) {
      if (g && g.mode === 'scrub') {
        dragging = false;
        say2('');
        if (g.wasPlaying && !cancel) toggle();
        show();
      }
    }
    hit.addEventListener('pointerup', function (e) {
      if (!g || e.pointerId !== g.id) return;
      var dy = e.clientY - g.y;
      if (g.mode === 'scrub') endDrag(false);
      else if (g.mode === 'swipe') { if (isFs() && dy > 80) exitFs(); }
      else tap(e);
      g = null;
    });
    hit.addEventListener('pointercancel', function () { endDrag(true); g = null; });
    hit.addEventListener('dblclick', function (e) { e.preventDefault(); });
    hit.addEventListener('contextmenu', function (e) { if (e.pointerType !== 'mouse') e.preventDefault(); });

    function tap(e) {
      var now = Date.now();
      var r = hit.getBoundingClientRect();
      var zone = e.clientX < r.left + r.width / 3 ? -1 : (e.clientX > r.left + r.width * 2 / 3 ? 1 : 0);
      // a skip run: once double-tapped, each further tap on that side skips again
      if (last && last.zone === zone && zone !== 0 && (now - last.at) < (last.run ? 700 : 320)) {
        last.run = (last.run || 0) + 5;
        last.at = now;
        seek(v.currentTime + zone * 5);
        flash((zone > 0 ? '+' : '−') + last.run + ' s', zone > 0 ? 'right' : 'left');
        show();
        return;
      }
      if (last && last.zone === 0 && zone === 0 && (now - last.at) < 320) {
        last = null;
        toggleFs();
        return;
      }
      last = { at: now, zone: zone };
      if (root.classList.contains('show')) hide(); else show();
    }

    // the scrub bar
    function trackSeek(e) {
      var r = rail.getBoundingClientRect();
      var f = Math.max(0, Math.min(1, (e.clientX - r.left) / (r.width || 1)));
      var t = f * dur();
      seek(t);
      say2(clockShort(t));
    }
    var trackDrag = null;
    track.addEventListener('pointerdown', function (e) {
      if (e.button > 0) return;
      e.preventDefault();
      trackDrag = { id: e.pointerId, wasPlaying: !v.paused };
      dragging = true;
      v.pause();
      try { track.setPointerCapture(e.pointerId); } catch (_) { /* fine without it */ }
      trackSeek(e);
      root.classList.add('show');
    });
    track.addEventListener('pointermove', function (e) { if (trackDrag && e.pointerId === trackDrag.id) trackSeek(e); });
    function trackEnd(e) {
      if (!trackDrag || e.pointerId !== trackDrag.id) return;
      var resume = trackDrag.wasPlaying && e.type === 'pointerup';
      trackDrag = null;
      dragging = false;
      say2('');
      if (resume) toggle();
      show();
    }
    track.addEventListener('pointerup', trackEnd);
    track.addEventListener('pointercancel', trackEnd);

    // keys
    root.addEventListener('keydown', function (e) {
      if (e.target !== root && e.target !== track && !(e.target && e.target.closest && e.target.closest('.p-chrome'))) return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      var k = e.key, done = true;
      if (k === ' ' || k === 'k') { if (e.target.tagName === 'BUTTON' && k === ' ') return; toggle(); }
      else if (k === 'ArrowLeft') skip(-5);
      else if (k === 'ArrowRight') skip(5);
      else if (k === 'j') skip(-10);
      else if (k === 'l') skip(10);
      else if (k === ',') { v.pause(); seek(v.currentTime - 1 / 30); }
      else if (k === '.') { v.pause(); seek(v.currentTime + 1 / 30); }
      else if (k === 'm') v.muted = !v.muted;
      else if (k === 'f') toggleFs();
      else if (k === '?') help(!root.classList.contains('helping'));
      else if (k === 'Escape' && root.classList.contains('helping')) help(false);
      else if (k === 'Escape' && isFs()) exitFs();
      else done = false;
      if (done) { e.preventDefault(); show(); }
    });

    state();
    paint();
    show();
    return {
      root: root,
      video: v,
      // a layer between the picture and the controls (the comment pins)
      layer: function (node) { root.insertBefore(node, hit); },
      ticks: function (list) { tickList = list || []; drawTicks(); },
      exitFs: exitFs
    };
  }

  // The part of the video element the picture fills (object-fit: contain).
  function contentRect(v) {
    var w = v.clientWidth, h = v.clientHeight;
    var vw = v.videoWidth, vh = v.videoHeight;
    if (!vw || !vh || !w || !h) return { x: 0, y: 0, w: w, h: h };
    var s = Math.min(w / vw, h / vh);
    return { x: (w - vw * s) / 2, y: (h - vh * s) / 2, w: vw * s, h: vh * s };
  }

  // ---- one cut ------------------------------------------------------------------

  function cutCard(cut, exports) {
    var card = el('section', null, 'card cut-card');
    card.appendChild(el('h3', cut.title));
    var threads = cut.threads || [];
    var picked = { thread: null, box: null };

    // the player: the latest export, pins over it, markers under it
    var stage = null;
    var v = null, pins = null, markers = null, pl = null, input = null;
    var latest = exports[0];
    if (latest) {
      v = video('/x/' + encodeURIComponent(latest.id));
      pl = player(v, {
        title: cut.title,
        duration: latest.duration_s,
        onLayout: function () { drawPins(); },
        onComment: function () {
          if (!input) return;
          input.scrollIntoView({ block: 'center' });
          input.focus();
        }
      });
      stage = pl.root;
      stage.classList.add('stage');
      pins = el('div', null, 'pins');
      pl.layer(pins);
      card.appendChild(stage);
      markers = el('div', null, 'markers');
      markers.setAttribute('aria-label', 'Comments on the timeline');
      card.appendChild(markers);
      var meta = el('p', (latest.duration_s || 0).toFixed(1) + ' s · ' + exports.length + ' version' + (exports.length === 1 ? '' : 's'), 'muted');
      card.appendChild(meta);
    } else {
      card.appendChild(el('p', 'Not exported yet.', 'muted'));
    }

    function drawPins() {
      if (!v || !pins) return;
      pins.textContent = '';
      var cr = contentRect(v);
      var t = v.currentTime;
      threads.forEach(function (th) {
        if (!th.box || th.t === null || th.t === undefined) return;
        if (Math.abs(th.t - t) > 1.5 && picked.thread !== th.id) return;
        var b = el('div', null, 'pin' + (th.resolved ? ' resolved' : '') + (picked.thread === th.id ? ' picked' : ''));
        b.style.left = (cr.x + th.box.x * cr.w) + 'px';
        b.style.top = (cr.y + th.box.y * cr.h) + 'px';
        b.style.width = (th.box.w * cr.w) + 'px';
        b.style.height = (th.box.h * cr.h) + 'px';
        pins.appendChild(b);
      });
      if (picked.box) {
        var d = el('div', null, 'pin drawing');
        d.style.left = (cr.x + picked.box.x * cr.w) + 'px';
        d.style.top = (cr.y + picked.box.y * cr.h) + 'px';
        d.style.width = (picked.box.w * cr.w) + 'px';
        d.style.height = (picked.box.h * cr.h) + 'px';
        pins.appendChild(d);
      }
    }
    function drawMarkers() {
      if (!markers || !latest) return;
      markers.textContent = '';
      pl.ticks(threads.filter(function (th) { return th.t !== null && th.t !== undefined; })
        .map(function (th) { return { t: th.t, resolved: th.resolved }; }));
      var dur = (v && v.duration && isFinite(v.duration)) ? v.duration : (latest.duration_s || 1);
      threads.forEach(function (th) {
        if (th.t === null || th.t === undefined) return;
        var m = button('', 'marker' + (th.resolved ? ' resolved' : ''), function () {
          picked.thread = th.id;
          v.pause();
          v.currentTime = th.t;
          drawPins();
          var row = card.querySelector('[data-thread="' + th.id + '"]');
          if (row) row.scrollIntoView({ block: 'nearest' });
        });
        m.setAttribute('aria-label', 'Comment at ' + clock(th.t));
        m.style.left = Math.min(100, Math.max(0, 100 * th.t / dur)) + '%';
        markers.appendChild(m);
      });
    }
    if (v) {
      v.addEventListener('timeupdate', drawPins);
      v.addEventListener('seeked', drawPins);
      v.addEventListener('loadedmetadata', function () { drawPins(); drawMarkers(); });
      window.addEventListener('resize', drawPins);
      drawMarkers();
    }

    // drawing a box on the frame
    var drawing = false, start = null;
    if (pins) {
      pins.addEventListener('pointerdown', function (e) {
        if (!drawing) return;
        e.preventDefault();
        var r = pins.getBoundingClientRect(), cr = contentRect(v);
        start = { x: (e.clientX - r.left - cr.x) / cr.w, y: (e.clientY - r.top - cr.y) / cr.h };
        try { pins.setPointerCapture(e.pointerId); } catch (_) { /* some browsers refuse it for a synthetic pointer */ }
      });
      pins.addEventListener('pointermove', function (e) {
        if (!drawing || !start) return;
        var r = pins.getBoundingClientRect(), cr = contentRect(v);
        var x = Math.min(1, Math.max(0, (e.clientX - r.left - cr.x) / cr.w));
        var y = Math.min(1, Math.max(0, (e.clientY - r.top - cr.y) / cr.h));
        var sx = Math.min(1, Math.max(0, start.x)), sy = Math.min(1, Math.max(0, start.y));
        picked.box = { x: Math.min(sx, x), y: Math.min(sy, y), w: Math.abs(x - sx), h: Math.abs(y - sy) };
        drawPins();
      });
      pins.addEventListener('pointerup', function () {
        start = null;
        if (picked.box && (picked.box.w < 0.01 || picked.box.h < 0.01)) picked.box = null;
        drawPins();
      });
    }

    // the composer
    if (latest) {
      var comp = el('div', null, 'composer');
      input = el('input');
      input.type = 'text';
      input.maxLength = 2000;
      input.placeholder = 'Comment at this moment';
      input.setAttribute('aria-label', 'Comment');
      var boxBtn = button('Draw a box', 'ghost', function () {
        drawing = !drawing;
        boxBtn.setAttribute('aria-pressed', String(drawing));
        stage.classList.toggle('drawing', drawing);
        if (drawing) {
          v.pause();
          // the frame is where the finger goes next
          stage.scrollIntoView({ block: 'center' });
        } else { picked.box = null; drawPins(); }
      });
      boxBtn.setAttribute('aria-pressed', 'false');
      var out = el('p', flash[cut.id + ':comment'] || '', 'muted small');
      delete flash[cut.id + ':comment'];
      var send = button('Comment', 'primary', function () {
        var text = input.value.trim();
        if (!text) { out.textContent = 'Write something first.'; return; }
        var body = { cut: cut.id, text: text, t: Math.max(0, v.currentTime || 0), export_ref: latest.id };
        if (picked.box) body.box = picked.box;
        postJson('/api/comment', body).then(function (r) {
          if (r.ok) {
            out.textContent = 'Comment added at ' + clock(body.t) + '.';
            flash[cut.id + ':comment'] = out.textContent;
            input.value = '';
            picked.box = null;
            drawing = false;
            stage.classList.remove('drawing');
            boxBtn.setAttribute('aria-pressed', 'false');
            load();
          } else {
            out.textContent = 'Not added (' + (r.body.error || r.status) + ').';
          }
        });
      });
      var row = el('div', null, 'row');
      row.appendChild(input);
      comp.appendChild(row);
      var row2 = el('div', null, 'row');
      row2.appendChild(boxBtn);
      row2.appendChild(send);
      comp.appendChild(row2);
      comp.appendChild(out);
      card.appendChild(comp);
    }

    // the threads
    var list = el('div', null, 'threads');
    list.appendChild(el('h4', threads.length ? 'Comments' : 'No comments yet'));
    threads.forEach(function (th) {
      var t = el('div', null, 'thread' + (th.resolved ? ' resolved' : ''));
      t.setAttribute('data-thread', th.id);
      var head = el('div', null, 'row');
      if (th.t !== null && th.t !== undefined) {
        head.appendChild(button(clock(th.t), 'chip', function () {
          picked.thread = th.id;
          if (v) { v.pause(); v.currentTime = th.t; drawPins(); }
        }));
      } else if (th.pinned) {
        head.appendChild(el('span', 'not in the cut now', 'chip muted'));
      }
      head.appendChild(el('span', who(th.author) + (th.box ? ' · box' : ''), 'muted small'));
      t.appendChild(head);
      t.appendChild(el('p', th.text, 'untrusted'));
      (th.replies || []).forEach(function (r) {
        t.appendChild(el('p', who(r.author) + ': ' + r.text, 'untrusted reply'));
      });
      var rrow = el('div', null, 'row');
      var ri = el('input');
      ri.type = 'text';
      ri.maxLength = 2000;
      ri.placeholder = 'Reply';
      ri.setAttribute('aria-label', 'Reply');
      rrow.appendChild(ri);
      rrow.appendChild(button('Reply', 'ghost', function () {
        if (!ri.value.trim()) return;
        postJson('/api/comment', { cut: cut.id, text: ri.value, reply_to: th.id }).then(function (r) {
          if (r.ok) load();
        });
      }));
      rrow.appendChild(button(th.resolved ? 'Reopen' : 'Resolve', 'ghost', function () {
        postJson('/api/resolve', { comment: th.id, resolved: !th.resolved }).then(function (r) {
          if (r.ok) load();
        });
      }));
      t.appendChild(rrow);
      list.appendChild(t);
    });
    card.appendChild(list);

    // suggested edits (everyone sees them; editors make them)
    var sugs = cut.suggestions || [];
    if (sugs.length) {
      var sl = el('div', null, 'suggestions');
      sl.appendChild(el('h4', 'Suggested edits'));
      sugs.forEach(function (s) {
        var p = el('p', null, 'suggestion ' + s.state);
        p.appendChild(el('b', s.state === 'proposed' ? 'waiting' : s.state));
        p.appendChild(document.createTextNode(' · ' + s.summary + ' · ' + who(s.author)));
        if (s.note) p.appendChild(el('span', ' — ' + s.note, 'untrusted'));
        sl.appendChild(p);
      });
      card.appendChild(sl);
    }
    if (role === 'editor') card.appendChild(suggestForm(cut, function () { return v ? v.currentTime : 0; }));

    // versions: compare two
    if (exports.length > 1) card.appendChild(compare(exports));

    // approve and review notes (a reviewer's link)
    if (role !== 'editor') {
      if (latest) {
        var ap = el('div', null, 'row');
        var apOut = el('p', '', 'muted small');
        ap.appendChild(button('Approve → Clippy calendar', 'primary', function () {
          postJson('/api/approve', { export: latest.id }).then(function (r) {
            if (r.body.sent) apOut.textContent = 'Sent to Clippy’s calendar as a draft. Approving the post stays in Clippy.';
            else if (r.ok) apOut.textContent = 'Waiting for you in the app (' + (r.body.reasons || []).join('; ') + ').';
            else apOut.textContent = 'Not sent (' + (r.body.error || r.status) + ').';
          });
        }));
        card.appendChild(ap);
        card.appendChild(apOut);
      }
      if (cut.notes) card.appendChild(el('p', cut.notes, 'untrusted muted'));
      (cut.reviews || []).forEach(function (r) {
        card.appendChild(el('p', r.verdict + ' · ' + r.author + ': ' + r.text, 'untrusted'));
      });
      card.appendChild(reviewForm(cut.id, null));
    }
    return card;
  }

  // ---- suggesting an edit (editor links) ---------------------------------------------

  function suggestForm(cut, now) {
    var box = el('div', null, 'suggest');
    box.appendChild(el('h4', 'Suggest an edit'));
    var kind = document.createElement('select');
    kind.setAttribute('aria-label', 'Kind of edit');
    [['trim', 'Trim a part'], ['strike', 'Cut a stretch out'], ['caption_move', 'Move the captions']].forEach(function (k) {
      var o = el('option', k[1]);
      o.value = k[0];
      kind.appendChild(o);
    });
    var fields = el('div', null, 'fields');
    var note = el('input');
    note.type = 'text';
    note.maxLength = 2000;
    note.placeholder = 'Why (optional)';
    note.setAttribute('aria-label', 'Why');
    var out = el('p', flash[cut.id + ':suggest'] || '', 'muted small');
    delete flash[cut.id + ':suggest'];
    var state = {};
    function num(label, key, step) {
      var l = el('label', label + ' ');
      var i = el('input');
      i.type = 'number';
      i.step = step || '0.1';
      i.min = '0';
      i.value = '0';
      i.addEventListener('input', function () { state[key] = parseFloat(i.value) || 0; });
      state[key] = 0;
      l.appendChild(i);
      return l;
    }
    function render() {
      fields.textContent = '';
      state = { kind: kind.value };
      if (kind.value === 'trim') {
        var sel = document.createElement('select');
        sel.setAttribute('aria-label', 'Part');
        (cut.parts || []).forEach(function (p) {
          var o = el('option', 'Part ' + p.n + ' (' + clock(p.start_s) + '–' + clock(p.end_s) + ')');
          o.value = p.id;
          sel.appendChild(o);
        });
        state.part = sel.value;
        sel.addEventListener('change', function () { state.part = sel.value; });
        fields.appendChild(sel);
        fields.appendChild(num('Off its start (s)', 'head_s'));
        fields.appendChild(num('Off its end (s)', 'tail_s'));
      } else if (kind.value === 'strike') {
        var from = el('span', 'from —', 'chip');
        var to = el('span', 'to —', 'chip');
        fields.appendChild(button('From here', 'ghost', function () { state.from_s = now(); from.textContent = 'from ' + clock(state.from_s); }));
        fields.appendChild(from);
        fields.appendChild(button('To here', 'ghost', function () { state.to_s = now(); to.textContent = 'to ' + clock(state.to_s); }));
        fields.appendChild(to);
      } else {
        var l = el('label', 'Height ');
        var r = el('input');
        r.type = 'range';
        r.min = '5';
        r.max = '95';
        r.value = '60';
        state.y = 0.6;
        var val = el('span', '60%', 'muted');
        r.addEventListener('input', function () { state.y = parseInt(r.value, 10) / 100; val.textContent = r.value + '%'; });
        l.appendChild(r);
        fields.appendChild(l);
        fields.appendChild(val);
        var only = el('label', ' this part only ');
        var cb = el('input');
        cb.type = 'checkbox';
        cb.addEventListener('change', function () { state.only = cb.checked; });
        only.appendChild(cb);
        fields.appendChild(only);
      }
    }
    if (kinds[cut.id]) kind.value = kinds[cut.id];
    kind.addEventListener('change', function () { kinds[cut.id] = kind.value; render(); });
    render();
    var send = button('Suggest', 'primary', function () {
      var b = { cut: cut.id, kind: state.kind, note: note.value };
      if (state.kind === 'trim') { b.part = state.part; b.head_s = state.head_s; b.tail_s = state.tail_s; }
      if (state.kind === 'strike') { b.from_s = state.from_s; b.to_s = state.to_s; }
      if (state.kind === 'caption_move') { b.y = state.y; if (state.only) b.t = now(); }
      postJson('/api/suggest', b).then(function (r) {
        out.textContent = r.ok ? 'Suggested. It waits for the owner to accept it.' : 'Not sent (' + (r.body.error || r.status) + ').';
        if (r.ok) { flash[cut.id + ':suggest'] = out.textContent; load(); }
      });
    });
    box.appendChild(kind);
    box.appendChild(fields);
    var row = el('div', null, 'row');
    row.appendChild(note);
    row.appendChild(send);
    box.appendChild(row);
    box.appendChild(out);
    return box;
  }

  // ---- two versions, synced -----------------------------------------------------------

  function compare(exports) {
    var box = el('div', null, 'compare');
    box.appendChild(el('h4', 'Compare versions'));
    var pick = { a: exports[1], b: exports[0], mode: 'ab', showing: 'b' };
    var bar = el('div', null, 'row');
    var view = el('div', null, 'pair');
    function label(x) {
      var d = new Date(x.at_ms || 0);
      return d.toLocaleDateString(undefined, { day: 'numeric', month: 'short' }) + ' ' +
        d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' }) + ' · ' + (x.duration_s || 0).toFixed(1) + ' s';
    }
    function versionRow(which) {
      var r = el('div', null, 'row versions');
      r.appendChild(el('span', which.toUpperCase(), 'chip'));
      exports.forEach(function (x) {
        var b = button(label(x), 'ghost small', function () { pick[which] = x; draw(); });
        b.setAttribute('aria-pressed', String(pick[which] === x));
        r.appendChild(b);
      });
      return r;
    }
    function draw() {
      box.textContent = '';
      box.appendChild(el('h4', 'Compare versions'));
      box.appendChild(versionRow('a'));
      box.appendChild(versionRow('b'));
      bar.textContent = '';
      var side = button('Side by side', 'ghost', function () { pick.mode = 'side'; draw(); });
      side.setAttribute('aria-pressed', String(pick.mode === 'side'));
      var ab = button('A/B', 'ghost', function () { pick.mode = 'ab'; draw(); });
      ab.setAttribute('aria-pressed', String(pick.mode === 'ab'));
      bar.appendChild(side);
      bar.appendChild(ab);
      box.appendChild(bar);
      view.textContent = '';
      view.className = 'pair ' + pick.mode;
      var va = video('/x/' + encodeURIComponent(pick.a.id));
      var vb = video('/x/' + encodeURIComponent(pick.b.id));
      va.setAttribute('aria-label', 'Version A');
      vb.setAttribute('aria-label', 'Version B');
      vb.muted = pick.mode === 'side';
      sync(va, vb);
      var pa = player(va, { title: 'Version A', duration: pick.a.duration_s });
      var pb = player(vb, { title: 'Version B', duration: pick.b.duration_s });
      view.appendChild(pa.root);
      view.appendChild(pb.root);
      box.appendChild(view);
      function hidden(x, on) { x.classList.toggle('hidden', on); x.parentNode.classList.toggle('hidden', on); }
      if (pick.mode === 'ab') {
        var lead = pick.showing === 'a' ? va : vb, other = lead === va ? vb : va;
        hidden(other, true);
        other.muted = true;
        var tog = button('Show ' + (pick.showing === 'a' ? 'B' : 'A'), 'primary', function () {
          var from = pick.showing === 'a' ? va : vb, to = from === va ? vb : va;
          var playing = !from.paused;
          to.currentTime = from.currentTime;
          from.pause();
          from.muted = true;
          hidden(from, true);
          hidden(to, false);
          to.muted = false;
          if (playing) to.play();
          pick.showing = pick.showing === 'a' ? 'b' : 'a';
          tog.textContent = 'Show ' + (pick.showing === 'a' ? 'B' : 'A');
        });
        box.appendChild(tog);
      }
    }
    draw();
    return box;
  }

  // keeps b on a's clock (play, pause, seek; drift corrected on every tick)
  function sync(a, b) {
    var lock = false;
    function follow(src, dst) {
      src.addEventListener('play', function () { if (!lock && dst.paused && !dst.classList.contains('hidden')) dst.play(); });
      src.addEventListener('pause', function () { if (!lock && !dst.paused) dst.pause(); });
      src.addEventListener('seeked', function () {
        if (lock) return;
        lock = true;
        dst.currentTime = src.currentTime;
        setTimeout(function () { lock = false; }, 50);
      });
      src.addEventListener('timeupdate', function () {
        if (!lock && !src.paused && Math.abs(dst.currentTime - src.currentTime) > 0.15) dst.currentTime = src.currentTime;
      });
    }
    follow(a, b);
    follow(b, a);
  }

  // ---- review notes -------------------------------------------------------------------

  function reviewForm(cutId, exportId) {
    var box = el('div', null, 'notes');
    var ta = el('textarea');
    ta.maxLength = 2000;
    ta.placeholder = 'Note';
    var sel = document.createElement('select');
    ['keep', 'change', 'drop'].forEach(function (v) {
      var o = el('option', v);
      o.value = v;
      sel.appendChild(o);
    });
    var out = el('p', '', 'muted');
    var send = button('Save note', '', function () {
      var body = { cut: cutId, text: ta.value, verdict: sel.value };
      if (exportId) body.export_ref = exportId;
      post('/api/review', body).then(function (r) {
        out.textContent = r.ok ? 'Saved.' : 'Not saved (' + r.status + ').';
      });
    });
    var fix = button('Fix with agent', '', function () {
      post('/api/fix', { cut: cutId, note: ta.value }).then(function (r) {
        return r.json().then(function (j) {
          out.textContent = j.started ? 'An agent run was started on this cut.' :
            'Queued for an in-app click (' + (j.reasons || []).join('; ') + ').';
        });
      });
    });
    var row = el('div', null, 'row');
    row.appendChild(sel);
    row.appendChild(send);
    row.appendChild(fix);
    box.appendChild(ta);
    box.appendChild(row);
    box.appendChild(out);
    return box;
  }

  function render(home) {
    main.textContent = '';
    role = home.role || 'reviewer';
    say((home.project ? 'Project: ' + home.project : '') + (role === 'editor' ? ' · editor link: comment and suggest edits' : ''));
    var exports = home.exports || [];
    var cuts = home.cuts || [];
    var shown = {};
    var cs = el('section');
    cs.appendChild(el('h2', 'Cuts'));
    // cuts with exports first, newest export first
    var withX = cuts.map(function (c) {
      var xs = exports.filter(function (x) { return x.cut === c.id; })
        .sort(function (a, b) { return (b.at_ms || 0) - (a.at_ms || 0); });
      return { cut: c, xs: xs };
    }).sort(function (a, b) { return (b.xs.length ? b.xs[0].at_ms || 1 : 0) - (a.xs.length ? a.xs[0].at_ms || 1 : 0); });
    withX.forEach(function (c) {
      c.xs.forEach(function (x) { shown[x.id] = true; });
      cs.appendChild(cutCard(c.cut, c.xs));
    });
    main.appendChild(cs);
    // exports whose cut is not listed (another library's)
    var rest = exports.filter(function (x) { return !shown[x.id]; });
    if (rest.length) {
      var ex = el('section');
      ex.appendChild(el('h2', 'Other exports'));
      rest.forEach(function (x) {
        var c = el('div', null, 'card');
        c.appendChild(el('h3', x.title));
        c.appendChild(player(video('/x/' + encodeURIComponent(x.id)), { title: x.title, duration: x.duration_s }).root);
        c.appendChild(el('p', (x.duration_s || 0).toFixed(1) + ' s', 'muted'));
        if (x.blocked && x.blocked.length) c.appendChild(el('p', 'Blocked: ' + x.blocked.join('; '), 'blocked'));
        ex.appendChild(c);
      });
      main.appendChild(ex);
    }
    var ms = el('section');
    ms.appendChild(el('h2', 'Moments'));
    (home.moments || []).forEach(function (m) {
      var c = el('div', null, 'card');
      c.appendChild(el('p', m.quote, 'untrusted'));
      if (m.clip) c.appendChild(player(video('/m/' + encodeURIComponent(m.id))).root);
      ms.appendChild(c);
    });
    main.appendChild(ms);
  }

  function load() {
    return fetch('/api/home', { credentials: 'same-origin' }).then(function (r) {
      if (r.status === 401) { say('This link expired or was revoked. Ask for a new one.'); return; }
      if (!r.ok) { say('Could not load (' + r.status + ').'); return; }
      return r.json().then(function (home) {
        csrf = home.csrf || '';
        // keep what is playing where it is: only re-render when nothing plays
        // (or is fullscreen: a re-render would drop the reviewer out of it)
        var playing = Array.prototype.some.call(document.querySelectorAll('video'), function (v) { return !v.paused; }) ||
          !!document.querySelector('.player.fs');
        if (!playing || !main.firstChild) render(home);
        document.body.setAttribute('data-loads', String(++loads));
      });
    });
  }

  var token = new URLSearchParams(location.hash.slice(1)).get('t');
  if (token) {
    history.replaceState(null, '', location.pathname);
    fetch('/session', {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ token: token })
    }).then(function (r) {
      if (!r.ok) { say('This link is not valid any more.'); return; }
      return load();
    });
  } else {
    load();
  }
})();
