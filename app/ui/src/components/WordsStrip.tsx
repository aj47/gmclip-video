// The words strip (SPEC §5.4, backlog U.06): the cut's words in output order, by part,
// with struck words in the gaps (restorable). Select by click, ⇧-click or drag; the
// selection bar strikes (d/x), restores, re-spells (double-click or f; empty drops the
// word from captions only), toggles keywords (* or ⇧K) and nudges word edges ([ ] start,
// { } end, 10 ms; ⇧ 50 ms). Clicking a word seeks to it. Every action is one command.
//
// Virtualised (R-WORD-1 in the app, a 3 h stream is ~20–40k words): the words are laid
// out in blocks of CHUNK; only the blocks within a screen or two of the strip's view
// hold their words, the others keep their measured height (or an estimate) so the
// scrollbar is true. The playing word is found by binary search, not a walk.
import { createEffect, createMemo, createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { actions, ideaActions, type CutWord } from "../actions";
import { askBroll } from "./Broll";
import { paneShown, reading, setReading, setWordsTall, showTab, wordsTall } from "../layout";
import { findRequest, setFindRequest } from "../transcriptState";
import { attempt, cutView, refreshCut, say, viewer } from "../store";
import { Icon } from "../design/Icon";
import { Button, IconButton, Switch } from "../design/components";
import "./ideas.css";

/** Words per block of the virtualised strip. */
const CHUNK = 240;

export function WordsStrip() {
  let strip!: HTMLDivElement;
  const [data, { refetch }] = createResource(
    () => cutView() && { p: cutView()!.project, c: cutView()!.cut, rev: cutView()!.rev },
    async (k) => (await attempt(() => actions.cutWords(k.p, k.c))) ?? { state: "none", words: [] as CutWord[] },
  );
  const words = () => data()?.words ?? [];
  // the stream edit's topics on these words (any cut that plays them): each word's
  // topic, accepted ones first, and where each run of a topic starts (its chip)
  const [overlay] = createResource(
    () => cutView() && { p: cutView()!.project, c: cutView()!.cut, rev: cutView()!.rev },
    async (k) => (await attempt(() => actions.streamOverlay(k.p, k.c))) ?? null,
  );
  const topicOf = createMemo(() => {
    const ts = (overlay()?.topics ?? []).slice().sort((a, b) => (a.state === "accepted" ? 0 : 1) - (b.state === "accepted" ? 0 : 1));
    return words().map((w) => ts.find((t) => (!t.owner || !w.owner || t.owner === w.owner) && t.w0 <= w.i && w.i <= t.w1));
  });
  const [anchor, setAnchor] = createSignal<number | null>(null); // index into words()
  const [focus, setFocus] = createSignal<number | null>(null);
  const [respelling, setRespelling] = createSignal<number | null>(null);
  let dragFrom: { x: number; i: number } | null = null;

  const sel = createMemo(() => {
    const a = anchor();
    const f = focus();
    if (a === null || f === null) return null;
    return { lo: Math.min(a, f), hi: Math.max(a, f) };
  });
  const selected = createMemo(() => {
    const s = sel();
    return s ? words().slice(s.lo, s.hi + 1) : [];
  });
  // (frame, index) of every word the cut plays, in output order: the playing word is the
  // last one starting at or before the playhead
  const timed = createMemo(() => {
    const out: [number, number][] = [];
    words().forEach((w, i) => {
      if (w.frame !== null) out.push([w.frame, i]);
    });
    return out;
  });
  const playing = createMemo(() => {
    const f = viewer()?.frame;
    const t = timed();
    if (f === undefined || !t.length || t[0]![0] > f) return -1;
    let lo = 0;
    let hi = t.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (t[mid]![0] <= f) lo = mid;
      else hi = mid - 1;
    }
    return t[lo]![1];
  });

  // the blocks: which hold their words now, and how tall each was when it last did
  const chunks = createMemo(() => Array.from({ length: Math.ceil(words().length / CHUNK) }, (_, k) => k));
  const [live, setLive] = createSignal<ReadonlySet<number>>(new Set([0, 1]));
  const heights = new Map<number, number>();
  const estimate = () => {
    if (heights.size) return [...heights.values()].reduce((a, b) => a + b, 0) / heights.size;
    // ~6 px a character at this size, 1.75 lines
    const per = Math.max(1, Math.floor((strip?.clientWidth || 600) / 46));
    return Math.ceil(CHUNK / per) * 23;
  };
  let io: IntersectionObserver | undefined;
  const watch = (el: HTMLElement) => queueMicrotask(() => {
    // (after the strip itself is in place: it is the observer's root)
    io ??= new IntersectionObserver(
      (entries) => {
        const next = new Set(live());
        for (const e of entries) {
          const k = Number((e.target as HTMLElement).dataset.chunk);
          if (e.isIntersecting) next.add(k);
          else {
            if (next.has(k)) heights.set(k, (e.target as HTMLElement).offsetHeight);
            next.delete(k);
          }
        }
        setLive(next);
      },
      { root: strip, rootMargin: "300px 0px" },
    );
    io.observe(el);
  });
  onCleanup(() => io?.disconnect());

  const cut = () => cutView();
  const run = async (f: () => Promise<unknown>) => {
    await attempt(f);
    void refetch();
  };
  // A strike or restore shows on the words at once; the cut's own answer (the re-read
  // after the command) replaces it, and a refused command puts them back.
  const [pending, setPending] = createSignal<ReadonlyMap<number, boolean>>(new Map());
  const struck = (i: number, w: CutWord) => pending().get(i) ?? w.struck;
  const strikeSel = (restore: boolean) => {
    const v = cut();
    const s = selected();
    const at = sel();
    if (!v || !s.length || !at) return;
    // one call per video the selection crosses (each word's ordinal is in its own
    // video's transcript), from the end so earlier ranges see the parts as they were
    const runs: { owner?: string; first: number; last: number }[] = [];
    for (const w of s) {
      const r = runs[runs.length - 1];
      if (r && r.owner === w.owner) r.last = w.i;
      else runs.push({ owner: w.owner, first: w.i, last: w.i });
    }
    setPending(new Map(Array.from({ length: at.hi - at.lo + 1 }, (_, k) => [at.lo + k, !restore] as const)));
    void attempt(async () => {
      for (const r of runs.reverse()) {
        await (restore
          ? actions.restoreWords(v.project, v.cut, r.first, r.last, r.owner)
          : actions.strikeWords(v.project, v.cut, r.first, r.last, r.owner));
      }
    })
      .then(() => refetch())
      .finally(() => setPending(new Map()));
  };
  const keywordSel = () => {
    const v = cut();
    const rev = data()?.revision;
    if (!v || !rev) return;
    for (const w of selected()) void run(() => actions.keyword(v.project, v.cut, w.revision ?? rev, w.i));
  };
  const canRespell = () => data()?.respell === true;
  const nudge = (edge: "start" | "end", ms: number) => {
    if (!canRespell()) return;
    const v = cut();
    const w = selected()[0];
    if (!v || !w || selected().length !== 1) return;
    void run(() => actions.nudgeWord(v.project, v.cut, w.i, edge, ms, w.owner));
  };
  const saveRespell = (i: number, text: string) => {
    const v = cut();
    const rev = data()?.revision;
    const w = words()[i];
    setRespelling(null);
    if (!v || !rev || !w) return;
    void run(() => actions.respell(v.project, v.cut, w.revision ?? rev, w.i, text === w.text ? null : text.trim()));
  };

  // Keys while the strip has focus (the strip renders once the cut has words).
  const onKey = (e: KeyboardEvent) => {
    if ((e.target as HTMLElement).closest("input")) return;
    const big = e.shiftKey ? 50 : 10;
    const k = e.key;
    const stop = () => {
      e.preventDefault();
      e.stopPropagation();
    };
    if (k === "d" || k === "x") { stop(); strikeSel(selected().every((w, j) => struck(sel()!.lo + j, w))); }
    else if (k === "*" || (k === "K" && e.shiftKey)) { stop(); keywordSel(); }
    else if (k === "f") { stop(); if (sel() && canRespell()) setRespelling(sel()!.lo); }
    else if (k === "[") { stop(); nudge("start", -big); }
    else if (k === "]") { stop(); nudge("start", big); }
    else if (k === "{") { stop(); nudge("end", -big); }
    else if (k === "}") { stop(); nudge("end", big); }
    else if (k === "Escape") { setAnchor(null); setFocus(null); }
  };

  const down = (e: PointerEvent, i: number) => {
    if (e.button !== 0) return;
    if (e.shiftKey && anchor() !== null) setFocus(i);
    else {
      setAnchor(i);
      setFocus(i);
    }
    dragFrom = { x: e.clientX, i };
    strip?.focus();
  };
  const enter = (e: PointerEvent, i: number) => {
    if (dragFrom && e.buttons && Math.abs(e.clientX - dragFrom.x) >= 6) setFocus(i);
  };
  const up = (w: CutWord) => {
    const wasClick = dragFrom && anchor() === focus();
    dragFrom = null;
    if (wasClick && w.frame !== null) void attempt(() => actions.viewerSeek(w.frame!));
  };

  // Fix everywhere (K.02): 1–4 caption words spelt right in every caption of the
  // project, added to the term list (and, asked, to every project's)
  const [fixing, setFixing] = createSignal(false);
  const [fixText, setFixText] = createSignal("");
  const [fixAll, setFixAll] = createSignal(false);
  const openFix = () => {
    const ws = selected();
    if (!ws.length || ws.length > 4) return;
    setFixText(ws.map((w) => (w.shown ?? w.text).replace(/[.,?!:;]+$/, "")).join(" "));
    askBroll(null);
    setFixing(true);
  };
  const fixEverywhere = async () => {
    const v = cut();
    const ws = selected();
    const written = fixText().trim();
    if (!v || !ws.length || !written) return;
    const r = await attempt(() => ideaActions.termFixEverywhere(v.project, v.cut, ws[0]!.i, ws[ws.length - 1]!.i, written, fixAll(), ws[0]!.owner));
    if (!r) return;
    setFixing(false);
    say(`“${r.heard}” shows as “${r.written}” in every caption (said ${r.found}×)${r.profile ? " · a term in all your projects" : " · a project term"} — ⌘Z undoes it`);
    void refetch();
    void refreshCut();
  };
  // B-roll (K.05): moments from other recordings that talk about the selected words (the
  // popover is Broll.tsx, shared with the transcript's selection and ⌘K)
  const openBroll = () => {
    const v = cut();
    const ws = selected();
    if (!v || ws.length < 3) return;
    setFixing(false);
    askBroll({ project: v.project, cut: v.cut, lo: ws[0]!.i, hi: ws[ws.length - 1]!.i });
  };

  // Search (⌘F while the strip is the transcript on screen; Enter / ⇧Enter step): a
  // phrase of whole words, the last one a prefix, against what each word shows and what
  // was heard (a re-spelled word is found by either). A step selects the match, brings it
  // into view and seeks to it.
  let searchEl: HTMLInputElement | undefined;
  const [query, setQuery] = createSignal("");
  const [hitAt, setHitAt] = createSignal(0);
  const norm = (t: string) => t.toLowerCase().replace(/[^\p{L}\p{N}'%$.]+/gu, "").replace(/\.+$/, "");
  const hits = createMemo(() => {
    const qw = query().trim().split(/\s+/).map(norm).filter(Boolean);
    const ws = words();
    if (!qw.length) return [] as number[][];
    const forms = ws.map((w) => [norm(w.shown ?? w.text), norm(w.text)]);
    const out: number[][] = [];
    for (let i = 0; i + qw.length <= ws.length; i++) {
      let ok = true;
      for (let j = 0; j < qw.length && ok; j++) {
        const q = qw[j]!;
        ok = forms[i + j]!.some((f) => (j === qw.length - 1 ? f.startsWith(q) : f === q));
      }
      if (ok) out.push(Array.from({ length: qw.length }, (_, j) => i + j));
    }
    return out;
  });
  const hitSet = createMemo(() => new Set(hits().flat()));
  const hitNow = createMemo(() => new Set(hits()[hitAt()] ?? []));
  /** Brings output word `i` into view (its block first: the strip is virtualised). */
  const reveal = (i: number) => {
    const k = Math.floor(i / CHUNK);
    if (!live().has(k)) setLive(new Set([...live(), k]));
    strip?.querySelector(`[data-chunk="${k}"]`)?.scrollIntoView({ block: "nearest" });
    requestAnimationFrame(() => strip?.querySelector(`[data-x="${i}"]`)?.scrollIntoView({ block: "center" }));
  };
  const gotoHit = (k: number) => {
    const h = hits();
    if (!h.length) return;
    const j = ((k % h.length) + h.length) % h.length;
    setHitAt(j);
    const ws = h[j]!;
    setAnchor(ws[0]!);
    setFocus(ws[ws.length - 1]!);
    reveal(ws[0]!);
    const f = words()[ws[0]!]?.frame;
    if (f !== null && f !== undefined) void attempt(() => actions.viewerSeek(f));
  };
  createEffect(() => {
    const q = findRequest();
    if (q === null || !paneShown("words") || reading()) return;
    setFindRequest(null);
    if (q) {
      setQuery(q);
      setHitAt(0);
      queueMicrotask(() => gotoHit(0));
    }
    searchEl?.focus();
    searchEl?.select();
  });
  // Replace (captions only: the match's first word shows the new text, the rest drop out
  // of the captions; the audio is untouched). One call per video the matches are in; a
  // match across a cut (its words not one run of the transcript) is left as it is.
  const [replaceWith, setReplaceWith] = createSignal("");
  const replace = async (all: boolean) => {
    const v = cut();
    const rev0 = data()?.revision;
    const h = all ? hits() : hits()[hitAt()] ? [hits()[hitAt()]!] : [];
    if (!v || !rev0 || !h.length) return;
    const byRev = new Map<string, number[][]>();
    let skipped = 0;
    for (const m of h) {
      const ws = m.map((i) => words()[i]!);
      const rev = ws[0]!.revision ?? rev0;
      const run = ws.every((w, j) => (w.revision ?? rev0) === rev && (j === 0 || w.i === ws[j - 1]!.i + 1));
      if (!run) { skipped++; continue; }
      byRev.set(rev, [...(byRev.get(rev) ?? []), ws.map((w) => w.i)]);
    }
    let n = 0;
    for (const [rev, ords] of byRev) {
      const r = await attempt(() => actions.captionReplace(v.project, v.cut, rev, ords, replaceWith()));
      if (r) n += r.matches;
    }
    void refetch();
    void refreshCut();
    const to = replaceWith().trim();
    say(`${n} match${n === 1 ? "" : "es"} ${to ? `show “${to}” in the captions` : "back to what was heard"}${skipped ? ` · ${skipped} across a cut left as they are` : ""} — ⌘Z undoes it`);
    if (!all) gotoHit(hitAt() + 1);
  };

  const seconds = () => selected().filter((w) => !w.struck).reduce((a, w) => a + w.dur_ms, 0) / 1000;
  const partsCrossed = () => new Set(selected().map((w) => w.part)).size;

  return (
    <Show when={data()?.state === "ok" && words().length}>
    <section class="words" aria-label="Words" data-tall={String(wordsTall())}>
      <div class="words-bar">
        <span class="lbl" data-tip="Click a word to seek; drag or ⇧-click to select; d strikes, f re-spells, * keywords">Words</span>
        <input
          ref={searchEl}
          type="search"
          class="words-search"
          placeholder="Search  ⌘F"
          aria-label="Search the words"
          data-testid="words-search"
          value={query()}
          onInput={(e) => { setQuery(e.currentTarget.value); setHitAt(0); queueMicrotask(() => gotoHit(0)); }}
          onKeyDown={(e) => {
            if (e.key === "Enter") { e.preventDefault(); gotoHit(hitAt() + (e.shiftKey ? -1 : 1)); }
            if (e.key === "Escape") { e.preventDefault(); setQuery(""); strip?.focus(); }
            e.stopPropagation();
          }}
        />
        <Show when={query().trim()}>
          <span class="words-hits small" data-testid="words-hits">{hits().length ? `${hitAt() + 1} / ${hits().length}` : "no matches"}</span>
        </Show>
        <Show when={selected().length}>
          <span class="selbar" role="toolbar" aria-label="Selection">
            <span class="sel-sum">{selected().length} words · {seconds().toFixed(1)} s<span class="sel-parts"> · {partsCrossed()} part{partsCrossed() === 1 ? "" : "s"}</span></span>
            <button type="button" class="ghost small" onClick={() => strikeSel(false)} data-tip="Strike the words" data-kbd="D"><Icon name="strike" size={13} /><span class="sel-label">Strike</span></button>
            <button type="button" class="ghost small" onClick={() => strikeSel(true)} data-tip="Put the words back"><Icon name="restore" size={13} /><span class="sel-label">Restore</span></button>
            <button
              type="button"
              class="ghost small"
              aria-label="✎ Re-spell"
              disabled={selected().length !== 1 || !canRespell()}
              onClick={() => setRespelling(sel()!.lo)}
              data-tip={canRespell() ? "Re-spell in the captions ([ ] { } tune the edges ±10 ms)" : data()?.respell_why}
              data-kbd={canRespell() ? "F" : undefined}
            >
              <Icon name="pencil" size={13} /><span class="sel-label">Re-spell</span>
            </button>
            <button type="button" class="ghost small" onClick={keywordSel} data-tip="Keyword on / off" data-kbd="*"><Icon name="star" size={13} /><span class="sel-label">Keyword</span></button>
            <button
              type="button"
              class="ghost small"
              data-testid="fix-everywhere"
              disabled={selected().length > 4}
              onClick={openFix}
              data-tip="A name the transcriber got wrong: spell it right in every caption of the project and add it to your terms"
            >
              <Icon name="pencil" size={13} /><span class="sel-label">Fix everywhere</span>
            </button>
            <button
              type="button"
              class="ghost small"
              data-testid="broll-open"
              disabled={selected().length < 3}
              onClick={openBroll}
              data-tip="Moments from your other recordings that talk about these words"
            >
              <Icon name="film" size={13} /><span class="sel-label">B-roll</span>
            </button>
          </span>
        </Show>
        {/* bigger views of the words: a tall strip, the side panel's Words tab, reading */}
        <span class="words-views" role="toolbar" aria-label="Words view">
          <button type="button" class="ghost small" data-testid="words-tall" aria-pressed={wordsTall()} onClick={() => setWordsTall(!wordsTall())} data-tip={wordsTall() ? "Back to a short strip" : "Show more lines of words"}>
            <Icon name={wordsTall() ? "chevron-down" : "chevron-up"} size={13} /><span class="sel-label">{wordsTall() ? "Shorter" : "Taller"}</span>
          </button>
          <button type="button" class="ghost small" data-testid="words-side" onClick={() => showTab("words")} data-tip="The words in the side panel, beside the viewer (drag its edge to widen)">
            <Icon name="panel-right" size={13} /><span class="sel-label">Side</span>
          </button>
          <button type="button" class="ghost small" data-testid="words-read" onClick={() => setReading(true)} data-tip="Reading: the whole transcript large, beside a smaller viewer">
            <Icon name="book-open" size={13} /><span class="sel-label">Read</span>
          </button>
        </span>
      </div>
      <Show when={query().trim()}>
        <div class="words-find" role="toolbar" aria-label="Find and replace">
          <button type="button" class="ghost small" disabled={!hits().length} onClick={() => gotoHit(hitAt() - 1)} data-tip="Previous match" data-kbd="⇧↵"><Icon name="chevron-up" size={13} /></button>
          <button type="button" class="ghost small" disabled={!hits().length} onClick={() => gotoHit(hitAt() + 1)} data-tip="Next match" data-kbd="↵"><Icon name="chevron-down" size={13} /></button>
          <input
            type="text"
            class="words-replace"
            placeholder="Replace with…"
            aria-label="Replace in captions with"
            data-testid="words-replace"
            value={replaceWith()}
            onInput={(e) => setReplaceWith(e.currentTarget.value)}
            onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); void replace(e.metaKey || e.ctrlKey); } e.stopPropagation(); }}
          />
          <button type="button" class="ghost small" data-testid="words-replace-one" disabled={!hits().length} onClick={() => void replace(false)} data-tip="Show this match as the new text in the captions, then go to the next (audio untouched)">Replace</button>
          <button type="button" class="ghost small" data-testid="words-replace-all" disabled={!hits().length} onClick={() => void replace(true)} data-tip="Every match, in the captions (audio untouched)" data-kbd="⌘↵">All</button>
          <span class="muted small">captions only · audio untouched</span>
          <IconButton icon="x" size="sm" label="Close search" onClick={() => { setQuery(""); strip?.focus(); }} />
        </div>
      </Show>
      <Show when={fixing() && selected().length}>
        <form class="fix-pop" data-testid="fix-pop" onSubmit={(e) => { e.preventDefault(); void fixEverywhere(); }} onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); setFixing(false); } }}>
          <span class="small">
            Heard <b>{selected().map((w) => w.text).join(" ")}</b> — show it as
          </span>
          <div class="row">
            <input type="text" aria-label="Show as" data-testid="fix-written" value={fixText()} onInput={(e) => setFixText(e.currentTarget.value)} ref={(el) => queueMicrotask(() => el.select())} />
            <Button type="submit" size="sm" variant="primary" data-testid="fix-apply" disabled={!fixText().trim()}>Fix everywhere</Button>
            <IconButton icon="x" size="sm" label="Close" onClick={() => setFixing(false)} />
          </div>
          <Switch testid="fix-all" on={fixAll()} onChange={setFixAll} label={<>In all my projects</>} hint="Every new transcription spells it this way, and the name check before export knows it" />
        </form>
      </Show>
      <Show when={data()?.state === "ok"}>
        <div class="strip" ref={strip} tabindex="0" onKeyDown={onKey} role="listbox" aria-label="Cut words" aria-multiselectable="true" data-testid="words" data-words={words().length}>
          <For each={chunks()}>
            {(k) => (
              <div class="wchunk" data-chunk={k} ref={watch} style={live().has(k) ? undefined : { height: `${heights.get(k) ?? estimate()}px` }}>
              <Show when={live().has(k)}>
              <For each={words().slice(k * CHUNK, (k + 1) * CHUNK)}>
            {(w, j) => {
              const i = () => k * CHUNK + j();
              return (
              <>
                <Show when={i() === 0 || words()[i() - 1]!.part !== w.part}>
                  <span class="part-chip" aria-hidden="true">{w.part}</span>
                </Show>
                <Show when={topicOf()[i()] && topicOf()[i()] !== topicOf()[i() - 1]}>
                  <span classList={{ "topic-chip": true, proposed: topicOf()[i()]!.state === "proposed" }} style={{ "--topic": topicOf()[i()]!.colour }} aria-hidden="true" data-testid="words-topic">{topicOf()[i()]!.title}</span>
                </Show>
                <Show
                  when={respelling() !== i()}
                  fallback={
                    <input
                      class="respell"
                      aria-label="Re-spell"
                      value={w.shown ?? w.text}
                      ref={(el) => queueMicrotask(() => el.select())}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") saveRespell(i(), e.currentTarget.value);
                        if (e.key === "Escape") setRespelling(null);
                        e.stopPropagation();
                      }}
                      onBlur={(e) => saveRespell(i(), e.currentTarget.value)}
                    />
                  }
                >
                  <span
                    role="option"
                    aria-selected={!!sel() && i() >= sel()!.lo && i() <= sel()!.hi}
                    classList={{
                      w: true,
                      tp: !!topicOf()[i()],
                      "tp-proposed": topicOf()[i()]?.state === "proposed",
                      struck: struck(i(), w),
                      kw: w.keyword,
                      edited: w.edited,
                      dropped: w.shown === null,
                      sel: !!sel() && i() >= sel()!.lo && i() <= sel()!.hi,
                      now: playing() === i(),
                      hit: hitSet().has(i()),
                      "hit-now": hitNow().has(i()),
                    }}
                    style={topicOf()[i()] ? { "--topic": topicOf()[i()]!.colour } : undefined}
                    data-x={i()}
                    title={w.edited ? `heard “${w.text}”` : undefined}
                    data-part={w.part}
                    data-i={w.i}
                    onPointerDown={(e) => down(e, i())}
                    onPointerEnter={(e) => enter(e, i())}
                    onPointerUp={() => up(w)}
                    onDblClick={() => canRespell() && setRespelling(i())}
                  >
                    {w.shown ?? w.text}
                  </span>
                </Show>
              </>
              );
            }}
              </For>
              </Show>
              </div>
            )}
          </For>
        </div>
      </Show>
    </section>
    </Show>
  );
}
