// The View menu (top bar, in the cut view): every pane with a tick and its key, the
// side panel's tabs (Words, Look, Captions; every one with the developer setting), focus,
// the viewer's own view toggles (hover scrub, safe zones), the layouts the person saved
// before decision 0054 (none are built in, and none are saved from here now), and Reset
// layout. Reading (⌥R) is in ⌘K and the Words tab. The keys are in keys.ts (VIEW_KEYS)
// and the ? list.
import { Menu, type MenuItem } from "../design/components";
import { applyWorkspace, isCurrent, savedWorkspaces } from "../workspaces";
import { DEFAULT_TABS, paneShown, reading, resetLayout, SIDE_TABS, tabShown, toggleFocus, togglePane, toggleTab } from "../layout";
import { prefs, setPref, toggleHoverScrub } from "../prefs";
import { setRoute } from "../store";

export function viewItems(): MenuItem[] {
  return [
    { label: "Project map", icon: "panel-left", checked: paneShown("map"), keys: "⌥1", testid: "view-map", onSelect: () => togglePane("map") },
    { label: "Side panel", icon: "panel-right", checked: paneShown("side"), keys: "⌥2", testid: "view-side", onSelect: () => togglePane("side") },
    { label: "Words strip", icon: "type", checked: paneShown("words"), keys: "⌥4", testid: "view-words", onSelect: () => togglePane("words") },
    { label: "Timeline", icon: "panel-bottom", checked: paneShown("timeline"), keys: "⌥3", testid: "view-timeline", onSelect: () => togglePane("timeline") },
    { separator: true, label: "" },
    // the side panel's tabs by name (the rest are ⌘K's "More panels")
    ...SIDE_TABS.filter((t) => prefs.developer || DEFAULT_TABS.includes(t.id)).map((t): MenuItem => ({
      label: t.label,
      icon: t.icon,
      checked: tabShown(t.id),
      keys: t.id === "words" ? "⌥T" : t.id === "look" ? "⌥I" : undefined,
      testid: `view-tab-${t.id}`,
      onSelect: () => toggleTab(t.id),
    })),
    { separator: true, label: "" },
    { label: "Focus: viewer and timeline", icon: "focus", checked: prefs.focus && !reading(), keys: "⌥F", testid: "view-focus", onSelect: toggleFocus },
    { separator: true, label: "" },
    { label: "Hover scrub", icon: "scrub", checked: prefs.hover_scrub, keys: "⌥S", testid: "view-hover-scrub", onSelect: () => toggleHoverScrub() },
    { label: "Safe zones", icon: "safe-zone", checked: prefs.safe_zones, testid: "view-safe-zones", onSelect: () => setPref("safe_zones", !prefs.safe_zones) },
    { separator: true, label: "" },
    // the person's own saved layouts, if any
    ...savedWorkspaces().map((w): MenuItem => ({
      label: `Layout: ${w.name}`,
      icon: "layout",
      checked: isCurrent(w),
      testid: `view-layout-${w.name.toLowerCase().replace(/[^a-z0-9]+/g, "-")}`,
      onSelect: () => applyWorkspace(w),
    })),
    ...(savedWorkspaces().length ? [{ separator: true, label: "" }] : []),
    { label: "Reset layout", icon: "restore", keys: "⌥0", testid: "view-reset", onSelect: resetLayout },
    { label: "Appearance and layout settings…", icon: "settings", testid: "view-settings", onSelect: () => setRoute({ view: "settings" }) },
  ];
}

export function ViewMenu() {
  return <Menu label="View" text="View" icon="layout" align="start" testid="view-menu" items={viewItems} />;
}

