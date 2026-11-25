// App-wide UI state: fine-grained signals fed by calls and subscriptions (SPEC §12.4).
// No model math here: everything is read from the core's replies. Lists from the core
// are reconciled by id, so a refresh keeps rows (and focus) that did not change.
import { createSignal } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { actions, type CutView, type Home, type ImportInspect, type ImportMany, type JobView, type ProjectInfo, type ViewerState } from "./actions";
import type { ManyAnswer } from "./components/ImportManySheet";
import { CallError, editsAnswered, onMutation, seekReported, subscribe } from "./ipc";
import { agentEditing, noteCommits, onSettled } from "./presence";
/** The event plumbing, for state modules beside this one (agentsState), and the call error
 * a view reads (History). */
export { CallError, subscribe };
import { loadPrefs, prefs, setPref, toggleHoverScrub } from "./prefs";
import { openRecording } from "./recordingState";

export type Route =
  | { view: "home" }
  | { view: "cut"; project: string; cut: string }
  | { view: "settings" }
  | { view: "search"; q?: string; project?: string }
  | { view: "agents" };

export const [route, setRoute] = createSignal<Route>({ view: "home" });
export const [projects, setProjects] = createSignal<ProjectInfo[]>([]);
const [cutStore, setCutStore] = createStore<{ v: CutView | null }>({ v: null });
export const cutView = () => cutStore.v;
export function setCutView(v: CutView | null) {
  setCutStore("v", reconcile(v, { key: "id", merge: false }));
}
const [homeStore, setHomeStore] = createStore<Home>({ running: [], needs_you: [], done: [] });
export const home = () => homeStore;
const [viewerRaw, setViewerRaw] = createSignal<ViewerState | null>(null);
export function setViewer(v: ViewerState) {
  setViewerRaw(v);
  seekReported(v.frame);
}
/** The viewer's state, only while it shows the cut on screen (never a previous cut's). */
export const viewer = () => {
  const v = viewerRaw();
  const r = route();
  if (!v) return null;
  if (r.view === "cut") return v.cut === r.cut ? v : null;
  // library search plays a hit's span (lane L)
  if (r.view === "search") return v.cut?.startsWith("span:") ? v : null;
  return null;
};
/** The viewer's state while it shows the stream around the open cut (the run viewer). */
export const streamViewer = () => {
  const v = viewerRaw();
  const r = route();
  return v && r.view === "cut" && v.cut === `stream:${r.cut}` ? v : null;
};
/** ⌘E / File › Export asks the cut view to export (it runs the term gate first). */
export const [exportAsk, setExportAsk] = createSignal(0);
export const [jobs, setJobs] = createSignal<JobView[]>([]);
/** A toast; `undo` (a project) adds an Undo button that undoes the project's last step;
 * `act` adds a button that does something else (e.g. "Draw layout"). */
export const [toast, setToast] = createSignal<{ text: string; kind: "error" | "info"; undo?: string; act?: { label: string; run: () => void } } | null>(null);
export const [dropped, setDropped] = createSignal<string[]>([]);
export const [saved, setSaved] = createSignal<"saved" | "saving" | "failed">("saved");
export const [selectedPart, setSelectedPart] = createSignal<string | null>(null);
/** The timeline's horizontal mapping (x = 12 + frame · ppf − scroll, CSS px from its
 * lanes' left edge), so lanes drawn outside its canvas (the topic lane) follow its zoom. */
export const [tlView, setTlView] = createSignal<{ ppf: number; scroll: number } | null>(null);
/** Set while a panel wants a dropped video (the caption matcher): a dropped video goes
 * there instead of opening. */
/** Crop mode asked for from outside the cut view (a double-click on a layer: its part, or
 * the part under the playhead when `part` is null). */
export const [cropAsk, setCropAsk] = createSignal<{ part: string | null; role: string | null; n: number } | null>(null);
/** `role`: the layer to put in focus (else the first). */
export function askCrop(part: string | null, role: string | null = null) {
  setCropAsk({ part, role, n: (cropAsk()?.n ?? 0) + 1 });
}
/** Layers grouped on the timeline's layer rows (AJ 2026-10-20: "group them and change the
 * crop for multiple cameras … and positioning"): model `(part, layer)`s. A crop or place
 * written on one of them is written on all of them (one undo). */
export type GroupMember = { part: string; layer: string };
export const [layerGroup, setLayerGroupRaw] = createSignal<GroupMember[]>([]);
export function setLayerGroup(g: GroupMember[]) {
  setLayerGroupRaw(g);
  // the preview's drags commit through the viewer, so it holds the group too
  void attempt(() => actions.viewerGroup(g));
}
/** The group to send with a write on `(part, layer)`: the group when that layer is in it. */
export const groupFor = (part: string, layer: string | null): GroupMember[] | undefined => {
  const g = layerGroup();
  return layer && g.length > 1 && g.some((m) => m.part === part && m.layer === layer) ? g : undefined;
};
/** An added video or image picked on the timeline's layer rows: its box on the preview
 * takes a drag anywhere in it (else only by its tag). */
export const [selectedOverlay, setSelectedOverlay] = createSignal<string | null>(null);
export const [videoDropHook, setVideoDropHook] = createSignal<((path: string) => void) | null>(null);
/** The timeline's drop target (decision 0042): files dropped on it at `at` (CSS px of the
 * window) become picture layers at that time; it answers whether the drop was its. */
export const [timelineDropHook, setTimelineDropHook] = createSignal<((paths: string[], at: [number, number]) => boolean) | null>(null);
/** Spans a sweep would cut (output frames), drawn on the timeline while previewing. */
export const [sweepRibbons, setSweepRibbons] = createSignal<[number, number][]>([]);
/** The speech model download (first run), from the `app` scope. */
export const [modelsProgress, setModelsProgress] = createSignal<{ state: string; done?: number; total?: number; error?: string } | null>(null);
/** Bumps when an agent run starts, moves or ends. */
export const [agentTick, setAgentTick] = createSignal(0);
/** The export that just finished (the "Review it" banner after an export). */
export const [justExported, setJustExported] = createSignal<{ project: string; title: string; export: string | null } | null>(null);
/** Bumps when something Home lists changed (a project opened, a video added), by anyone. */
export const [homeTick, setHomeTick] = createSignal(0);
/** Bumps when a project's analysis moves (words arrived, phase changed). */
export const [analysisTick, setAnalysisTick] = createSignal(0);
/** Bumps when a cut-out's accurate mask tracks start or finish building (R.01). */
export const [mattesTick, setMattesTick] = createSignal(0);
/** Bumps when a scrub proxy covers more of a source (the filmstrip asks again). */
export const [proxyTick, setProxyTick] = createSignal(0);
/** The output frame under the pointer on the timeline, and when the pointer left it
 * (null while it is over it): `s`, `q` and `w` cut there rather than at the playhead. */
export const [tlHover, setTlHover] = createSignal<{ frame: number; left: number | null } | null>(null);
/** The timeline's hover frame while the pointer is on it or left it under 3 s ago. */
export const tlHoverFrame = (): number | null => {
  const h = tlHover();
  return h && (h.left === null || performance.now() - h.left < 3000) ? h.frame : null;
};
/** A measurement being marked (`m` at its start, output frame), until `⇧m` ends it. */
export const [measureFrom, setMeasureFrom] = createSignal<number | null>(null);
/** The marked range (output frames): I marks its in, O its out (also ⇧-click on the
 * timeline); ⌥L loops it and ⇧⌫ ripple-deletes it. Session-only, cleared with the cut. */
export const [markIn, setMarkIn] = createSignal<number | null>(null);
export const [markOut, setMarkOut] = createSignal<number | null>(null);
/** The range when both marks are set, in order. */
export function markedRange(): [number, number] | null {
  const a = markIn();
  const b = markOut();
  return a === null || b === null || a === b ? null : [Math.min(a, b), Math.max(a, b)];
}
/** The part edge the keyboard nudges (V picks the one nearest the playhead; ⌥← / ⌥→
 * move it a frame, ⇧ ten). */
export const [selectedEdge, setSelectedEdge] = createSignal<{ part: string; edge: "in" | "out" } | null>(null);
/** The transport's loop button asks the timeline to loop its range, else the part (⌥L). */
export const [loopAsk, setLoopAsk] = createSignal(0);
/** The project map as a drawer (windows under 1280 px), for this session only. */
export const [mapDrawer, setMapDrawer] = createSignal(false);
/** Timeline tools: the blade splits where you click; snapping off is ⌥ held for good. */
export const [tlTool, setTlTool] = createSignal<"select" | "blade">("select");
export const [snapping, setSnapping] = createSignal(true);
/** Bumps when the timeline's lanes (overlays, gains, measurements) changed outside the cut's plan. */
export const [lanesTick, setLanesTick] = createSignal(0);

const VIDEO_EXT = /\.(mp4|mov|m4v|mkv|webm)$/i;
const IMAGE_EXT = /\.(png|jpe?g)$/i;
/** A recording's `session.json` (decision 0045): its folder is the session. */
const SESSION_FILE = /[\\/]session\.json$/i;

/** The project a view is on, if any. */
export function currentProject(): string | null {
  const r = route();
  return r.view === "cut" ? r.project : null;
}

export const [saveError, setSaveError] = createSignal<string | null>(null);
/** The keys list (?), opened from the keyboard, the top bar or the View menu. */
export const [helpOpen, setHelpOpen] = createSignal(false);
/** The History dialog (with why it opened), and the ⌘K palette. */
export const [historyOpen, setHistoryOpen] = createSignal<{ reason?: string; nameFirst?: boolean } | null>(null);
/** R-HIST-1: an undo/redo of an earlier session's edit waiting for the human's yes. */
export const [undoAsk, setUndoAsk] = createSignal<{ project: string; redo: boolean; command: string; ts: number } | null>(null);

/** ⌘Z / ⌘⇧Z and Edit › Undo: the human's own stack. The first one after a reopen asks
 * once (the core answers `needs_confirm`); with nothing left to undo in a cut, the
 * history opens instead. */
/** Commits on their way that have not reached `call` yet (a gesture's release waits for
 * its hold's id first): ⌘Z waits for them too. */
const pendingEdits = new Set<Promise<unknown>>();
/** Marks `p` as an edit on its way until it settles; returns it. */
export function editing<T>(p: Promise<T>): Promise<T> {
  pendingEdits.add(p);
  void p.finally(() => pendingEdits.delete(p));
  return p;
}

export async function undoRedo(project: string, redo: boolean, inCut: boolean) {
  // ⌘Z right after a release undoes that release: wait for the edits on their way (sent
  // after them, the undo landed first and undid the edit before, lanes.spec's gain step)
  await Promise.race([Promise.allSettled([...pendingEdits]), new Promise((d) => setTimeout(d, 10_000))]);
  await editsAnswered();
  try {
    const r = redo ? await actions.redo(project) : await actions.undo(project);
    // the call returns the command's output itself: `{needs_confirm, command, ts}` asks
    if (r && "needs_confirm" in r) setUndoAsk({ project, redo, command: r.command || "edit", ts: r.ts || Date.now() });
  } catch (err: unknown) {
    const m = String((err as { message?: string })?.message ?? err);
    if (!redo && /nothing to undo/i.test(m) && inCut) {
      setHistoryOpen({ reason: "Nothing to undo in this session. Restore an earlier version of this cut instead?" });
    } else {
      say(m, "error");
    }
  }
}
export const [paletteOpen, setPaletteOpen] = createSignal(false);

let saveTimer: ReturnType<typeof setTimeout> | undefined;
/** Asks the core whether everything committed is durable (Saved / Saving… / Not saved). */
async function checkSave() {
  clearTimeout(saveTimer);
  const p = currentProject();
  if (!p) {
    setSaved("saved");
    return;
  }
  try {
    const s = await actions.saveState(p);
    setSaved(s.state);
    setSaveError(s.error);
    if (s.state === "saving") saveTimer = setTimeout(() => void checkSave(), 400);
    // R-ERR-2: the core retries a failed save every second; follow it until it clears
    else if (s.state === "failed") saveTimer = setTimeout(() => void checkSave(), 1000);
  } catch {
    // the project closed; nothing to report
  }
}

let pendingMutations = 0;
onMutation((phase) => {
  pendingMutations = Math.max(0, pendingMutations + (phase === "start" ? 1 : -1));
  if (phase === "start") setSaved("saving");
  else if (pendingMutations === 0) void checkSave();
});

/** ⌘S: everything durable now, and a restore point to come back to. */
export async function saveRestorePoint() {
  const p = currentProject();
  if (!p) return;
  const r = await attempt(() => actions.restorePoint(p));
  if (r) say(r.error ? `Not saved: ${r.error}` : "Saved — restore point made", r.error ? "error" : "info");
  void checkSave();
}

let lastKeyUndo = 0;
/** The keyboard's ⌘Z/⌘⇧Z ran (the menu's matching event is then ignored). */
export function noteKeyUndo() {
  lastKeyUndo = performance.now();
}

/** File and Edit menu items (the native menu sends them as `app` events; ⌘K reuses them). */
export async function onMenu(id: string) {
  const p = currentProject();
  switch (id) {
    case "open_video":
      await openVideo();
      break;
    case "add_video":
      await addVideosPick();
      break;
    case "save":
      await saveRestorePoint();
      break;
    case "export": {
      // the cut view exports (through the term gate)
      if (cutView()) setExportAsk(exportAsk() + 1);
      else say("Open a cut to export it");
      break;
    }
    case "undo":
    case "redo":
      // Edit › Undo Last Edit (mouse): the project's undo. The keyboard's ⌘Z never
      // arrives here (it is the system item; a focused field undoes its own typing).
      if (performance.now() - lastKeyUndo < 400) return;
      if (p) await undoRedo(p, id === "redo", route().view === "cut");
      break;
    case "keys":
      setHelpOpen(true);
      break;
    case "hover_scrub":
      toggleHoverScrub("menu");
      break;
    case "safe_zones":
      setPref("safe_zones", !prefs.safe_zones);
      break;
  }
}

let toastTimer: ReturnType<typeof setTimeout> | undefined;
export function say(text: string, kind: "error" | "info" = "info") {
  setToast({ text, kind });
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => setToast(null), kind === "error" ? 8000 : 3500);
}

/** A toast with one action button (shown a little longer, so it can be reached). */
export function sayAct(text: string, label: string, run: () => void) {
  setToast({ text, kind: "info", act: { label, run } });
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => setToast(null), 9000);
}

/** A toast with Undo (SPEC §5.3: delete ⌫ shows an undo toast, never a confirm). */
export function sayUndo(text: string, project: string) {
  setToast({ text, kind: "info", undo: project });
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => setToast(null), 6000);
}

/** Runs an action, showing its error in plain words. */
export async function attempt<T>(f: () => Promise<T>): Promise<T | undefined> {
  try {
    return await f();
  } catch (e) {
    const text = e instanceof CallError ? e.error.message : String(e);
    // an agent mid-edit passes through cuts that do not plan (decision 0059): what a read
    // of such a cut answers is not said while the agent works, only if it is left so
    if (agentEditing() && PLAN_ERROR.test(text)) heldPlanError = text;
    else say(text, "error");
    return undefined;
  }
}
const PLAN_ERROR = /^(the cut does not plan yet|this cut has no framing to plan)/;
/** The last plan error held back while an agent was editing. */
let heldPlanError: string | null = null;

export async function refreshProjects() {
  const p = await attempt(actions.openProjects);
  if (p) setProjects(p);
}

export async function refreshHome() {
  const h = await attempt(actions.home);
  if (h) setHomeStore(reconcile(h, { key: "cut", merge: false }));
}

let cutGeneration = 0;
/** Why the cut on screen cannot be shown (it does not plan), instead of "Loading…". */
export const [cutError, setCutError] = createSignal<string | null>(null);

export async function refreshCut() {
  const r = route();
  if (r.view !== "cut") return;
  const gen = ++cutGeneration;
  let v: Awaited<ReturnType<typeof actions.cutView>> | undefined;
  let err: string | null = null;
  try {
    v = await actions.cutView(r.project, r.cut);
  } catch (e) {
    err = e instanceof Error ? e.message : String((e as { message?: string })?.message ?? e);
  }
  const now = route();
  // A late reply for another cut (or an older request) is dropped.
  if (gen !== cutGeneration || now.view !== "cut" || now.cut !== r.cut) return;
  setCutError(err);
  if (v) setCutView(v);
}

/**
 * One refresh at a time, and one more after it when events came in meanwhile (R-EVT-1):
 * an agent's burst sends a batch every 16 ms, and a refresh per batch kept the main
 * thread busy enough to show a key press a frame or two late.
 */
function singleFlight(run: () => Promise<void>): () => void {
  let busy = false;
  let again = false;
  return () => {
    if (busy) {
      again = true;
      return;
    }
    busy = true;
    void (async () => {
      try {
        do {
          again = false;
          await run();
        } while (again);
      } finally {
        busy = false;
      }
    })();
  };
}
const refreshCutSoon = singleFlight(refreshCut);
// the project list (names, cuts) needs no per-edit freshness: once things go quiet
let projectsTimer: ReturnType<typeof setTimeout> | undefined;
const refreshProjectsSoon = () => {
  clearTimeout(projectsTimer);
  projectsTimer = setTimeout(() => void refreshProjects(), 250);
};

async function refreshJobs() {
  const j = await attempt(actions.jobs);
  if (j) setJobs(j);
}

async function refreshViewer() {
  const v = await attempt(actions.viewerState);
  if (v) setViewer(v);
}

/** The viewer's standing error, and the one not said yet. A frame that fails says so a
 * moment later, and only while it still fails: an agent's commit and the frame it broke
 * arrive on two scopes in either order, and an agent mid-edit passes through cuts that
 * do not plan (it deletes the parts, then adds the new ones). While an agent is editing
 * the error waits; it is said once the agent has been quiet for a while and the frame
 * still fails (decision 0059). */
let viewerError: string | null = null;
let viewerErrorTimer: ReturnType<typeof setTimeout> | undefined;
const VIEWER_ERROR_GRACE_MS = 300;
function viewerFailed(error: string | null) {
  viewerError = error;
  clearTimeout(viewerErrorTimer);
  if (!error) return;
  viewerErrorTimer = setTimeout(() => {
    if (viewerError !== error || agentEditing()) return; // gone, or held until the agents settle
    say(error, "error");
  }, VIEWER_ERROR_GRACE_MS);
}
onSettled(() => {
  const e = viewerError ?? heldPlanError;
  heldPlanError = null;
  if (e && (cutError() || viewer()?.error)) say(e, "error");
});

const projectSubs = new Set<string>();
export async function watchProject(id: string) {
  if (projectSubs.has(id)) return;
  // claimed before the subscription is made: two opens of one project used to race here
  // and subscribe twice, so every event was handled twice (an agent's nine commits
  // counted as eighteen)
  projectSubs.add(id);
  const sub = subscribe(`project:${id}`, (b) => {
    // agents at work, drawn (decision 0059): their commits to the project on screen
    const r = route();
    if (r.view === "cut" && r.project === id) {
      const commits = b.events.filter((e) => e.kind === "committed");
      if (commits.length) noteCommits(id, r.cut, commits);
    }
    for (const e of b.events) {
      if (e.kind !== "agent_run") continue;
      setAgentTick((n) => n + 1);
      const r = e.run;
      if (r.state === "done" && (r.pass === "find_moments" || r.pass === "full_sweep")) say(`Your agent found ${r.found ?? 0} moment${r.found === 1 ? "" : "s"} — recall not measured`);
      else if (r.state === "done") say("Your agent finished");
      else if (r.state === "failed") say(`The agent run failed: ${r.error ?? "unknown"}`, "error");
    }
    const analysis = b.events.some((e) => e.kind === "analysis" || e.kind === "transcript");
    if (analysis) setAnalysisTick((n) => n + 1);
    if (b.events.some((e) => e.kind === "proxy")) setProxyTick((n) => n + 1);
    if (b.events.some((e) => e.kind !== "analysis" && e.kind !== "proxy" && e.kind !== "clip")) {
      refreshCutSoon();
      refreshProjectsSoon();
    }
  });
  try {
    await sub;
  } catch (e) {
    projectSubs.delete(id);
    throw e;
  }
}

export async function boot() {
  await loadPrefs();
  await subscribe("viewer", (b) => {
    for (const e of b.events) {
      if (e.kind === "resync") void refreshViewer();
      if (e.kind === "state") setViewer(e.state);
      // (not about a cut no longer on screen: replanning a long-form that was just
      // removed failed after the screen moved on, and its toast hid the one saying so)
      const r = route();
      const elsewhere = e.kind === "error" && typeof e.cut === "string" && r.view === "cut" && r.cut !== e.cut;
      if (e.kind === "error" && !elsewhere) viewerFailed(e.error ?? null);
      if (e.kind === "mattes") setMattesTick((n) => n + 1);
    }
  });
  await subscribe("jobs", (b) => {
    for (const e of b.events) {
      if (e.kind === "resync") {
        void refreshJobs();
        void refreshCut();
        continue;
      }
      if (e.kind !== "job") continue;
      const j = e.job;
      const prev = jobs().find((x) => x.id === j.id);
      setJobs((js) => [...js.filter((x) => x.id !== j.id), j]);
      if (prev?.state === j.state) continue;
      if (j.state === "done") {
        say(`Exported “${j.title}”`);
        setJustExported({ project: j.project, title: j.title, export: j.export ?? null });
        void refreshCut();
      }
      if (j.state === "failed") say(`Export failed: ${j.error ?? "unknown"}`, "error");
    }
  });
  await subscribe("app", (b) => {
    for (const e of b.events) {
      if (e.kind === "menu") void onMenu(e.id);
      if (e.kind === "models") {
        setModelsProgress({ state: e.state, done: e.done ?? undefined, total: e.total ?? undefined, error: e.error ?? undefined });
        if (e.state === "done") say("Speech model installed — transcribing");
        if (e.state === "failed") say(`The speech model download failed: ${e.error ?? "unknown"}`, "error");
      }
    }
  });
  await subscribe("home", (b) => {
    for (const e of b.events) {
      // R-MCP-1c: the first connection from a new program (the rail shows it, with Revoke)
      if (e.kind === "first_seen") {
        setAgentTick((n) => n + 1);
        say("A new program connected to gmclip as an agent: see Agents");
        continue;
      }
      // an agent's ask came or went (decision 0050): the activity rail reads them again
      if (e.kind === "needs_you") setAgentTick((n) => n + 1);
      // an agent's project_new or source_import: list it, and hear its edits
      if (e.kind === "changed" || e.kind === "resync") {
        setHomeTick((n) => n + 1);
        void refreshProjects().then(() => Promise.all(projects().map((p) => watchProject(p.id))));
        continue;
      }
      if (e.kind !== "dropped") continue;
      let paths = e.paths;
      // onto the timeline: videos and images there become layers at that time
      const tl = timelineDropHook();
      const at = e.at;
      if (tl && at?.length === 2 && tl(paths, [at[0]!, at[1]!])) continue;
      // a recording's session folder, or its session.json (decision 0045), opens as a
      // project; a folder that is not one is offered on Home as before
      const recs = paths.filter((p) => SESSION_FILE.test(p) || !/\.[^\\/.]+$/.test(p));
      if (recs.length) {
        void (async () => {
          const left: string[] = [];
          for (const p of recs) if (!(await openRecording(p.replace(SESSION_FILE, ""), !SESSION_FILE.test(p)))) left.push(p);
          if (left.length) setDropped((d) => [...d, ...left]);
        })();
        paths = paths.filter((p) => !recs.includes(p));
      }
      // A dropped video opens straight away; anything else is offered on Home.
      const videos = paths.filter((p) => VIDEO_EXT.test(p));
      const hook = videoDropHook();
      const open = cutView();
      if (videos[0] && hook) hook(videos[0]);
      // onto an open project: they join it (the sheet offers a project of their own);
      // on Home: one project holding all of them
      else if (videos.length && open) void addVideosTo(open.project, videos);
      else if (videos.length) void openVideos(videos);
      // an image dropped while a cut is open goes on the frame shown
      const v = cutView();
      const images = v ? paths.filter((p) => IMAGE_EXT.test(p)) : [];
      for (const p of images) void attempt(() => actions.imageOverlayAddPath(v!.project, v!.cut, viewer()?.frame ?? 0, p));
      setDropped(paths.filter((p) => !VIDEO_EXT.test(p) && !images.includes(p)));
    }
    void refreshHome();
  });
  await refreshProjects();
  await refreshHome();
  for (const p of projects()) await watchProject(p.id);
  await refreshViewer();
  await refreshJobs();
  // every subscription is up: an event from now on (a drop on Home) is heard. The page
  // draws before this, so a drop sent as soon as Home showed was published to no one
  // (multivideo.spec's sheet that never came, under load); tests wait for this mark.
  document.documentElement.dataset.booted = "1";
}

export async function openCut(project: string, cut: string) {
  setCutView(null);
  setSelectedPart(null);
  if (layerGroup().length) setLayerGroup([]);
  setSelectedEdge(null);
  setMarkIn(null);
  setMarkOut(null);
  setRoute({ view: "cut", project, cut });
  await attempt(() => watchProject(project));
  await refreshCut();
  await attempt(() => actions.viewerLoad(project, cut));
}

/** Opens a project at a cut: `cut`, else its first framed one (a story cut has no
 * picture to edit yet); says so when it has none. */
export async function openProject(project: string, cut?: string) {
  const id = cut ?? projects().find((x) => x.id === project)?.cuts.find((c) => c.framed)?.id;
  if (!id) return say("This project has no cut to open yet");
  await openCut(project, id);
}

/** Opens a video (a path, or the native panel when none): its project, its whole-video cut. */
/** The import sheet being shown, and how it answers (U.15). */
export const [importSheet, setImportSheet] = createSignal<{ sheet: ImportInspect; answer: (voice: number | null | undefined) => void } | null>(null);

export async function openVideo(path?: string, hires?: { path: string; offset_s: number; segments?: { from_s: number; offset_s: number }[] }): Promise<void> {
  if (!path) {
    // the panel takes several files: more than one opens as one project
    const p = await attempt(() => actions.pickVideos());
    if (!p?.picked.length) return;
    if (p.picked.length > 1) return openVideos(p.picked);
    path = p.picked[0]!;
  }
  const s = await attempt(() => actions.inspectVideo(path!));
  if (!s) return;
  let voice: number | null = null;
  if (s.sheet) {
    // undefined = cancelled
    const v = await new Promise<number | null | undefined>((answer) => setImportSheet({ sheet: s, answer }));
    setImportSheet(null);
    if (v === undefined) return;
    voice = v;
  }
  const r = await attempt(() => actions.openVideo(path!, voice, hires));
  if (!r) return;
  if (!s.known && !r.reopened && s.notes.length && !s.sheet) say(s.notes[0] ?? "");
  await attempt(() => watchProject(r.project));
  await refreshProjects();
  await openCut(r.project, r.cut);
}

/** The import sheet for several videos being shown, and how it answers. */
export const [importMany, setImportMany] = createSignal<{
  sheet: ImportMany;
  mode: "open" | "add";
  project?: string;
  answer: (a: ManyAnswer | "elsewhere" | undefined) => void;
} | null>(null);

async function askMany(paths: string[], mode: "open" | "add", project?: string): Promise<ManyAnswer | "elsewhere" | undefined> {
  const s = await attempt(() => actions.inspectVideos(paths));
  if (!s) return undefined;
  if (!s.files.length) {
    say(s.failed[0]?.error ?? "gmclip cannot read those files", "error");
    return undefined;
  }
  const name = project ? projects().find((p) => p.id === project)?.name : undefined;
  const a = await new Promise<ManyAnswer | "elsewhere" | undefined>((answer) => setImportMany({ sheet: s, mode, project: name, answer }));
  setImportMany(null);
  return a;
}

/** Several videos from Home (a drop, the panel): one project holding all of them unless
 * the sheet says a project each. One video opens as before. */
export async function openVideos(paths: string[]): Promise<void> {
  if (paths.length === 1) return openVideo(paths[0]);
  const a = await askMany(paths, "open");
  if (!a || a === "elsewhere") return;
  const r = await attempt(() => actions.openVideos(a.files, a.same, a.oneProject, a.name));
  if (!r) return;
  for (const n of r.notes ?? []) say(n);
  await attempt(() => watchProject(r.project));
  await refreshProjects();
  if (r.cut) await openCut(r.project, r.cut);
}

/** Videos dropped on (or added to) an open project: each a source with its own stream
 * edit, transcribed one at a time. */
export async function addVideosTo(project: string, paths: string[]) {
  const a = await askMany(paths, "add", project);
  if (!a) return;
  if (a === "elsewhere") {
    if (paths.length === 1) await openVideo(paths[0]);
    else await openVideos(paths);
    return;
  }
  const r = await attempt(() => actions.addVideos(project, a.files, a.same));
  if (!r) return;
  for (const n of r.notes) say(n);
  const n = r.added.length;
  if (n) say(`Added ${n} video${n === 1 ? "" : "s"} — on the end of the main cut; transcribing one at a time`);
  for (const x of r.skipped) say(`“${x.name}” is ${x.why}`);
  await refreshProjects();
  await refreshCut();
}

/** File › Add Video to Project… (and the project map's ＋): the panel, several files. */
export async function addVideosPick() {
  const v = cutView();
  if (!v) {
    say("Open a project to add videos to it");
    return;
  }
  const r = await attempt(() => actions.pickVideos());
  if (r?.picked.length) await addVideosTo(v.project, r.picked);
}
