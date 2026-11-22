// The transcript pane (AJ 2026-10-14): the cut's source, every word in source order.
// Words the cut does not play are greyed and struck through. Click a word to seek (words
// in the cut); drag or ⇧-click to select; Delete/Backspace cuts the selection out of the
// cut (core `words_strike`: a real part split/trim, one ⌘Z), and on struck words it puts
// them back (`words_restore`). The playhead's word is highlighted and followed while
// playing. ⌘F searches; Enter / ⇧Enter step through matches. Words arrive while the
// video is still being transcribed.
//
// A stream can have 30 000 words, so the per-word classes that change often (selection,
// playhead, search hits) are toggled on the elements directly; the rendered lines are
// cached by content so an edit re-renders only the lines it touched.
//
// Reading (AJ 2026-10-16: "view the transcript (without times) in full"): the same pane,
// wide beside a smaller viewer, as prose — no times, paragraphs at pauses and topic
// edges, large type, the whole source or only what the cut plays. Selection, search,
// click-to-seek, Cut / Topic / Keyword all work there as in the panel. The panel's times
// can be turned off on their own (the clock button, View › Times in the transcript).
import { thisMachine } from "../platform";
import { about } from "../eta";
import { createEffect, createMemo, createResource, createSignal, For, on, onCleanup, Show } from "solid-js";
import { Button, IconButton, SegmentedControl, Switch } from "../design/components";
import { Icon } from "../design/Icon";
import { askBroll } from "./Broll";
import { paneShown, reading, setReading } from "../layout";
import { prefs, setPref } from "../prefs";
import { actions, ideaActions, type CoverageRow, type FluffRow, type TopicBand, type TranscriptData } from "../actions";
import { analysisTick, attempt, cutView, modelsProgress, openCut, refreshCut, say, viewer } from "../store";
import { applyPending, labelAsk, noteTitle, overlay, overlayAll, pendingTopic, recentTitles, refreshOverlay, setLabelAsk, setOverlay, setPendingTopic, topicAt } from "../streamState";
import { EDGE_STEPS, edgeStep, trFollow, findRequest, respelling, setEdgeStep, setFindRequest, setRespelling, setTrAnchor, setTrData, setTrFocus, setTrHover, setTrOwner, setTuning, strikeOrRestore, trAnchor, trData, trFocus, trOwner, trSel, tuning, wordAtFrame } from "../transcriptState";

interface LineRow {
  key: string;
  a: number;
  b: number;
  t: string;
  /** A pause of PARA_GAP_MS or more before it: reading starts a paragraph here. */
  gap: boolean;
}

interface Para {
  key: string;
  lines: LineRow[];
  /** Every word in it is cut ("only the cut" hides it). */
  out: boolean;
}

/** A pause this long starts a paragraph in the reading view. */
const PARA_GAP_MS = 1200;
/** A paragraph this many words long ends at the next full stop. */
const PARA_WORDS = 90;

function clock(ms: number): string {
  const t = Math.floor(ms / 1000);
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const s = String(t % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

function norm(s: string): string {
  return s.toLowerCase().replace(/[^\p{L}\p{N}' ]+/gu, "");
}

export function Transcript() {
  let pane!: HTMLDivElement;
  let search!: HTMLInputElement;
  const els: HTMLElement[] = [];
  const key = () => {
    const v = cutView();
    return v ? { p: v.project, c: v.cut, rev: v.rev, tick: analysisTick(), o: trOwner() } : undefined;
  };
  const [data] = createResource(key, async (k) => (await attempt(() => actions.transcript(k.p, k.c, k.o))) ?? null);
  // Keep the last good data while a refetch runs (no flash of "loading"); shared with the
  // global keys. A different cut (or video) starts empty.
  createEffect(on(() => cutView()?.cut, () => setTrOwner(null)));
  createEffect(on(() => [cutView()?.cut, trOwner()], () => { setTrData(null); setTrAnchor(null); setTrFocus(null); }));
  // Another video's words (the source picker): the stream edit's topics and fluff are on
  // its own video's words, so they show only there.
  const [ownOwner, setOwnOwner] = createSignal<string | null>(null);
  createEffect(() => {
    const t = data();
    if (t && trOwner() === null) setOwnOwner(t.owner ?? null);
  });
  // a video this cut does not play (the main cut plays several: picking one of its own
  // videos is not browsing, its topics and fluff show there; decision 0036)
  const browsing = () => {
    const t = trData();
    const v = t?.videos?.find((x) => x.owner === t.owner);
    return trOwner() !== null && !v?.in_cut;
  };
  const owner = () => trData()?.owner ?? null;
  createEffect(() => {
    const d = data();
    if (d) setTrData(d);
  });
  onCleanup(() => setTrHover(null));
  // Decision 0011: topics, fluff proposals and coverage chips on the stream edit.
  createEffect(on(() => cutView()?.cut, () => setOverlay(null)));
  createEffect(on(() => [cutView()?.cut, cutView()?.rev, analysisTick()], () => void refreshOverlay()));
  const isStream = () => cutView()?.kind === "stream" && !browsing();
  // a topic of another of the cut's videos: show that video's words first
  createEffect(on([pendingTopic, overlayAll], ([id, o]) => {
    const tp = id ? o?.topics.find((x) => x.id === id) : undefined;
    const cur = trData()?.owner;
    if (tp?.owner && cur && tp.owner !== cur) setTrOwner(tp.owner === ownOwner() ? null : tp.owner);
  }));
  createEffect(on([pendingTopic, overlay, trData], ([id, o]) => {
    const tp = id ? o?.topics.find((x) => x.id === id) : undefined;
    if (!tp || !trData()?.text.length) return;
    setPendingTopic(null);
    setTrAnchor(tp.w0);
    setTrFocus(tp.w1);
    queueMicrotask(() => document.querySelector(`[data-topic="${tp.id}"]`)?.scrollIntoView({ block: "start" }));
  }));
  createEffect(on([trData, () => cutView()?.cut], () => {
    if (applyPending()) queueMicrotask(() => els[trAnchor() ?? 0]?.scrollIntoView({ block: "center" }));
  }));
  const d = (): TranscriptData | null => trData();
  const n = () => d()?.text.length ?? 0;

  // Lines, cached by content (text range + in/out mask + keywords) across fetches.
  let cache = new Map<string, LineRow>();
  const lines = createMemo<LineRow[]>(() => {
    const t = d();
    if (!t) return [];
    const kw = new Set(t.keywords);
    const next = new Map<string, LineRow>();
    const out: LineRow[] = [];
    for (const [a, b] of t.lines) {
      let mask = "";
      for (let i = a; i < b; i++) {
        const e = t.edits?.[String(i)];
        mask += (t.frame[i]! < 0 ? "0" : "1") + (kw.has(i) ? "k" : "") + (e !== undefined ? `e${e}|` : "") + (t.tuned?.[String(i)] ? "t" : "");
      }
      const prevEnd = a > 0 ? (t.start_ms[a - 1] ?? 0) + (t.dur_ms[a - 1] ?? 0) : -Infinity;
      const gap = (t.start_ms[a] ?? 0) - prevEnd >= PARA_GAP_MS;
      const k = `${t.revision}:${a}-${b}:${mask}${gap ? ":p" : ""}`;
      const row = cache.get(k) ?? { key: k, a, b, t: clock(t.start_ms[a] ?? 0), gap };
      next.set(k, row);
      out.push(row);
    }
    cache = next;
    return out;
  });

  // Topic headers, fluff rows and coverage chips by the line they start in; struck runs
  // over 30 s collapsed (the stream edit stays readable). Keyed by line start so the
  // cached line rows are kept.
  const lineOf = (i: number) => {
    const ls = lines();
    let lo = 0;
    let hi = ls.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (ls[mid]!.a <= i) lo = mid;
      else hi = mid - 1;
    }
    return ls[lo]?.a ?? 0;
  };
  const startsAt = <T extends { w0: number }>(xs: T[]) => {
    const m = new Map<number, T[]>();
    for (const x of xs) {
      const a = lineOf(x.w0);
      m.set(a, [...(m.get(a) ?? []), x]);
    }
    return m;
  };
  const topicHeads = createMemo(() => startsAt<TopicBand>(!browsing() ? (overlay()?.topics ?? []) : []));
  const fluffHeads = createMemo(() => startsAt<FluffRow>(isStream() ? (overlay()?.fluff ?? []) : []));
  // a cut that uses these words: its chip once per topic (not on every paragraph of it)
  const chips = createMemo(() => {
    const seen = new Set<string>();
    const xs: CoverageRow[] = [];
    for (const c of overlay()?.coverage ?? []) {
      const k = `${c.cut}:${topicAt(c.w0)?.id ?? `line${lineOf(c.w0)}`}`;
      if (c.kind === "stream" || seen.has(k)) continue;
      seen.add(k);
      xs.push(c);
    }
    return startsAt(xs);
  });
  const [expanded, setExpanded] = createSignal<Set<number>>(new Set());
  const collapse = createMemo(() => {
    const t = d();
    const heads = new Map<number, { ms: number; lines: number }>();
    const hidden = new Set<number>();
    if (!t || !isStream()) return { heads, hidden };
    const ls = lines();
    const struck = (a: number, b: number) => {
      for (let i = a; i < b; i++) if (t.frame[i]! >= 0) return false;
      return true;
    };
    for (let k = 0; k < ls.length; ) {
      if (!struck(ls[k]!.a, ls[k]!.b)) {
        k++;
        continue;
      }
      let j = k;
      while (j + 1 < ls.length && struck(ls[j + 1]!.a, ls[j + 1]!.b)) j++;
      const a = ls[k]!.a;
      const last = ls[j]!.b - 1;
      const ms = t.start_ms[last]! + t.dur_ms[last]! - t.start_ms[a]!;
      if (ms > 30_000 && !expanded().has(a)) {
        heads.set(a, { ms, lines: j - k + 1 });
        for (let x = k; x <= j; x++) hidden.add(ls[x]!.a);
      }
      k = j + 1;
    }
    return { heads, hidden };
  });
  // Retakes (decision 0040): the first try of something said again just after, as the
  // retakes sweep finds it (its output-frame ranges, mapped onto the words). The panel
  // marks them now the lines run together as prose (a line break no longer shows them).
  const [retakes] = createResource(
    () => { const v = cutView(); const t = d(); return v && t?.revision && !browsing() ? { p: v.project, c: v.cut, r: v.rev, rev: t.revision } : undefined; },
    async (k) => (await actions.sweepPreview(k.p, k.c, "retakes", 0.4, 0.1).catch(() => null))?.cuts ?? [],
  );
  const againWords = createMemo(() => {
    const t = d();
    const set = new Set<number>();
    const rs = (retakes() ?? []).filter((x) => x.out);
    if (!t || !rs.length) return set;
    for (let i = 0; i < t.frame.length; i++) {
      const f = t.frame[i]!;
      if (f >= 0 && rs.some((x) => f >= x.out![0] && f < x.out![1])) set.add(i);
    }
    return set;
  });
  // Paragraphs (the reading view's; in the panel a paragraph is invisible, `display:
  // contents`): a new one after a pause, at a topic, fluff or collapsed-run edge, and at a
  // full stop once one runs long. Cached by their lines' keys like the lines.
  let paraCache = new Map<string, Para>();
  const paras = createMemo<Para[]>(() => {
    const t = d();
    const ls = lines();
    if (!t) return [];
    const th = topicHeads();
    const fh = fluffHeads();
    const ch = collapse();
    const next = new Map<string, Para>();
    const out: Para[] = [];
    let cur: LineRow[] = [];
    let words = 0;
    const flush = () => {
      if (!cur.length) return;
      const key = cur.map((l) => l.key).join("|");
      let allOut = true;
      for (const l of cur) for (let i = l.a; i < l.b && allOut; i++) if (t.frame[i]! >= 0) allOut = false;
      const p = paraCache.get(key) ?? { key, lines: cur, out: allOut };
      next.set(key, p);
      out.push(p);
      cur = [];
      words = 0;
    };
    for (const l of ls) {
      const edge = th.has(l.a) || fh.has(l.a) || ch.heads.has(l.a) || ch.hidden.has(l.a) !== (cur.length ? ch.hidden.has(cur[cur.length - 1]!.a) : false);
      const prev = cur[cur.length - 1];
      const long = words >= PARA_WORDS && !!prev && /[.?!…]["”’)]*$/.test(t.text[prev.b - 1] ?? "");
      if (l.gap || edge || long) flush();
      cur.push(l);
      words += l.b - l.a;
    }
    flush();
    paraCache = next;
    return out;
  });
  // (the per-word classes below repaint when paragraphs re-render: a topic arriving can
  // regroup them, which makes their words new elements)
  // fluff proposals: their words dashed
  createEffect(on([() => overlay()?.fluff, paras], ([f], prev) => {
    const words = (xs: FluffRow[] | undefined) => (xs ?? []).flatMap((x) => range({ lo: x.w0, hi: x.w1 }));
    paint("fl", words(prev ? (prev[0] as FluffRow[] | undefined) : undefined), words(isStream() ? f : undefined));
  }));
  // the label popover (`t`) and edge drags
  const [labelText, setLabelText] = createSignal("");
  let labelInput: HTMLInputElement | undefined;
  createEffect(on(labelAsk, (a) => {
    if (!a) return;
    setLabelText(recentTitles()[0] ?? "");
    queueMicrotask(() => { labelInput?.focus(); labelInput?.select(); });
  }));
  const applyLabel = async () => {
    const v = cutView();
    const a = labelAsk();
    const title = labelText().trim();
    if (!v || !a || !title) return;
    setLabelAsk(null);
    const r = await attempt(() => actions.topicLabel(v.project, v.cut, a.lo, a.hi, title, trData()?.owner ?? undefined));
    if (r) {
      noteTitle(title);
      say(`Topic “${title}”${r.trimmed.length ? ` — ${r.trimmed.length} topic${r.trimmed.length === 1 ? "" : "s"} re-labelled` : ""} · ⌘Z undoes`);
      setAnchor(null);
      setFocus(null);
    }
    pane?.focus({ preventScroll: true });
  };
  let edgeDrag: { topic: string; edge: "start" | "end" } | null = null;
  const dropDrag = () => setTimeout(() => { edgeDrag = null; document.body.classList.remove("edge-dragging"); }, 0);
  addEventListener("pointerup", dropDrag);
  onCleanup(() => removeEventListener("pointerup", dropDrag));
  const decideTopic = (t: TopicBand, state: "accepted" | "rejected") => {
    const v = cutView();
    if (v) void attempt(() => actions.topicState(v.project, t.id, state));
  };
  const decideFluff = (f: FluffRow, state: "accepted" | "rejected") => {
    const v = cutView();
    if (v) void attempt(() => actions.fluffState(v.project, [f.id], state));
  };
  const clockMs = (x: number) => clock(x);
  const dur = (ms: number) => {
    const t = Math.round(ms / 1000);
    return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, "0")}`;
  };

  // Selection (word indices), playhead word, search hits: toggled on elements.
  const anchor = trAnchor;
  const focus = trFocus;
  const setAnchor = setTrAnchor;
  const setFocus = setTrFocus;
  const sel = createMemo(trSel);
  const paint = (cls: string, from: Iterable<number>, to: Iterable<number>) => {
    for (const i of from) els[i]?.classList.remove(cls);
    for (const i of to) els[i]?.classList.add(cls);
  };
  const range = (s: { lo: number; hi: number } | null) => {
    const r: number[] = [];
    if (s) for (let i = s.lo; i <= s.hi; i++) r.push(i);
    return r;
  };
  createEffect(on([sel, paras], ([s], prev) => {
    const was = range(prev ? (prev[0] as { lo: number; hi: number } | null) : null);
    const now = range(s);
    paint("sel", was, now);
    for (const i of was) els[i]?.setAttribute("aria-selected", "false");
    for (const i of now) els[i]?.setAttribute("aria-selected", "true");
  }));

  // The word at the playhead: the in-cut word whose frame is the last ≤ the viewer's.
  const byFrame = createMemo(() => {
    const t = d();
    if (!t) return [] as [number, number][];
    const r: [number, number][] = [];
    t.frame.forEach((f, i) => {
      if (f >= 0) r.push([f, i]);
    });
    return r.sort((x, y) => x[0] - y[0]);
  });
  const nowWord = createMemo(() => {
    const f = viewer()?.frame;
    const bf = byFrame();
    if (f === undefined || !bf.length) return -1;
    let lo = 0;
    let hi = bf.length - 1;
    if (bf[0]![0] > f) return -1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (bf[mid]![0] <= f) lo = mid;
      else hi = mid - 1;
    }
    return bf[lo]![1];
  });
  // ⌖ follow the playhead: the playhead's word stays in view while playing and stepping
  // following the playhead: on, ⌘K "Words: follow the playhead" turns it off (its ⌖ button
  // left the bar 2026-11-22)
  const follow = trFollow;
  createEffect(on([nowWord, paras, follow], ([w, , f], prev) => {
    const was = prev ? (prev[0] as number) : -1;
    paint("now", was >= 0 ? [was] : [], w >= 0 ? [w] : []);
    if (w >= 0 && f && (w !== was || (prev && prev[2] === false))) els[w]?.scrollIntoView({ block: "nearest" });
  }));

  // Search.
  const [query, setQuery] = createSignal("");
  const [hitAt, setHitAt] = createSignal(0);
  const hits = createMemo(() => {
    const q = norm(query()).trim();
    const t = d();
    if (!q || !t) return [] as number[][];
    const qw = q.split(/\s+/);
    const words = t.text.map(norm);
    const out: number[][] = [];
    for (let i = 0; i + qw.length <= words.length; i++) {
      let ok = true;
      for (let j = 0; j < qw.length && ok; j++) {
        const w = words[i + j]!;
        ok = j === qw.length - 1 ? w.startsWith(qw[j]!) : w === qw[j];
      }
      if (ok) out.push(Array.from({ length: qw.length }, (_, j) => i + j));
    }
    return out;
  });
  createEffect(on([hits, paras], ([h], prev) => {
    paint("hit", prev ? (prev[0] as number[][]).flat() : [], h.flat());
  }));
  const gotoHit = (k: number) => {
    const h = hits();
    if (!h.length) return;
    const j = ((k % h.length) + h.length) % h.length;
    setHitAt(j);
    const ws = h[j]!;
    setAnchor(ws[0]!);
    setFocus(ws[ws.length - 1]!);
    els[ws[0]!]?.scrollIntoView({ block: "center" });
  };

  // Find & replace (captions only: a re-spell of the matched words; audio untouched).
  const [replaceWith, setReplaceWith] = createSignal("");
  const replace = async (all: boolean) => {
    const v = cutView();
    const rev = d()?.revision;
    const h = hits();
    if (!v || !rev || !h.length) return;
    const which = all ? h : [h[hitAt()]!];
    const r = await attempt(() => actions.captionReplace(v.project, v.cut, rev, which, replaceWith()));
    if (r) say(`${which.length} match${which.length === 1 ? "" : "es"} re-spelled in the captions`);
    if (!all) gotoHit(hitAt() + 1);
  };

  // Editing.
  const selected = () => range(sel());
  const allStruck = () => {
    const t = d();
    const s = selected();
    return !!t && s.length > 0 && s.every((i) => t.frame[i]! < 0);
  };
  const cutSel = async () => {
    const s = sel();
    if (s) await strikeOrRestore(s.lo, s.hi);
  };
  // Fix everywhere (K.02): 1–4 caption words spelt right in every caption of the project,
  // added to the term list (and, asked, to every project's)
  const [fixing, setFixing] = createSignal(false);
  const [fixText, setFixText] = createSignal("");
  const [fixAll, setFixAll] = createSignal(false);
  const openFix = () => {
    const x = sel();
    const t = d();
    if (!x || !t || x.hi - x.lo > 3) return;
    setFixText(t.text.slice(x.lo, x.hi + 1).map((w) => w.replace(/[.,?!:;]+$/, "")).join(" "));
    setFixing(true);
  };
  const fixEverywhere = async () => {
    const v = cutView();
    const x = sel();
    const written = fixText().trim();
    if (!v || !x || !written) return;
    const r = await attempt(() => ideaActions.termFixEverywhere(v.project, v.cut, x.lo, x.hi, written, fixAll(), owner() ?? undefined));
    if (!r) return;
    setFixing(false);
    say(`“${r.heard}” shows as “${r.written}” in every caption (said ${r.found}×)${r.profile ? " · a term in all your projects" : " · a project term"} — ⌘Z undoes it`);
    void refreshCut();
  };
  const keywordSel = () => {
    const v = cutView();
    const rev = d()?.revision;
    if (!v || !rev) return;
    for (const i of selected()) void attempt(() => actions.keyword(v.project, v.cut, rev, i));
  };

  let drag: { x: number; i: number } | null = null;
  const down = (e: PointerEvent, i: number) => {
    if (e.button !== 0) return;
    if (edgeDrag) return;
    if (e.altKey) {
      // ⌥-click: the whole line (paragraph); ⇧⌥-click extends to it
      const l = lines().find((x) => x.a <= i && i < x.b);
      if (l && e.shiftKey && anchor() !== null) {
        const lo = Math.min(anchor()!, focus() ?? anchor()!);
        const hi = Math.max(anchor()!, focus() ?? anchor()!);
        if (l.a < lo) {
          setAnchor(hi);
          setFocus(l.a);
        } else {
          setAnchor(lo);
          setFocus(Math.max(hi, l.b - 1));
        }
      } else if (l) {
        setAnchor(l.a);
        setFocus(l.b - 1);
      }
      pane.focus({ preventScroll: true });
      return;
    }
    if (e.shiftKey && anchor() !== null) setFocus(i);
    else {
      setAnchor(i);
      setFocus(i);
    }
    drag = { x: e.clientX, i };
    pane.focus({ preventScroll: true });
  };
  const enter = (e: PointerEvent, i: number) => {
    if (drag && e.buttons && Math.abs(e.clientX - drag.x) >= 4) setFocus(i);
  };
  const up = (i0?: number) => {
    if (edgeDrag && i0 !== undefined) {
      const g = edgeDrag;
      edgeDrag = null;
      document.body.classList.remove("edge-dragging");
      const v = cutView();
      if (v) void attempt(() => actions.topicEdge(v.project, v.cut, g.topic, g.edge, i0));
      return;
    }
    // The word pressed (not the one released over: the layout may have moved).
    const click = drag && anchor() === focus();
    const i = drag?.i ?? -1;
    drag = null;
    const f = d()?.frame[i] ?? -1;
    if (click && f >= 0) void attempt(() => actions.viewerSeek(f));
  };

  /** The arrows in the focused words: ← → a word, ↑ ↓ a line (same place in it), ⇧
   * extends the selection. The first press with nothing selected picks the word at the
   * playhead; a plain move shows that word in the viewer. */
  const arrow = (key: string, extend: boolean) => {
    const t = d();
    if (!t || !t.text.length) return;
    const last = t.text.length - 1;
    const cur = focus() ?? anchor();
    let next: number;
    if (cur === null) {
      const w = wordAtFrame(viewer()?.frame ?? 0);
      next = w >= 0 ? w : 0;
    } else if (key === "ArrowLeft" || key === "ArrowRight") {
      next = Math.max(0, Math.min(last, cur + (key === "ArrowLeft" ? -1 : 1)));
    } else {
      const ls = lines();
      let li = 0;
      for (let lo = 0, hi = ls.length - 1; lo <= hi; ) {
        const mid = (lo + hi) >> 1;
        if (ls[mid]!.a <= cur) {
          li = mid;
          lo = mid + 1;
        } else hi = mid - 1;
      }
      const to = ls[Math.max(0, Math.min(ls.length - 1, li + (key === "ArrowUp" ? -1 : 1)))];
      const at = ls[li];
      next = to && at ? Math.min(to.b - 1, to.a + (cur - at.a)) : cur;
    }
    if (extend && cur !== null) {
      if (anchor() === null) setAnchor(cur);
      setFocus(next);
    } else {
      setAnchor(next);
      setFocus(next);
      const f = t.frame[next] ?? -1;
      if (f >= 0) void attempt(() => actions.viewerSeek(f));
    }
    els[next]?.scrollIntoView({ block: "nearest" });
  };
  const onKey = (e: KeyboardEvent) => {
    if ((e.target as HTMLElement).closest("input")) return;
    const stop = () => {
      e.preventDefault();
      e.stopPropagation();
    };
    if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(e.key) && !e.altKey && !e.metaKey && !e.ctrlKey) {
      stop();
      arrow(e.key, e.shiftKey);
      return;
    }
    if ((e.key === "Delete" || e.key === "Backspace") && sel()) { stop(); void cutSel(); }
    else if (e.key === "*") { stop(); keywordSel(); }
    // Esc drops the selection; with nothing selected it goes on (leaving reading or focus)
    else if (e.key === "Escape" && (sel() || labelAsk())) { stop(); setAnchor(null); setFocus(null); setLabelAsk(null); }
  };
  // ⌘F and / (seeded with the hovered word) come from the global keys.
  createEffect(() => {
    const q = findRequest();
    if (q === null) return;
    // the words strip is the transcript on screen: its own search takes the request
    if (paneShown("words") && !reading()) return;
    setFindRequest(null);
    if (q) {
      setQuery(q);
      setHitAt(0);
      queueMicrotask(() => gotoHit(0));
    }
    search?.focus();
    search?.select();
  });

  // Re-spell (captions only) and word-edge tuning.
  const [spell, setSpell] = createSignal("");
  createEffect(on(respelling, (i) => {
    const t = d();
    if (i === null || !t) return;
    setSpell(t.edits?.[String(i)] ?? t.text[i] ?? "");
    queueMicrotask(() => spellInput?.focus());
  }));
  let spellInput: HTMLInputElement | undefined;
  const saveSpell = async (text: string | null) => {
    const v = cutView();
    const i = respelling();
    if (!v || i === null) return;
    setRespelling(null);
    await attempt(() => actions.respellWord(v.project, v.cut, i, text, owner()));
    pane?.focus({ preventScroll: true });
  };
  const tunedOf = (i: number | null) => (i === null ? undefined : d()?.tuned?.[String(i)]);
  const ms = (x: number) => `${x > 0 ? "+" : ""}${x} ms`;
  const addPart = async () => {
    const v = cutView();
    const s = sel();
    if (!v || !s) return;
    if (await attempt(() => actions.addAsPart(v.project, v.cut, s.lo, s.hi, owner()))) {
      setAnchor(null);
      setFocus(null);
    }
  };
  const stretch = async () => {
    const v = cutView();
    const s = sel();
    if (v && s) await attempt(() => actions.stretchLast(v.project, v.cut, s.hi, owner()));
  };

  // Reading: the whole source, or only the words the cut plays.
  const [cutOnly, setCutOnly] = createSignal(false);
  const readSize = (dx: number) => setPref("read_size", Math.max(12, Math.min(40, prefs.read_size + dx)));
  // entering reading keeps the word you were at in view
  createEffect(on(reading, (r, was) => {
    if (r === was) return;
    const w = sel()?.lo ?? nowWord();
    queueMicrotask(() => (w >= 0 ? els[w]?.scrollIntoView({ block: "center" }) : undefined));
  }, { defer: true }));

  const progress = () => d()?.progress ?? data()?.progress ?? null;
  const selInfo = () => {
    const t = d();
    const s = sel();
    if (!t || !s) return "";
    const inCut = selected().filter((i) => t.frame[i]! >= 0).length;
    return `${s.hi - s.lo + 1} words${inCut < s.hi - s.lo + 1 ? ` · ${s.hi - s.lo + 1 - inCut} already cut` : ""}`;
  };

  return (
    <section
      classList={{ transcript: true, reading: reading(), "no-times": true, "cut-only": reading() && cutOnly() }}
      aria-label="Transcript"
      data-testid="transcript-pane"
      style={{ "--tr-size": `${prefs.tr_size}px`, "--read-size": `${prefs.read_size}px` }}
    >
      <Show when={reading()}>
        <div class="tr-readbar" role="toolbar" aria-label="Reading">
          <span class="tr-readtitle"><Icon name="book-open" size={15} /><span class="tr-readlabel">Full transcript</span><span class="muted small tabular">{n() ? ` · ${n().toLocaleString()} words` : ""}</span></span>
          <SegmentedControl
            label="Which words"
            size="sm"
            testid="reading-which"
            options={[
              { value: "all", label: "Whole source", hint: "Every word of the recording; cut words struck" },
              { value: "cut", label: "Only the cut", hint: "Just the words this cut plays" },
            ]}
            value={cutOnly() ? "cut" : "all"}
            onChange={(v) => setCutOnly(v === "cut")}
          />
          <span class="tr-size" role="group" aria-label="Text size">
            <IconButton icon="minus" size="sm" label="Smaller text" data-testid="read-smaller" onClick={() => readSize(-2)} />
            <span class="small tabular" data-testid="read-size">{prefs.read_size}</span>
            <IconButton icon="plus" size="sm" label="Larger text" data-testid="read-larger" onClick={() => readSize(2)} />
          </span>
          <button type="button" class="ghost small tr-readdone" data-testid="reading-done" data-tip="Back to the editor" data-kbd="Esc" onClick={() => setReading(false)}>
            <Icon name="x" size={13} />Done
          </button>
        </div>
      </Show>
      <div class="tr-bar">
        <input
          ref={search}
          type="search"
          placeholder="Search the transcript  ⌘F"
          aria-label="Search the transcript"
          value={query()}
          onInput={(e) => { setQuery(e.currentTarget.value); setHitAt(0); queueMicrotask(() => gotoHit(0)); }}
          onKeyDown={(e) => {
            if (e.key === "Enter") { e.preventDefault(); gotoHit(hitAt() + (e.shiftKey ? -1 : 1)); }
            if (e.key === "Escape") { setQuery(""); pane.focus(); }
          }}
        />
        <Show when={query()}>
          <span class="muted small" data-testid="hits">{hits().length ? `${hitAt() + 1} / ${hits().length}` : "no matches"}</span>
          <input
            type="text"
            class="tr-replace"
            placeholder="replace in captions"
            aria-label="Replace in captions with"
            data-testid="replace-with"
            value={replaceWith()}
            onInput={(e) => setReplaceWith(e.currentTarget.value)}
            onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); void replace(false); } }}
          />
          <button type="button" class="ghost small" disabled={!hits().length} onClick={() => void replace(false)} title="Re-spell this match in the captions (audio untouched)">Replace</button>
          <button type="button" class="ghost small" data-testid="replace-all" disabled={!hits().length} onClick={() => void replace(true)} title="Re-spell every match in the captions (audio untouched)">All</button>
        </Show>
        <Show when={(d()?.videos?.length ?? 0) > 1}>
          <select
            class="tr-source"
            aria-label="Whose words"
            data-testid="transcript-source"
            data-tip="Show another video's words: select some and ＋ Add as part puts them in this cut"
            value={d()?.owner ?? ""}
            onChange={(e) => setTrOwner(e.currentTarget.value && e.currentTarget.value !== ownOwner() ? e.currentTarget.value : null)}
          >
            <For each={d()?.videos ?? []}>{(x) => <option value={x.owner}>{x.name}{x.in_cut ? "" : " (not in this cut)"}</option>}</For>
          </select>
        </Show>
        <Show when={!reading()}>
          <button type="button" class="ghost small tr-read" data-testid="reading-open" data-tip="Read the full transcript: prose, no times, large type" data-kbd="⌥R" onClick={() => setReading(true)}>
            <Icon name="book-open" size={14} />
            <span>Read</span>
          </button>
        </Show>
      </div>
      <Show when={progress() && progress()!.phase !== "done"}>
        <div class="tr-progress" data-testid="transcribing">
          <Show
            when={!progress()!.asr_error && progress()!.phase !== "failed"}
            fallback={<ModelMissing why={progress()!.asr_error ?? progress()!.error ?? ""} />}
          >
            <span>{progress()!.phase === "transcribing" ? "Transcribing…" : progress()!.phase === "queued" ? "Waiting for the other videos to be transcribed…" : "Analysing audio…"}</span>
            <progress max={Math.max(1, progress()!.total_s)} value={progress()!.phase === "transcribing" ? progress()!.done_s : progress()!.total_s} />
            <span class="muted small">{clock(progress()!.done_s * 1000)} / {clock(progress()!.total_s * 1000)}</span>
            <Show when={progress()!.eta_s != null && progress()!.phase !== "analysing"}>
              <span class="small" data-testid="transcript-eta">transcript in {about(progress()!.eta_s)}</span>
            </Show>
          </Show>
        </div>
      </Show>
      <Show when={fixing() && sel()}>
        <form class="fix-pop" data-testid="fix-pop" onSubmit={(e) => { e.preventDefault(); void fixEverywhere(); }} onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); setFixing(false); } e.stopPropagation(); }}>
          <span class="small">Heard <b>{d()?.text.slice(sel()!.lo, sel()!.hi + 1).join(" ")}</b> — show it as</span>
          <div class="row">
            <input type="text" aria-label="Show as" data-testid="fix-written" value={fixText()} onInput={(e) => setFixText(e.currentTarget.value)} ref={(el) => queueMicrotask(() => el.select())} />
            <Button type="submit" size="sm" variant="primary" data-testid="fix-apply" disabled={!fixText().trim()}>Fix everywhere</Button>
            <IconButton icon="x" size="sm" label="Close" onClick={() => setFixing(false)} />
          </div>
          <Switch testid="fix-all" on={fixAll()} onChange={setFixAll} label={<>In all my projects</>} hint="Every new transcription spells it this way, and the name check before export knows it" />
        </form>
      </Show>
      <div classList={{ "tr-sel": true, on: !!sel() || respelling() !== null || tuning() !== null }} role="toolbar" aria-label="Transcript actions">
        <Show
          when={respelling() === null}
          fallback={
            <form class="row" onSubmit={(e) => { e.preventDefault(); void saveSpell(spell()); }}>
              <span class="small">Caption spelling (audio unchanged):</span>
              <input ref={spellInput} aria-label="Caption spelling" value={spell()} onInput={(e) => setSpell(e.currentTarget.value)}
                onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); setRespelling(null); pane?.focus(); } }} />
              <button type="submit">Save</button>
              <Show when={d()?.edits?.[String(respelling())] !== undefined}>
                <button type="button" class="ghost" onClick={() => void saveSpell(null)}>↺ Spelling</button>
              </Show>
              <span class="muted small">empty hides it from captions</span>
            </form>
          }
        >
          <Show
            when={sel()}
            fallback={
              <Show
                when={tuning() !== null}
                fallback={<span />}
              >
                <span class="small" data-testid="tune-readout">
                  “{d()?.text[tuning()!]}” start {ms(tunedOf(tuning())?.[0] ?? 0)} · end {ms(tunedOf(tuning())?.[1] ?? 0)}
                </span>
                <label class="small">step
                  <select aria-label="Edge step" value={edgeStep()} onChange={(e) => setEdgeStep(Number(e.currentTarget.value))}>
                    <For each={EDGE_STEPS}>{(x) => <option value={x}>{x} ms</option>}</For>
                  </select>
                </label>
                <button type="button" class="ghost" onClick={() => { const v = cutView(); const i = tuning(); if (v && i !== null) void attempt(() => actions.edgeReset(v.project, v.cut, i, owner())); }}>↺ Edges</button>
                <button type="button" class="ghost" aria-label="Done tuning" onClick={() => setTuning(null)}>✕</button>
              </Show>
            }
          >
            <span>{selInfo()}</span>
            <button type="button" onClick={() => void cutSel()} title="Delete or Backspace">{allStruck() ? "Restore" : "Cut"}</button>
            <Show when={isStream()}>
              <button type="button" data-testid="label-topic" onClick={() => setLabelAsk(sel())} title="T">Topic…</button>
            </Show>
            <button type="button" onClick={keywordSel} title="*">★ Keyword</button>
            {/* Fix everywhere (K.02), here since the words strip retired (2026-11-22): 1–4
                words spelt right in every caption of the project, and a term */}
            <Show when={sel()!.hi - sel()!.lo < 4 && cutView()}>
              <button type="button" class="ghost" data-testid="fix-everywhere" data-tip="A name the transcriber got wrong: spell it right in every caption of the project and add it to your terms" onClick={openFix}>
                <Icon name="pencil" size={13} />Fix everywhere…
              </button>
            </Show>
            <Show when={sel()!.hi - sel()!.lo >= 2 && cutView()}>
              <button type="button" class="ghost" data-testid="tr-broll" data-tip="B-roll: moments from your other recordings that talk about these words" onClick={() => { const v = cutView(); const x = sel(); if (v && x) askBroll({ project: v.project, cut: v.cut, lo: x.lo, hi: x.hi }); }}>
                <Icon name="film" size={13} />B-roll…
              </button>
            </Show>
            <Show when={sel()!.lo === sel()!.hi}>
              <button type="button" onClick={() => setRespelling(sel()!.lo)} title="F or double-click">✎ Spelling</button>
            </Show>
            <Show when={allStruck()}>
              <button type="button" data-testid="add-part" onClick={() => void addPart()} title="A new part after the last one">＋ Add as part</button>
              <button type="button" class="ghost" onClick={() => void stretch()} title="The last part ends at this word">↧ Stretch last part</button>
            </Show>
          </Show>
        </Show>
      </div>
      <Show when={labelAsk()}>
        <form class="topic-label" role="dialog" aria-label="Label topic" onSubmit={(e) => { e.preventDefault(); void applyLabel(); }}>
          <span class="small">Topic for {labelAsk()!.hi - labelAsk()!.lo + 1} words:</span>
          <input
            ref={labelInput}
            type="text"
            list="topic-titles"
            aria-label="Topic title"
            data-testid="topic-title"
            value={labelText()}
            onInput={(e) => setLabelText(e.currentTarget.value)}
            onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); setLabelAsk(null); pane?.focus(); } }}
          />
          <datalist id="topic-titles">
            <For each={[...new Set([...recentTitles(), ...(overlay()?.topics ?? []).map((t) => t.title)])]}>{(t) => <option value={t} />}</For>
          </datalist>
          <button type="submit" class="primary">Label</button>
          <button type="button" class="ghost" aria-label="Cancel" onClick={() => setLabelAsk(null)}>✕</button>
        </form>
      </Show>
      <Show
        when={n() > 0}
        fallback={
          <p class="empty" data-testid="transcript-empty">
            {progress()?.phase === "transcribing" ? "Listening… the first words appear in a few seconds." : progress()?.phase === "queued" ? "Waiting its turn: videos are transcribed one at a time." : "No transcript for this video yet."}
          </p>
        }
      >
        <div class="tr-body" ref={pane} tabindex="0" role="listbox" aria-label="Transcript words" aria-multiselectable="true" onKeyDown={onKey} data-testid="transcript">
          <For each={paras()}>
            {(para) => (
            <div
              classList={{ "tr-para": true, "all-out": para.out, topic: !!topicAt(para.lines[0]!.a), proposed: topicAt(para.lines[0]!.a)?.state === "proposed" }}
              style={topicAt(para.lines[0]!.a) ? { "--topic": topicAt(para.lines[0]!.a)!.colour } : undefined}
            >
            <For each={para.lines}>
            {(l) => (
              <>
              <For each={topicHeads().get(l.a) ?? []}>
                {(tp) => (
                  <div classList={{ "topic-head": true, proposed: tp.state === "proposed", conflict: tp.conflict }} style={{ "--topic": tp.colour }} data-testid="topic-head" data-topic={tp.id}>
                    <Show when={!tp.from_stream}>
                    <span
                      class="topic-edge"
                      role="button"
                      tabindex="-1"
                      aria-label="Topic start: drag onto a word"
                      data-tip="Drag onto a word to move where this topic starts"
                      data-testid="topic-edge-start"
                      onPointerDown={(e) => { e.preventDefault(); edgeDrag = { topic: tp.id, edge: "start" }; document.body.classList.add("edge-dragging"); }}
                    >⇤</span>
                    </Show>
                    <b class="topic-title">{tp.title}</b>
                    <span class="muted small">{clockMs(tp.from_ms)}–{clockMs(tp.to_ms)}</span>
                    <Show when={tp.state === "proposed" && !tp.from_stream}>
                      <span class="small topic-state">{tp.replaces.length ? "proposed split/merge" : "proposed"}{tp.origin === "agent" ? " by your agent" : ""}</span>
                      <Show when={tp.conflict}><span class="small conflict-note" data-testid="conflict">conflicts with your edit</span></Show>
                      <button type="button" class="small" data-testid="topic-accept" onClick={() => decideTopic(tp, "accepted")}>Accept</button>
                      <button type="button" class="ghost small" data-testid="topic-reject" onClick={() => decideTopic(tp, "rejected")}>Reject</button>
                    </Show>
                    <Show when={!tp.from_stream}>
                    <span
                      class="topic-edge"
                      role="button"
                      tabindex="-1"
                      aria-label="Topic end: drag onto a word"
                      data-tip="Drag onto a word to move where this topic ends"
                      data-testid="topic-edge-end"
                      onPointerDown={(e) => { e.preventDefault(); edgeDrag = { topic: tp.id, edge: "end" }; document.body.classList.add("edge-dragging"); }}
                    >⇥</span>
                    </Show>
                  </div>
                )}
              </For>
              <For each={fluffHeads().get(l.a) ?? []}>
                {(f) => (
                  <div classList={{ "fluff-head": true, conflict: f.conflict }} data-testid="fluff-head">
                    <span class="small">✂ Fluff? {f.reason || "(no reason)"} · {dur(f.to_ms - f.from_ms)}</span>
                    <Show when={f.conflict}><span class="small conflict-note">conflicts with your edit</span></Show>
                    <button type="button" class="ghost small" title="Play it" onClick={() => void attempt(() => actions.viewerPlaySrc(f.from_ms, f.to_ms))}>▶</button>
                    <button type="button" class="small" data-testid="fluff-accept" onClick={() => decideFluff(f, "accepted")}>Strike</button>
                    <button type="button" class="ghost small" data-testid="fluff-reject" onClick={() => decideFluff(f, "rejected")}>Keep</button>
                  </div>
                )}
              </For>
              <Show when={collapse().heads.get(l.a)}>
                {(c) => (
                  <button type="button" class="struck-run" data-testid="struck-run" onClick={() => setExpanded((x) => new Set([...x, l.a]))} title="Show the struck words">
                    – {dur(c().ms)} struck
                  </button>
                )}
              </Show>
              <p
                classList={{ "tr-line": true, topic: !!topicAt(l.a), proposed: topicAt(l.a)?.state === "proposed" }}
                style={topicAt(l.a) ? { "--topic": topicAt(l.a)!.colour } : undefined}
                hidden={collapse().hidden.has(l.a)}
              >
                <span class="tr-t" aria-hidden="true">{l.t}</span>
                <For each={chips().get(l.a) ?? []}>
                  {(c) => (
                    <button type="button" class={`cov-chip ${c.kind}`} aria-label={`Used by “${c.title}” — open it`} data-tip={`Used by the ${c.kind === "long" ? "long video" : "short"} “${c.title}” — open it`} data-testid="coverage-chip" onClick={() => { const v = cutView(); if (v) void openCut(v.project, c.cut); }}>
                      <span aria-hidden="true">{c.kind === "long" ? "▭" : "▯"}</span>
                      <span class="cov-name">{c.title}</span>
                    </button>
                  )}
                </For>
                <For each={Array.from({ length: l.b - l.a }, (_, j) => l.a + j)}>
                  {(i) => {
                    const t = d()!;
                    const out = t.frame[i]! < 0;
                    const ed = t.edits?.[String(i)];
                    return (
                      <span
                        ref={(el) => (els[i] = el)}
                        role="option"
                        aria-selected="false"
                        classList={{ w: true, out, kw: t.keywords.includes(i), respelled: ed !== undefined && ed !== "", nocap: ed === "", tuned: !!t.tuned?.[String(i)], again: againWords().has(i) }}
                        title={ed !== undefined ? (ed === "" ? `“${t.text[i]}” is hidden from captions` : `heard “${t.text[i]}”`) : againWords().has(i) ? "Said again just after: Tighten (in the status) keeps the second try" : undefined}
                        onDblClick={() => setRespelling(i)}
                        data-i={i}
                        onPointerDown={(e) => down(e, i)}
                        onPointerEnter={(e) => { setTrHover(i); enter(e, i); }}
                        onPointerLeave={() => setTrHover(null)}
                        onPointerUp={() => up(i)}
                      >
                        {ed !== undefined && ed !== "" ? ed : t.text[i]}
                      </span>
                    );
                  }}
                </For>
              </p>
              </>
            )}
          </For>
            </div>
            )}
          </For>
        </div>
      </Show>
    </section>
  );
}

/** No transcript: says why; when the speech model is missing, offers to download it (a
 *  click — never a silent download), with progress. */
function ModelMissing(props: { why: string }) {
  const [st, { refetch }] = createResource(async () => (await attempt(actions.modelsStatus)) ?? null);
  const p = () => modelsProgress();
  const pct = () => {
    const x = p();
    return x?.total ? Math.round((100 * (x.done ?? 0)) / x.total) : 0;
  };
  return (
    <span class="model-missing">
      <Show
        when={st() && !st()!.installed}
        fallback={<span class="muted">No transcript: {props.why}</span>}
      >
        <span class="muted">The speech model is not on {thisMachine} yet.</span>
        <Show
          when={p()?.state === "running" || st()!.running}
          fallback={
            <button type="button" class="primary" data-testid="download-model" onClick={() => void attempt(actions.modelsDownload).then(() => refetch())}>
              Download speech model ({st()!.mb} MB)
            </button>
          }
        >
          <span>Downloading… {pct()}%</span>
          <progress max="100" value={pct()} />
        </Show>
      </Show>
    </span>
  );
}
