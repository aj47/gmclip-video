// The cut view's panes (AJ 2026-10-16: "hide the sidebar, the ui should be very
// configurable and the window should be resizable"). Every pane — the project map, the
// side panel (Words, Look, Captions), the words strip and the timeline — shows or hides
// from the View menu, a key, or its own button, in every layout. Focus keeps only the
// viewer and the timeline; reading puts the whole transcript beside the viewer as prose.
// What shows is saved (prefs); the compact layout's drawers are for this session only
// (they open over the viewer, so they never open by themselves).
import { createSignal } from "solid-js";
import { prefs, setPref, setPrefs } from "./prefs";
import { mapDrawer, setMapDrawer } from "./store";

export type CutLayout = "wide" | "medium" | "compact" | "portrait";

/** wide ≥ 1280 · medium 900–1279 · compact < 900 (landscape, and small windows under 560
 * px tall) · portrait: taller than wide, or narrow with height to stack in. */
export function measureLayout(w = innerWidth, h = innerHeight): CutLayout {
  if (h > w * 1.1 || (w < 640 && h >= 560)) return "portrait";
  if (w < 900) return "compact";
  return w >= 1280 ? "wide" : "medium";
}

const [layout, setLayout] = createSignal<CutLayout>(measureLayout());
export { layout };
addEventListener("resize", () => {
  const l = measureLayout();
  if (l === layout()) return;
  // a drawer never carries over into another layout
  setSideDrawer(false);
  setMapDrawer(false);
  setLayout(l);
});

/** The side panel's tabs (decision 0054): Words (the transcript, Tighten inside it) and
 * Captions are the bar; the rest are "More panels" — in ⌘K, and in the bar only while one
 * is open (or with the developer setting). Look (layout and the part's inspector) left the
 * bar on 2026-11-22 (AJ never used it): View › Look, ⌥I, ⌘K and a double-click on a layer
 * still open it, and the agent's layout commands are unchanged. */
export type SideTab = "words" | "look" | "captions" | "story" | "polish" | "thumbnail" | "review" | "agent";
export const DEFAULT_TABS: SideTab[] = ["words", "captions"];
/** Off the bar but named in the View menu (the one quiet place it lives). */
export const VIEW_TABS: SideTab[] = [...DEFAULT_TABS, "look"];
// The panel opens on Words: the transcript lives in one place, the Words tab (AJ
// 2026-11-22: the words strip under the picture is retired, the picture gets its room).
const [sideTab, setSideTab] = createSignal<SideTab>("words");
export { sideTab, setSideTab };

/** The tabs the bar shows: the default three, every one with the developer setting, and
 * a "More panels" one while it is open. */
export function barTabs(): SideTab[] {
  if (prefs.developer) return SIDE_TABS.map((t) => t.id);
  return DEFAULT_TABS.includes(sideTab()) ? DEFAULT_TABS : [...DEFAULT_TABS, sideTab()];
}

/** Tighten (pauses, fillers, retakes) lives in the cut's status dropdown (AJ 2026-11-22:
 * one Tighten), folded until asked for; `statusOpen` is that dropdown. */
export const [tightenOpen, setTightenOpen] = createSignal(false);
export const [statusOpen, setStatusOpen] = createSignal(false);
/** Opens the status dropdown on Tighten (⌘K, the "tighten" section of an old layout). */
export function openTighten() {
  setTightenOpen(true);
  setStatusOpen(true);
}

/** A tab by an older name (a saved layout from before decision 0054, a key, a link):
 * the merged ones go to their new tab, and a tab that left the bar opens Words. */
export function tabFrom(name: string): { tab: SideTab; section?: string } {
  switch (name) {
    case "transcript":
      return { tab: "words" };
    case "tighten":
      return { tab: "words", section: "words-tighten" };
    case "inspector":
    case "layout":
    case "look":
      return { tab: "look" };
    case "short":
      return { tab: "captions" };
  }
  return { tab: DEFAULT_TABS.includes(name as SideTab) ? (name as SideTab) : "words" };
}
/** The words strip at reading height (its expand button; this session only). */
export const [wordsTall, setWordsTall] = createSignal(false);

/** The compact layout's side drawer (session only). */
export const [sideDrawer, setSideDrawer] = createSignal(false);

/** Reading: the whole transcript as prose in a large pane beside a smaller viewer. */
const [reading, setReadingRaw] = createSignal(false);
export { reading };
export function setReading(on: boolean) {
  if (on) {
    setSideTab("words");
    if (prefs.focus) setPref("focus", false);
  }
  setReadingRaw(on);
}

export type PaneId = "map" | "side" | "timeline" | "words";

/** Whether a pane shows now (its saved choice, the layout, focus and reading). */
export function paneShown(id: PaneId): boolean {
  const l = layout();
  if (reading()) return id === "side";
  if (prefs.focus && id !== "timeline") return false;
  switch (id) {
    case "map":
      return l === "wide" ? prefs.map_open : mapDrawer();
    case "side":
      return l === "compact" ? sideDrawer() : prefs.side_open;
    case "timeline":
      return prefs.timeline_open;
    case "words":
      // the words strip is retired (AJ 2026-11-22): the transcript is the Words tab only.
      // WordsStrip stays in the code; this is the one switch that brings it back.
      return false;
  }
}

/** Is the pane a drawer over the viewer in this layout (rather than docked)? */
export function paneIsDrawer(id: PaneId): boolean {
  return (id === "map" && layout() !== "wide") || (id === "side" && layout() === "compact" && !reading());
}

export function togglePane(id: PaneId) {
  const on = paneShown(id);
  if (!on && (prefs.focus || reading())) {
    // asking for a pane leaves focus / reading; when that shows it, that is the answer
    if (reading()) setReadingRaw(false);
    if (prefs.focus) setPref("focus", false);
    if (paneShown(id)) return;
  }
  switch (id) {
    case "map":
      if (layout() === "wide") setPref("map_open", !on);
      else setMapDrawer(!on);
      break;
    case "side":
      if (layout() === "compact") setSideDrawer(!on);
      else setPref("side_open", !on);
      break;
    case "timeline":
      setPref("timeline_open", !on);
      break;
    case "words":
      // ⌥4 (the strip's key) now shows the transcript where it lives: the Words tab
      toggleTab("words");
      break;
  }
}

/** A side-panel tab as its own pane: shows the panel on that tab, or hides it when the
 * tab is already showing. */
export function tabShown(t: SideTab): boolean {
  return paneShown("side") && sideTab() === t;
}
export function toggleTab(t: SideTab) {
  if (tabShown(t) && !reading()) {
    togglePane("side");
    return;
  }
  if (reading()) setReadingRaw(false);
  setSideTab(t);
  if (!paneShown("side")) togglePane("side");
}

/** Shows a side-panel tab (leaving reading and focus), never hides it: ⌘K and the
 * buttons that send you to a panel. `section` is scrolled into view once it is there
 * ("words-tighten" opens the status dropdown's Tighten instead). */
export function showTab(t: SideTab, section?: string) {
  if (section === "words-tighten") return openTighten();
  if (reading()) setReadingRaw(false);
  setSideTab(t);
  if (!paneShown("side")) togglePane("side");
  if (section) {
    let tries = 0;
    const find = () => {
      const el = document.querySelector<HTMLElement>(`.side [data-testid="${section}"]`);
      if (el) el.scrollIntoView({ block: "start" });
      else if (tries++ < 20) setTimeout(find, 50);
    };
    queueMicrotask(find);
  }
}

/** The side-panel tabs: their names and icons (the bar, View menu, ⌘K), the bar's two
 * first, then Look. */
export const SIDE_TABS: { id: SideTab; label: string; icon: string; what: string }[] = [
  { id: "words", label: "Words", icon: "file-text", what: "the transcript, search, topics; tighten pauses and fillers" },
  { id: "captions", label: "Captions", icon: "captions", what: "captions, hook and hook lines, openings, pills, run clock, end card short" },
  { id: "look", label: "Look", icon: "sliders", what: "layout: crops, insets, presets; this part: crop, gain, speed and ramp, sound, images, plates, backdrop, covers inspector" },
  { id: "story", label: "Story", icon: "book", what: "grow the cut into a story" },
  { id: "polish", label: "Polish", icon: "sparkles", what: "stressed words, punch-ins, callouts, loop, screen holds" },
  { id: "thumbnail", label: "Thumbnail", icon: "image", what: "covers, titles and descriptions" },
  { id: "review", label: "Review", icon: "message-square", what: "comments and suggested edits" },
  { id: "agent", label: "Agent", icon: "bot", what: "run your agent on this cut" },
];

export function toggleFocus() {
  if (reading()) setReadingRaw(false);
  setPref("focus", !prefs.focus);
}

export function toggleReading() {
  setReading(!reading());
}

/** Every pane back, at the layout's own sizes. */
export function resetLayout() {
  setReadingRaw(false);
  setSideDrawer(false);
  setMapDrawer(false);
  setPrefs({ map_open: true, side_open: true, timeline_open: true, words_open: true, focus: false, side_w: 0, map_w: 0, tl_h: 0 });
}
