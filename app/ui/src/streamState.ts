// The stream edit's shared UI state (decision 0011): the topic/fluff/coverage overlay of
// the cut on screen, the project map, a selection waiting for a cut to open (⌘↑ "Show in
// stream" / ⌘↓ "Shorts here"), the cuts a "Shorts here" lit up, and the label popover.
import { createSignal } from "solid-js";
import { actions, type ProjectMapData, type StreamOverlay, type TopicBand } from "./actions";
import { attempt, cutView, openCut, say } from "./store";
import { setTrAnchor, setTrFocus, trData, trHover, trSel } from "./transcriptState";

const [overlayAll, setOverlay] = createSignal<StreamOverlay | null>(null);
/** The overlay of every video the cut plays (the topic lane spans them all). */
export { overlayAll, setOverlay };
/** The overlay on the transcript shown: its video's topics, fluff and coverage (a cut
 * may play several videos, each word ordinal in its own transcript; decision 0036). */
export const overlay = (): StreamOverlay | null => {
  const o = overlayAll();
  const own = trData()?.owner;
  if (!o || !own) return o;
  const mine = (x: { owner?: string }) => !x.owner || x.owner === own;
  return { ...o, topics: o.topics.filter(mine), fluff: o.fluff.filter(mine), coverage: o.coverage.filter(mine) };
};
export const [projectMapData, setProjectMapData] = createSignal<ProjectMapData | null>(null);
/** Words to select once `cut` is on screen (a jump between levels). */
export const [pendingSel, setPendingSel] = createSignal<{ cut: string; lo: number; hi: number } | null>(null);
/** A topic to select and scroll to once the stream edit's overlay shows it. */
export const [pendingTopic, setPendingTopic] = createSignal<string | null>(null);
/** Cuts a "Shorts here" found (the map highlights them). */
export const [lit, setLit] = createSignal<string[]>([]);
/** The `t` popover: open over this word range. */
export const [labelAsk, setLabelAsk] = createSignal<{ lo: number; hi: number } | null>(null);
/** Recent topic titles (this session, newest first). */
export const [recentTitles, setRecentTitles] = createSignal<string[]>([]);
/** "Make long-form from topics…" sheet. */
export const [longformOpen, setLongformOpen] = createSignal(false);

export function noteTitle(t: string) {
  setRecentTitles((r) => [t, ...r.filter((x) => x !== t)].slice(0, 8));
}

export async function refreshOverlay() {
  const v = cutView();
  if (!v) return;
  const o = await attempt(() => actions.streamOverlay(v.project, v.cut));
  if (o && cutView()?.cut === v.cut) setOverlay(o);
}

export async function refreshMap(project: string) {
  const m = await attempt(() => actions.projectMap(project));
  if (m) setProjectMapData(m);
}

/** The topic holding word `i`, if any (accepted ones first). */
export function topicAt(i: number): TopicBand | undefined {
  const ts = overlay()?.topics ?? [];
  return ts.find((t) => t.state === "accepted" && t.w0 <= i && i <= t.w1) ?? ts.find((t) => t.w0 <= i && i <= t.w1);
}

/** The words a level jump carries: the selection, else the hovered word. */
function carried(): { lo: number; hi: number } | null {
  const s = trSel();
  if (s) return s;
  const h = trHover();
  return h === null ? null : { lo: h, hi: h };
}

/** ⌘↑ "Show in stream": the same words in the stream edit (same source, same ordinals). */
export async function showInStream() {
  const v = cutView();
  const m = projectMapData();
  const s = carried();
  if (!v || !m?.stream) {
    say("This project has no stream edit yet");
    return;
  }
  if (v.cut === m.stream.cut) return;
  if (s) setPendingSel({ cut: m.stream.cut, lo: s.lo, hi: s.hi });
  await openCut(v.project, m.stream.cut);
}

/** ⌘↓ "Shorts here": the cuts that play the selected words; one opens, several light up. */
export async function shortsHere() {
  const v = cutView();
  const s = carried();
  const o = overlay();
  if (!v || !s || !o) {
    say("Select words first");
    return;
  }
  const ids = [...new Set(o.coverage.filter((c) => c.kind !== "stream" && c.w0 <= s.hi && s.lo <= c.w1).map((c) => c.cut))];
  setLit(ids);
  if (!ids.length) say("No long-form or short uses these words");
  else if (ids.length === 1) {
    setPendingSel({ cut: ids[0]!, lo: s.lo, hi: s.hi });
    await openCut(v.project, ids[0]!);
  } else say(`${ids.length} cuts use these words — lit up in the map`);
}

/** Applies a pending selection once the cut it is for shows its transcript. */
export function applyPending(): boolean {
  const p = pendingSel();
  const v = cutView();
  const t = trData();
  if (!p || !v || p.cut !== v.cut || !t || !t.text.length) return false;
  setPendingSel(null);
  setTrAnchor(p.lo);
  setTrFocus(p.hi);
  return true;
}
