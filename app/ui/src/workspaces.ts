// Workspace layouts (AJ 2026-10-19): named arrangements of the cut view's panes the person
// saved (ui prefs `workspaces`). Since decision 0054 none are built in and none are saved
// from the View menu; the ones already saved are still listed there. Applying one sets its
// pane keys in one save, its side tab (an older tab name maps, `tabFrom`), and reading.
import type { Workspace } from "./actions";
import { reading, setReading, showTab, sideTab, tabFrom } from "./layout";
import { prefs, setPrefs } from "./prefs";

export const savedWorkspaces = (): Workspace[] => prefs.workspaces ?? [];

const PANE_KEYS = ["map_open", "side_open", "timeline_open", "words_open", "focus", "side_w", "map_w", "tl_h"] as const;

/** Applies a layout: its pane keys in one save, then its side tab and reading. */
export function applyWorkspace(w: Workspace) {
  const patch: Record<string, unknown> = {};
  for (const k of PANE_KEYS) if (w[k] !== undefined) patch[k] = w[k];
  if (w.reading) patch.focus = false;
  setPrefs(patch);
  if (w.side_tab) {
    const t = tabFrom(w.side_tab);
    showTab(t.tab, t.section);
  }
  if (w.reading !== undefined) setReading(w.reading);
}

/** Whether the panes are arranged as `w` says now (the View menu ticks it). */
export function isCurrent(w: Workspace): boolean {
  if (w.reading !== undefined && w.reading !== reading()) return false;
  if (reading()) return !!w.reading;
  if (w.side_tab && prefs.side_open && tabFrom(w.side_tab).tab !== sideTab()) return false;
  return PANE_KEYS.every((k) => w[k] === undefined || w[k] === prefs[k]);
}
