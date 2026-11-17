// Shared UI state of "Ideas 2026-10-16 (library and layout)" (lane K4): the "Draw your
// layout once" sheet (which video of which project) and the long-form comparison.
import { createSignal } from "solid-js";

/** The draw-your-layout sheet: open on this video of this project. */
export const [drawLayout, setDrawLayout] = createSignal<{ project: string; source: string } | null>(null);
/** The long-form comparison: open with these two long-forms (a, b) of this project. */
export const [compareLongs, setCompareLongs] = createSignal<{ project: string; a: string; b: string } | null>(null);

/** Compare: this long-form and the next one, else the newest two. */
export function compareLongsFor(project: string, longs: { cut: string }[], current: string | undefined) {
  if (longs.length < 2) return;
  const at = longs.findIndex((l) => l.cut === current);
  const a = at >= 0 ? longs[at]! : longs[longs.length - 2]!;
  const b = at >= 0 ? longs[(at + 1) % longs.length]! : longs[longs.length - 1]!;
  setCompareLongs({ project, a: a.cut, b: b.cut });
}
