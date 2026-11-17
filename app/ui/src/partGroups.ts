// Part groups and the timeline's multi-selection (decision 0037): ⌘-click a part adds it
// to the selection, ⇧-click selects the run of parts up to it; ⌘G groups the selection,
// ⌘⇧G ungroups it. A click on a grouped part selects its whole group, and a drag moves
// the selection (or the group) as one block.
import { createRoot, createSignal, on, createEffect } from "solid-js";
import { actions, type CutView } from "./actions";
import { attempt, cutView, say, selectedPart, setSelectedPart } from "./store";

/** The parts picked with ⌘- or ⇧-click (and whole groups), besides `selectedPart`. */
export const [pickedParts, setPickedParts] = createSignal<ReadonlySet<string>>(new Set());
// another cut: nothing picked
createRoot(() => createEffect(on(() => cutView()?.cut, () => setPickedParts(new Set<string>()), { defer: true })));

/** The cut's parts once each, in order (a part can show as several plan parts). */
function order(v: CutView): string[] {
  return [...new Set(v.parts.map((p) => p.id))];
}

/** `ids` with every other part of their groups, in the cut's order. */
export function withGroups(v: CutView, ids: Iterable<string>): string[] {
  const want = new Set(ids);
  const groups = new Set(v.parts.filter((p) => want.has(p.id) && p.group).map((p) => p.group!));
  return order(v).filter((id) => want.has(id) || v.parts.some((p) => p.id === id && p.group && groups.has(p.group)));
}

/** What is selected now: the picked parts, else the selected part, with their groups. */
export function selection(): string[] {
  const v = cutView();
  if (!v) return [];
  const picked = pickedParts();
  const sel = selectedPart();
  return withGroups(v, picked.size ? picked : sel ? [sel] : []);
}

/** A click on part `id`: `only` selects it (and its group), `toggle` (⌘) adds or drops
 * it, `run` (⇧) selects every part from the selected one to it. */
export function pickPart(id: string, how: "only" | "toggle" | "run") {
  const v = cutView();
  if (!v) return;
  if (how === "only") {
    setSelectedPart(id);
    setPickedParts(new Set(withGroups(v, [id])));
    return;
  }
  const now = new Set(selection());
  if (how === "toggle") {
    const group = withGroups(v, [id]);
    if (now.has(id)) for (const x of group) now.delete(x);
    else for (const x of group) now.add(x);
  } else {
    const ids = order(v);
    const a = ids.indexOf(selectedPart() ?? id);
    const b = ids.indexOf(id);
    for (const x of ids.slice(Math.min(a, b), Math.max(a, b) + 1)) now.add(x);
  }
  setPickedParts(new Set(withGroups(v, now)));
  setSelectedPart(now.has(id) ? id : ([...now][0] ?? null));
}

export function clearPicked() {
  if (pickedParts().size) setPickedParts(new Set<string>());
}

/** The selection's groups (none when nothing grouped is selected). */
function groupsOf(v: CutView, ids: string[]): Set<string> {
  return new Set(v.parts.filter((p) => ids.includes(p.id) && p.group).map((p) => p.group!));
}

/** ⌘G is offered: two or more parts selected, not already one group. */
export function canGroup(): boolean {
  const v = cutView();
  const ids = selection();
  if (!v || ids.length < 2) return false;
  const g = groupsOf(v, ids);
  return !(g.size === 1 && v.parts.filter((p) => ids.includes(p.id)).every((p) => p.group));
}

/** ⌘⇧G is offered: a grouped part is selected. */
export function canUngroup(): boolean {
  const v = cutView();
  return !!v && groupsOf(v, selection()).size > 0;
}

export async function groupSelected() {
  const v = cutView();
  if (!v || !canGroup()) return say("Select two or more parts (⌘-click or ⇧-click) to group them");
  const r = await attempt(() => actions.partsGroup(v.project, v.cut, selection()));
  if (r) say(`${r.parts.length} parts grouped: they move together · ⌘⇧G ungroups`);
}

export async function ungroupSelected() {
  const v = cutView();
  if (!v || !canUngroup()) return say("Select a grouped part to ungroup it");
  if (!(await attempt(() => actions.partsUngroup(v.project, v.cut, selection())))) return;
  // the parts are on their own again: only the one clicked stays selected
  setPickedParts(new Set<string>());
  say("Ungrouped · ⌘Z undoes");
}
