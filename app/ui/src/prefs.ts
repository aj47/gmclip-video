// The person's view choices (app settings, `ui-prefs.json` in app data; never profile
// fields): hover scrub, safe zones, the project map, the render stats line, which coach
// marks were seen, the cut view's panes and their sizes, the transcript's times and text
// sizes, the theme and the density. Read once at boot; a change shows at once and is
// saved behind it.
import { createStore } from "solid-js/store";
import { actions, type UiPrefs } from "./actions";

type Prefs = Required<UiPrefs>;

export const DEFAULTS: Prefs = {
  hover_scrub: true,
  safe_zones: false,
  map_open: true,
  render_stats: false,
  waveform: true,
  coach_seen: [],
  side_open: true,
  timeline_open: true,
  words_open: true,
  lanes_open: true,
  focus: false,
  side_w: 0,
  map_w: 0,
  tl_h: 0,
  tr_times: false,
  tr_size: 14,
  read_size: 20,
  theme: "system",
  density: "comfortable",
  open_panes: "last",
  workspaces: [],
  recording: false,
  recordings_folder: "",
  developer: false,
};

const [store, setStore] = createStore<Prefs>({ ...DEFAULTS });
export const prefs = store;

/** The panes every launch starts with, by `open_panes` (the saved ones for "last"). */
const OPEN_ALL: Partial<Prefs> = { map_open: true, side_open: true, timeline_open: true, words_open: true, focus: false };

export async function loadPrefs() {
  try {
    const p = await actions.uiPrefsGet();
    setStore({ ...DEFAULTS, ...p });
    if (store.open_panes === "all") setStore(OPEN_ALL);
    if (store.open_panes === "focus") setStore({ focus: true });
  } catch {
    // an older core: the defaults hold for this session
  }
  applyAppearance();
}

/** Sets one preference now and saves it (a failed save keeps the choice for the session). */
export function setPref<K extends keyof Prefs>(key: K, value: Prefs[K]) {
  setStore(key, value as never);
  void actions.uiPrefsSet({ [key]: value } as Partial<UiPrefs>).catch(() => undefined);
  if (key === "theme" || key === "density") applyAppearance();
}

/** Several at once (one save). */
export function setPrefs(patch: Partial<Prefs>) {
  setStore(patch);
  void actions.uiPrefsSet(patch).catch(() => undefined);
  if ("theme" in patch || "density" in patch) applyAppearance();
}

/** Shows a value without saving it (a splitter mid-drag); `setPref` saves the last one. */
export function previewPref<K extends keyof Prefs>(key: K, value: Prefs[K]) {
  setStore(key, value as never);
}

let systemWatch: MediaQueryList | null = null;
/** Theme and density onto <html> (`data-theme`, `data-density`); "system" follows macOS. */
export function applyAppearance() {
  const root = document.documentElement;
  const light = store.theme === "light" || (store.theme === "system" && matchMedia("(prefers-color-scheme: light)").matches);
  const was = root.dataset.theme;
  if (light) root.dataset.theme = "light";
  else delete root.dataset.theme;
  root.dataset.density = store.density;
  if (!systemWatch) {
    systemWatch = matchMedia("(prefers-color-scheme: light)");
    systemWatch.addEventListener("change", () => store.theme === "system" && applyAppearance());
  }
  // canvas code (the timeline) caches its colours: tell it they changed
  if (was !== root.dataset.theme) dispatchEvent(new Event("gmclip-theme"));
}

let lastKeyToggle = 0;
/** ⌥S: hover scrub on/off (the native View item sends the same as a menu event; the one
 * that follows the key within 400 ms is the same press, so it is ignored). */
export function toggleHoverScrub(from: "key" | "menu" | "ui" = "ui") {
  if (from === "menu" && performance.now() - lastKeyToggle < 400) return;
  if (from === "key") lastKeyToggle = performance.now();
  setPref("hover_scrub", !store.hover_scrub);
}

export function coachSeen(id: string): boolean {
  return store.coach_seen.includes(id);
}
export function markCoachSeen(id: string) {
  if (!coachSeen(id)) setPref("coach_seen", [...store.coach_seen, id].slice(-64));
}
