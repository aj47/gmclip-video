// The editor's keys (research/editor-inventory.md §1.15, §8.1–8.3). Playback, cutting
// and transcript first; crops and layouts with the 9:16 work.
//
// Rules carried over: keys are ignored while typing in a field; with ⌘/Ctrl only s, z
// (and ⇧z for redo, which the station lacked), f and g (part groups) are handled; ? opens the list.
// Where the station did something gmclip does not do yet, the key is left unbound rather
// than doing something else.
import { actions, pendingSeek, rate, type PartView } from "./actions";
import { attempt, cutView, markIn, markOut, markedRange, measureFrom, saveRestorePoint, say, sayUndo, selectedEdge, selectedPart, setLanesTick, setMarkIn, setMarkOut, setMeasureFrom, setSelectedEdge, setSelectedPart, tlHover, tlHoverFrame, viewer } from "./store";
import { clearTrSel, setFindRequest, setRespelling, strikeOrRestore, trData, trHover, trSel, tuneEdge, wordAtFrame } from "./transcriptState";
import { setLabelAsk, showInStream, shortsHere, topicAt } from "./streamState";
import { tapMark } from "./tapmark";
import { reading, resetLayout, setReading, tabShown, toggleFocus, togglePane, toggleReading, toggleTab } from "./layout";
import { prefs } from "./prefs";
import { groupSelected, ungroupSelected } from "./partGroups";
import { removeSection, selectedSection } from "./sectionsState";

export interface KeyRow {
  keys: string[];
  what: string;
}

export const GROUPS: { name: string; rows: KeyRow[] }[] = [
  {
    name: "Playback",
    rows: [
      { keys: ["Space"], what: "Play / pause (from the timeline's hover line while the pointer is on it)" },
      { keys: ["J"], what: "Play backwards; again ×1.5 faster (wraps after 5×)" },
      { keys: ["K"], what: "Pause (forwards or backwards)" },
      { keys: ["L"], what: "Play; again ×1.5 faster (wraps after 5×); after J, forwards at 1×" },
      { keys: ["←", "→"], what: "One frame back / forward (⇧ ten)" },
      { keys: ["↑", "↓"], what: "Previous / next part" },
      { keys: ["Home", "End"], what: "Start / end of the cut" },
      { keys: ["E"], what: "Full-quality frame on / off" },
    ],
  },
  {
    name: "Cutting",
    rows: [
      { keys: ["S"], what: "Split before the hovered word, else at the playhead" },
      { keys: ["I", "O"], what: "Mark in / out at the playhead: the range (⌥L loops it, ⇧⌫ removes it)" },
      { keys: ["⇧I", "⇧O"], what: "Trim the part's in / out to the playhead" },
      { keys: ["H"], what: "Hold the screen on the frame shown, from the playhead to the end of the sentence being said (camera and audio stay live); again removes it" },
      { keys: ["Q"], what: "Trim the start to the hovered word (else the playhead); at a part's head, delete the part before" },
      { keys: ["W"], what: "Trim the end to the hovered word's end (else the playhead); at a part's end, delete the part after" },
      { keys: ["⌫", "Delete"], what: "Delete the selected part; the rest closes up (in the transcript: cut the selected words; on the zoom and layout lanes: the selected section)" },
      { keys: ["⌘-click", "⇧-click"], what: "On the timeline: add a part to the selection / select the parts up to it; drag one to move them all" },
      { keys: ["⇧-drag", "⌘-drag"], what: "Across the parts (or any drag in the empty space under the lanes): select every part it covers; ⌘ adds to the selection" },
      { keys: ["⌘G", "⌘⇧G"], what: "Group the selected parts (they move together) / ungroup" },
      { keys: ["⇧⌫"], what: "Ripple-delete the marked range: its frames leave the cut and the rest closes up (no range: the selected part)" },
      { keys: ["V"], what: "Pick the part edge nearest the playhead (again at a join: the other side)" },
      { keys: ["⌥←", "⌥→"], what: "Nudge that edge a frame earlier / later (⇧ ten frames)" },
      { keys: ["⌥↑", "⌥↓"], what: "Move the selected part earlier / later" },
      { keys: ["Esc"], what: "Drop the picked edge, then the marks" },
      { keys: ["⌘Z"], what: "Undo" },
      { keys: ["⌘⇧Z"], what: "Redo" },
      { keys: ["⌘S"], what: "Save a restore point (every change is already saved as you go)" },
      { keys: ["⌘O"], what: "Open a video" },
      { keys: ["⌘⇧O"], what: "Add videos to this project" },
      { keys: ["⌘E"], what: "Export" },
    ],
  },
  {
    name: "Captions & transcript",
    rows: [
      { keys: ["D", "X"], what: "Cut the selected words, else the hovered word, else the word at the playhead; on cut words, put them back" },
      { keys: ["P"], what: "Play exactly the hovered or selected word" },
      { keys: ["F"], what: "Re-spell the word in captions (audio unchanged)" },
      { keys: ["[", "]"], what: "Move the word's start earlier / later by the step" },
      { keys: ["{", "}"], what: "Move the word's end earlier / later by the step" },
      { keys: ["*"], what: "Keyword on / off (transcript selection)" },
      { keys: ["T"], what: "Name a topic: select words in the main cut's transcript and press T (recent titles offered; Enter applies). Topics show in the project map once there is one" },
      { keys: ["⇧T"], what: "Stream edit: split the topic at the caret (the selection's first word, else the hovered word)" },
      { keys: ["⌥-click"], what: "Select the whole line (⇧⌥-click extends to it)" },
      { keys: ["←", "→"], what: "In the transcript (⌥T, or click a word): the word before / after; ⇧ extends the selection" },
      { keys: ["↑", "↓"], what: "In the transcript: the line above / below; ⇧ extends the selection" },
      { keys: ["⌘↑"], what: "Show in stream: the same words in the stream edit" },
      { keys: ["⌘↓"], what: "Shorts here: the long-forms and shorts that use the selected words" },
      { keys: ["⌘F"], what: "Search the transcript" },
      { keys: ["/"], what: "Search, seeded with the hovered word" },
      { keys: ["Enter", "⇧Enter"], what: "Next / previous match (in search)" },
      { keys: ["Esc"], what: "Drop the selection / close search" },
    ],
  },
  {
    name: "Timeline",
    rows: [
      { keys: ["−", "="], what: "Zoom out / in (also ⌥-wheel)" },
      { keys: ["wheel", "⇧-wheel"], what: "Scroll through time (over the ruler and parts; ⇧ anywhere); also a middle-button drag, or the scrollbar under the lanes" },
      { keys: ["drag a part"], what: "Move it (drop between two parts)" },
      { keys: ["M", "⇧M"], what: "Measure: mark the start / the end at the playhead (a bracket with its value)" },
      { keys: ["drag an overlay"], what: "Move a hook, pill or clock; drag its edge to re-time it (snaps to parts and words, ⌥ off)" },
      { keys: ["double-click the gain lane"], what: "Add 1 s of gain there at that level (or drag across it); drag it to move or change its level, its edges to re-time; double-click removes" },
      { keys: ["Esc"], what: "Cancel a drag (nothing is written)" },
      { keys: ["⌥ drag an edge"], what: "Trim by frames without snapping to words" },
      { keys: ["⇧-click"], what: "Extend a word selection" },
      { keys: ["blade tool"], what: "Timeline toolbar: click a part to split it there (the select tool trims and moves)" },
      { keys: ["snapping"], what: "Timeline toolbar: off, edges move by frames (as with ⌥ held)" },
      { keys: ["⌥L"], what: "Loop the range, else the part under the playhead (also the transport's loop button)" },
      { keys: ["⌥S"], what: "Hover scrub on / off (on: the viewer follows the pointer over the ruler; off: hovering shows the time only)" },
    ],
  },
  {
    name: "On the frame",
    rows: [
      { keys: ["drag a layer"], what: "Move it; drag a corner to resize (snaps to the frame and the other layers, ⌥ off)" },
      { keys: ["drag a hook or pill"], what: "Move the text on the frame (its centre snaps to the middle, ⌥ off)" },
      { keys: ["drag the caption line"], what: "Move the captions up or down for every part; ⇧ only this part" },
      { keys: ["drag a crop box"], what: "In Crop mode: move a layer's crop on the source; a corner resizes it, keeping its shape" },
      { keys: ["Esc"], what: "Cancel a drag on the frame (it goes back; nothing is written)" },
    ],
  },
  {
    name: "View",
    rows: [
      { keys: ["⌥1"], what: "Project map on / off" },
      { keys: ["⌥2"], what: "Side panel on / off" },
      { keys: ["⌥3"], what: "Timeline on / off" },
      { keys: ["⌥4"], what: "Words (the transcript) on / off" },
      { keys: ["⌥T"], what: "Words (the transcript) in the side panel, with the arrow keys in its words (again from the words hides it)" },
      { keys: ["⌥I"], what: "Look (layout and the part's inspector) in the side panel (again hides it)" },
      { keys: ["⌥R"], what: "Read the full transcript: prose, no times, beside the viewer (Esc leaves)" },
      { keys: ["⌥F"], what: "Focus: only the viewer and the timeline (Esc leaves)" },
      { keys: ["⌥0"], what: "Reset layout: every pane back at its own size" },
      { keys: ["drag a pane's edge"], what: "Resize the side panel, the map or the timeline; double-click the edge resets it" },
    ],
  },
  {
    name: "Library",
    rows: [
      { keys: ["⌘⇧F"], what: "Search everything you said, across every project (from anywhere)" },
      { keys: ["↑", "↓", "Enter"], what: "In search: move through the hits; Enter plays one" },
      { keys: [",", "."], what: "Mark in / out while watching (any speed): the pair becomes a moment on sentence edges" },
    ],
  },
  {
    name: "Help",
    rows: [
      { keys: ["?"], what: "This list (Esc or ? closes it)" },
      { keys: ["⌘K"], what: "Command palette: every action and panel by name (Compare openings, Polish, Draw your layout…)" },
      { keys: ["View menu"], what: "Every pane and every side-panel tab (Short, Polish, Thumbnail…) by name" },
    ],
  },
];

function partAt(parts: PartView[], f: number): PartView | undefined {
  return parts.find((p) => f >= p.out[0] && f < p.out[1]);
}

/** The word a key acts on: the hovered one, else the selection's first, else the playhead's. */
function targetWord(): number {
  const h = trHover();
  if (h !== null) return h;
  const s = trSel();
  if (s) return s.lo;
  const f = viewer()?.frame;
  return f === undefined ? -1 : wordAtFrame(f);
}


/** ↑ / ↓ and the transport's part buttons: to the previous / next part's head. */
/** ↑ / ↓: the playhead to the previous / next part's head, and that part selected. From
 * the frame the last seek asked for while the viewer has not shown it yet: a seek's reply
 * comes before the viewer reports the frame, so a press right after another read the old
 * frame and jumped from there (↑ then ↓ went nowhere, ↓ ↓ landed on the same part). */
export function jumpPart(dir: -1 | 1) {
  const v = cutView();
  if (!v) return;
  const vs = viewer();
  const f = (!vs?.playing ? pendingSeek() : null) ?? vs?.frame ?? 0;
  const heads = v.parts.map((p) => p.out[0]).sort((a, b) => a - b);
  const to = dir < 0 ? [...heads].reverse().find((h) => h < f) : heads.find((h) => h > f);
  if (to === undefined) return;
  void attempt(() => actions.viewerSeek(to));
  const p = partAt(v.parts, to);
  if (p) setSelectedPart(p.id);
}

/** m:ss.ff of an output frame (the transport's timecode). */
function tc(f: number, fps: number): string {
  const r = Math.max(1, fps);
  const s = Math.floor(f / r);
  const ff = Math.max(0, Math.floor(f - s * r + 1e-6));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}.${String(ff).padStart(2, "0")}`;
}

/** The shuttle's next speed: ×1.5 per press, back to 1× past 5×. */
export function shuttleNext(rate: number): number {
  const r = Math.abs(rate) * 1.5;
  return r > 5 ? 1 : r;
}

/** V: the part edge nearest output frame `f`. At a join (one part's out, the next one's
 * in) the side the playhead is on, and the other side when that one is already picked. */
export function nearestEdge(parts: PartView[], f: number, cur: { part: string; edge: "in" | "out" } | null): { part: string; edge: "in" | "out" } | null {
  const all = parts.flatMap((p) => [
    { part: p.id, edge: "in" as const, at: p.out[0] },
    { part: p.id, edge: "out" as const, at: p.out[1] },
  ]);
  if (!all.length) return null;
  let best = all[0]!.at;
  for (const c of all) if (Math.abs(c.at - f) < Math.abs(best - f)) best = c.at;
  const here = all.filter((c) => c.at === best);
  const picked = cur ? here.findIndex((c) => c.part === cur.part && c.edge === cur.edge) : -1;
  const pick = picked >= 0 && here.length > 1 ? here[(picked + 1) % here.length]! : (here.find((c) => (f >= best ? c.edge === "in" : c.edge === "out")) ?? here[0]!);
  return { part: pick.part, edge: pick.edge };
}

/** ⌥← / ⌥→: the picked edge (else the nearest) by `d` frames; the viewer shows the
 * frame on that edge (an in: the part's new first frame; an out: its new last one). */
function nudgeEdge(d: number) {
  const v = cutView();
  if (!v) return;
  const f = viewer()?.frame ?? 0;
  let e = selectedEdge();
  if (!e || !v.parts.some((p) => p.id === e!.part)) {
    e = nearestEdge(v.parts, f, null);
    if (!e) return;
    setSelectedEdge(e);
  }
  const p = v.parts.find((x) => x.id === e!.part)!;
  const edge = e.edge;
  void attempt(() => actions.trim(v.project, v.cut, p.id, edge, d)).then((r) => {
    if (r === undefined) return;
    const at = edge === "in" ? p.out[0] : Math.max(p.out[0], p.out[1] + d - 1);
    void attempt(() => actions.viewerSeek(at));
  });
}

/** The View keys (⌥ + a digit or letter, by physical key: ⌥ changes the character). */
export const VIEW_KEYS: Record<string, () => void> = {
  Digit1: () => togglePane("map"),
  Digit2: () => togglePane("side"),
  Digit3: () => togglePane("timeline"),
  Digit4: () => togglePane("words"),
  KeyT: () => {
    // shown: the words take the keys (← → ↑ ↓ select, D cuts); from the words, it hides
    const words = () => document.querySelector<HTMLElement>('[data-testid="transcript"]');
    const inWords = !!words()?.contains(document.activeElement);
    if (!(tabShown("words") && !reading()) || inWords) toggleTab("words");
    requestAnimationFrame(() => {
      if (tabShown("words") && !reading()) words()?.focus({ preventScroll: true });
    });
  },
  KeyI: () => toggleTab("look"),
  KeyR: toggleReading,
  KeyF: toggleFocus,
  Digit0: resetLayout,
};

/** ⌥-keys of the View menu; true when handled. */
export function viewKey(e: KeyboardEvent): boolean {
  if (!e.altKey || e.metaKey || e.ctrlKey || e.shiftKey) return false;
  const f = VIEW_KEYS[e.code];
  if (!f) return false;
  f();
  return true;
}

/** Esc with nothing else to drop: leaves reading, then focus. */
function leaveMode(): boolean {
  if (reading()) {
    setReading(false);
    return true;
  }
  if (prefs.focus) {
    toggleFocus();
    return true;
  }
  return false;
}

/** The cut view's keys; returns true when the key was handled. */
export function cutKey(e: KeyboardEvent, openHelp: () => void): boolean {
  const v = cutView();
  if (!v) return false;
  const vs = viewer();
  const f = vs?.frame ?? 0;
  const fps = vs?.fps ?? rate(v.fps);
  const t = trData();
  const k = e.key;
  if (e.metaKey || e.ctrlKey) {
    // ⌘G groups the selected parts, ⌘⇧G ungroups (decision 0037)
    if (k.toLowerCase() === "g") {
      void (e.shiftKey ? ungroupSelected() : groupSelected());
      return true;
    }
    if (k.toLowerCase() === "s") {
      void saveRestorePoint();
      return true;
    }
    if (k.toLowerCase() === "f") {
      setFindRequest("");
      return true;
    }
    if (k === "ArrowUp") {
      void showInStream();
      return true;
    }
    if (k === "ArrowDown") {
      void shortsHere();
      return true;
    }
    return false;
  }
  if (e.altKey) {
    if (k === "ArrowLeft" || k === "ArrowRight") {
      nudgeEdge((k === "ArrowLeft" ? -1 : 1) * (e.shiftKey ? 10 : 1));
      return true;
    }
    if ((k === "ArrowUp" || k === "ArrowDown") && !e.shiftKey) {
      const sel = v.parts.find((p) => p.id === selectedPart()) ?? partAt(v.parts, f);
      if (!sel) return true;
      setSelectedPart(sel.id);
      void attempt(() => actions.partMove(v.project, v.cut, sel.id, k === "ArrowUp" ? "up" : "down"));
      return true;
    }
    return viewKey(e);
  }
  switch (k) {
    case "?":
      openHelp();
      return true;
    case " ": {
      // paused with the pointer on the timeline: play from its hover line, else from the
      // playhead (playing: pause)
      const h = tlHover();
      if (vs && !vs.playing && h && h.left === null) {
        const at = Math.max(0, Math.min(h.frame, (vs.frames || v.frames) - 1));
        void attempt(() => actions.viewerSeek(at)).then(() => attempt(actions.viewerPlay));
        return true;
      }
      void attempt(actions.viewerToggle);
      return true;
    }
    case "j":
    case "J": {
      // backwards; again faster (the viewer plays it silently on the scrub path)
      const r = vs?.rate ?? 1;
      void attempt(() => actions.viewerRate(-(r < 0 ? shuttleNext(r) : 1)));
      return true;
    }
    case "k":
    case "K":
      void attempt(actions.viewerPause).then(() => attempt(() => actions.viewerRate(1)));
      return true;
    case "l":
    case "L": {
      if (!vs?.playing || (vs.rate ?? 1) < 0) {
        void attempt(() => actions.viewerRate(1)).then(() => attempt(actions.viewerPlay));
      } else {
        void attempt(() => actions.viewerRate(shuttleNext(vs.rate ?? 1)));
      }
      return true;
    }
    case "ArrowLeft":
    case "ArrowRight":
      void attempt(() => actions.viewerStep((k === "ArrowLeft" ? -1 : 1) * (e.shiftKey ? 10 : 1)));
      return true;
    case "ArrowUp":
    case "ArrowDown":
      jumpPart(k === "ArrowUp" ? -1 : 1);
      return true;
    case "Home":
    case "End":
      void attempt(() => actions.viewerSeek(k === "Home" ? 0 : Math.max(0, (vs?.frames ?? v.frames) - 1)));
      return true;
    case "e":
    case "E":
      void attempt(() => actions.viewerFull(!(vs?.full ?? false)));
      return true;
    case "s":
    case "S": {
      if (e.repeat) return true;
      const h = trHover();
      const wf = h !== null && t ? t.frame[h]! : -1;
      // a hovered word, else the timeline's hover line (on it or just left), else the playhead
      const at = wf > 0 ? wf : tlHoverFrame() ?? f;
      void attempt(() => actions.split(v.project, v.cut, at));
      return true;
    }
    case "m":
    case "M": {
      if (e.repeat) return true;
      if (!e.shiftKey) {
        setMeasureFrom(f);
        say("Measuring: ⇧M at the end");
        return true;
      }
      const a = measureFrom();
      if (a === null) say("Press M at the start first", "error");
      else if (f <= a) say("The end comes after the start", "error");
      else {
        setMeasureFrom(null);
        void attempt(() => actions.runMeasure(v.project, v.cut, a, f)).then((r) => {
          setLanesTick((n) => n + 1);
          if (r) say(`Measured ${r.seconds.toFixed(2)} s`);
        });
      }
      return true;
    }
    case "h":
    case "H":
      if (e.repeat) return true;
      void attempt(() => actions.holdToggle(v.project, v.cut, f)).then((r) => r && say(r.held ? `Screen held to the end of the ${r.until === "sentence" ? "sentence" : "part"}` : "Hold removed"));
      return true;
    case "i":
    case "o":
    case "I":
    case "O": {
      if (!e.shiftKey) {
        // mark in / out (the range: ⌥L loops it, ⇧⌫ removes it)
        const isIn = k.toLowerCase() === "i";
        const other = isIn ? markOut() : markIn();
        if (isIn) setMarkIn(f);
        else setMarkOut(f);
        // a mark past the other one starts the range again from here
        if (other !== null && (isIn ? other <= f : other >= f)) (isIn ? setMarkOut : setMarkIn)(null);
        const r = markedRange();
        say(r ? `Range ${tc(r[0], fps)}–${tc(r[1], fps)} · ⇧⌫ removes it, ⌥L loops it` : `${isIn ? "In" : "Out"} at ${tc(f, fps)} · ${isIn ? "O" : "I"} marks the ${isIn ? "out" : "in"}`);
        return true;
      }
      const sel = v.parts.find((p) => p.id === selectedPart());
      const p = sel && f >= sel.out[0] && f < sel.out[1] ? sel : partAt(v.parts, f);
      if (!p) return true;
      if (k.toLowerCase() === "i") {
        if (f > p.out[0]) void attempt(() => actions.trim(v.project, v.cut, p.id, "in", f - p.out[0]));
      } else if (f > p.out[0]) {
        void attempt(() => actions.trim(v.project, v.cut, p.id, "out", f - p.out[1]));
      }
      return true;
    }
    case "q":
    case "Q":
    case "w":
    case "W": {
      const start = k.toLowerCase() === "q";
      const h = trHover();
      let at = tlHoverFrame() ?? f;
      if (h !== null && t && t.frame[h]! >= 0) {
        at = start ? t.frame[h]! : t.frame[h]! + Math.max(1, Math.round((t.dur_ms[h]! * fps) / 1000));
      }
      const p = partAt(v.parts, start ? at : Math.max(0, at - 1));
      if (!p) return true;
      const n = v.parts.indexOf(p);
      if (start) {
        if (at <= p.out[0]) {
          const prev = v.parts[n - 1];
          if (prev) void attempt(() => actions.deletePart(v.project, prev.id));
        } else void attempt(() => actions.trim(v.project, v.cut, p.id, "in", at - p.out[0]));
      } else if (at >= p.out[1]) {
        const next = v.parts[n + 1];
        if (next) void attempt(() => actions.deletePart(v.project, next.id));
      } else void attempt(() => actions.trim(v.project, v.cut, p.id, "out", at - p.out[1]));
      return true;
    }
    case "d":
    case "D":
    case "x":
    case "X": {
      const s = trSel();
      if (s) void strikeOrRestore(s.lo, s.hi).then(clearTrSel);
      else {
        const w = targetWord();
        if (w >= 0) void strikeOrRestore(w, w);
      }
      return true;
    }
    case "t": {
      if (v.kind !== "stream") return false;
      const s = trSel();
      if (s) setLabelAsk(s);
      else say("Select the topic's words first (drag, ⇧-click, ⌥-click a line)");
      return true;
    }
    case "T": {
      if (v.kind !== "stream") return false;
      const s = trSel();
      const at = s ? s.lo : trHover();
      const tp = at === null ? undefined : topicAt(at);
      if (at === null || !tp) {
        say("Put the caret inside a topic to split it");
        return true;
      }
      void attempt(() => actions.topicSplit(v.project, v.cut, tp.id, at)).then((r) => r && say("Topic split · ⌘Z undoes"));
      return true;
    }
    case "p":
    case "P": {
      const s = trSel();
      const w = s ? s.lo : targetWord();
      if (!t || w < 0 || t.frame[w]! < 0) return true;
      const last = s ? s.hi : w;
      const ms = t.start_ms[last]! + t.dur_ms[last]! - t.start_ms[w]!;
      // exactly the word(s): the audio stops on the last word's end sample
      void attempt(() => actions.viewerPlaySrc(t.start_ms[w]!, t.start_ms[w]! + Math.max(1, ms)));
      return true;
    }
    case "f":
    case "F": {
      const w = targetWord();
      if (w >= 0) setRespelling(w);
      return true;
    }
    case "[":
    case "]":
    case "{":
    case "}": {
      const w = targetWord();
      if (w >= 0) void tuneEdge(w, e.key === "[" || e.key === "]" ? "start" : "end", e.key === "]" || e.key === "}" ? 1 : -1);
      return true;
    }
    case "/": {
      const h = trHover();
      setFindRequest(h !== null && t ? (t.text[h] ?? "").replace(/[^\p{L}\p{N}']+/gu, "") : "");
      return true;
    }
    case "Backspace":
    case "Delete": {
      const r = e.shiftKey ? markedRange() : null;
      if (e.shiftKey && !r && !selectedPart() && !trSel()) {
        say("Mark the range first (I at its start, O at its end), or pick a part (↑ ↓)");
        return true;
      }
      if (r) {
        void attempt(() => actions.rangeDelete(v.project, v.cut, r[0], r[1])).then((x) => {
          if (!x) return;
          setMarkIn(null);
          setMarkOut(null);
          setSelectedEdge(null);
          sayUndo(`Removed ${(x.removed / fps).toFixed(1)} s`, v.project);
          void attempt(() => actions.viewerSeek(Math.min(r[0], Math.max(0, x.frames - 1))));
        });
        return true;
      }
      const s = trSel();
      if (s) {
        void strikeOrRestore(s.lo, s.hi).then(clearTrSel);
        return true;
      }
      // a zoom or layout section picked on its lane (decision 0046)
      const sec = selectedSection();
      if (sec) {
        void removeSection(sec);
        return true;
      }
      const p = selectedPart();
      if (p) {
        const n = v.parts.find((x) => x.id === p)?.n;
        void attempt(() => actions.deletePart(v.project, p)).then((ok) => ok && sayUndo(`Part ${n ?? ""} deleted`.replace("  ", " "), v.project));
        setSelectedPart(null);
      }
      return true;
    }
    case "v":
    case "V": {
      if (e.repeat) return true;
      const ed = nearestEdge(v.parts, f, selectedEdge());
      setSelectedEdge(ed);
      const p = ed ? v.parts.find((x) => x.id === ed.part) : undefined;
      if (ed && p) say(`Part ${p.n} ${ed.edge === "in" ? "start" : "end"} · ⌥← ⌥→ nudge it (⇧ ten frames)`);
      return true;
    }
    case ",":
    case ".":
      if (e.repeat) return true;
      tapMark(k === "," ? "in" : "out", v.project, { cut: v.cut });
      return true;
    case "Escape":
      if (trSel()) {
        clearTrSel();
        return false;
      }
      if (selectedEdge()) {
        setSelectedEdge(null);
        return true;
      }
      if (markIn() !== null || markOut() !== null) {
        setMarkIn(null);
        setMarkOut(null);
        return true;
      }
      return leaveMode();
  }
  return false;
}
