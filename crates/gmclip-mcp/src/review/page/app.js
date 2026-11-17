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
    v.controls = true;
    v.preload = 'metadata';
    v.playsInline = true;
    v.src = src;
    return v;
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
    var stage = el('div', null, 'stage');
    var v = null, pins = null, markers = null;
    var latest = exports[0];
    if (latest) {
      v = video('/x/' + encodeURIComponent(latest.id));
      pins = el('div', null, 'pins');
      stage.appendChild(v);
      stage.appendChild(pins);
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
      var input = el('input');
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
      view.appendChild(va);
      view.appendChild(vb);
      box.appendChild(view);
      if (pick.mode === 'ab') {
        var lead = pick.showing === 'a' ? va : vb, other = lead === va ? vb : va;
        other.classList.add('hidden');
        other.muted = true;
        var tog = button('Show ' + (pick.showing === 'a' ? 'B' : 'A'), 'primary', function () {
          var from = pick.showing === 'a' ? va : vb, to = from === va ? vb : va;
          var playing = !from.paused;
          to.currentTime = from.currentTime;
          from.pause();
          from.muted = true;
          from.classList.add('hidden');
          to.classList.remove('hidden');
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
        c.appendChild(video('/x/' + encodeURIComponent(x.id)));
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
      if (m.clip) c.appendChild(video('/m/' + encodeURIComponent(m.id)));
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
        var playing = Array.prototype.some.call(document.querySelectorAll('video'), function (v) { return !v.paused; });
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
