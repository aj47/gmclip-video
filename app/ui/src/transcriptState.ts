// What the transcript pane shares with the global keys (d/x, s, p, q/w, /): its data, the
// selected run, the hovered word, and a way to open its search.
import { createSignal } from "solid-js";
import { actions, type TranscriptData } from "./actions";
import { attempt, cutView, say } from "./store";

export const [trData, setTrData] = createSignal<TranscriptData | null>(null);
/** The Words panel keeps the playhead's word in view (on; ⌘K turns it off). */
export const [trFollow, setTrFollow] = createSignal(true);
export const [trAnchor, setTrAnchor] = createSignal<number | null>(null);
export const [trFocus, setTrFocus] = createSignal<number | null>(null);
export const [trHover, setTrHover] = createSignal<number | null>(null);
/** The word being re-spelled (double-click, `f`). */
export const [respelling, setRespelling] = createSignal<number | null>(null);
/** Word-edge tuning step in ms (1/5/10/25/50/100). */
export const EDGE_STEPS = [1, 5, 10, 25, 50, 100];
export const [edgeStep, setEdgeStep] = createSignal(10);
/** The word last tuned (its readout shows in the pane's bar). */
export const [tuning, setTuning] = createSignal<number | null>(null);
/** The video whose words the pane shows (the source picker): null = the cut's first
 * video's. The data's `owner` says whose words they are; edits name it. */
export const [trOwner, setTrOwner] = createSignal<string | null>(null);
/** Set to open the pane's search with this text (the pane clears it). */
export const [findRequest, setFindRequest] = createSignal<string | null>(null);

export function trSel(): { lo: number; hi: number } | null {
  const a = trAnchor();
  const f = trFocus();
  return a === null || f === null ? null : { lo: Math.min(a, f), hi: Math.max(a, f) };
}

export function clearTrSel() {
  setTrAnchor(null);
  setTrFocus(null);
}

/** The in-cut word showing at output frame `f` (the last one starting at or before it). */
export function wordAtFrame(f: number): number {
  const t = trData();
  if (!t) return -1;
  let best = -1;
  let bestF = -1;
  for (let i = 0; i < t.frame.length; i++) {
    const wf = t.frame[i]!;
    if (wf >= 0 && wf <= f && wf >= bestF) {
      best = i;
      bestF = wf;
    }
  }
  return best;
}

/** Frames per second of the transcript's cut. */
export function trFps(): number {
  return trData()?.fps ?? 30;
}

/** `[` `]` `{` `}`: moves a word's start or end by the step (SPEC §5.4). */
export async function tuneEdge(i: number, edge: "start" | "end", sign: 1 | -1): Promise<void> {
  const v = cutView();
  if (!v || i < 0) return;
  setTuning(i);
  await attempt(() => actions.nudgeWord(v.project, v.cut, i, edge, sign * edgeStep(), trData()?.owner));
}

/** Cuts words `lo..=hi` out of the cut, or puts them back when they are all cut already. */
export async function strikeOrRestore(lo: number, hi: number): Promise<void> {
  const v = cutView();
  const t = trData();
  if (!v || !t) return;
  let all = true;
  for (let i = lo; i <= hi; i++) if (t.frame[i]! >= 0) all = false;
  if (all) {
    await attempt(() => actions.restoreWords(v.project, v.cut, lo, hi, t.owner));
    return;
  }
  const r = await attempt(() => actions.strikeWords(v.project, v.cut, lo, hi, t.owner));
  if (r === undefined) return;
  // a piece left under 0.12 s beside the words cannot be a part: it went with them
  const dropped = r.output?.fragments_dropped ?? 0;
  const also = dropped ? ` and ${dropped} piece${dropped === 1 ? "" : "s"} too short to keep` : "";
  say(`Cut ${hi - lo + 1} word${hi === lo ? "" : "s"}${also} — ⌘Z puts them back`);
}
