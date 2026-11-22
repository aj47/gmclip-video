// The app shell: top bar, the current view, the toast, global keys (SPEC §5.15 subset).
import { createEffect, createSignal, Match, on, onCleanup, onMount, Show, Switch } from "solid-js";
import { KeysHelp } from "./components/KeysHelp";
import { cutKey } from "./keys";
import { actions, ideaActions, packagingActions } from "./actions";
import { size } from "./sizeclass";
import { ImportSheet } from "./components/ImportSheet";
import { ImportManySheet } from "./components/ImportManySheet";
import { attempt, cutView, helpOpen, home, openCut, openProject, historyOpen, importMany, importSheet, justExported, setJustExported, noteKeyUndo, onMenu, paletteOpen, projects, refreshHome, route, undoRedo, saved, saveError, say, setHelpOpen, setHistoryOpen, setPaletteOpen, setRoute, setToast, toast } from "./store";
import { History, UndoAsk } from "./components/History";
import { prefs, toggleHoverScrub } from "./prefs";
import { IconButton, installTooltips } from "./design/components";
import { Icon } from "./design/Icon";
// its own file, never a data: URL (the app's CSP has no img-src, so data: images are blocked)
import sun from "./design/sun-64.png?no-inline";
import { ExportDoneChip, setTopbarSlot } from "./components/Chrome";
import { Palette, type PaletteAction } from "./components/Palette";
import { CutView } from "./views/CutView";
import { Home } from "./views/Home";
import { Settings } from "./components/Setup";
import { Search } from "./views/Search";
import { Agents } from "./views/Agents";
import { AgentLaunch } from "./components/AgentLaunch";
import { AgentDock } from "./components/AgentDock";
import { launchFor, liveSessions, setLaunchFor, waitingSessions, watchSessions } from "./agentsState";
import { setCutLikeOpen } from "./libraryState";
import { applyWorkspace, savedWorkspaces } from "./workspaces";
import { DEFAULT_TABS, openTighten, paneShown, resetLayout, showTab, SIDE_TABS, toggleFocus, togglePane, toggleReading } from "./layout";
import { DrawLayoutSheet } from "./components/DrawLayoutSheet";
import { LongformCompare } from "./components/LongformCompare";
import { compareLongs, compareLongsFor, drawLayout, setCompareLongs, setDrawLayout } from "./ideasState";
import { projectMapData, refreshMap } from "./streamState";
import { copyText } from "./components/Packaging";
import { askBroll } from "./components/Broll";
import { setTrFollow, trFollow, trSel } from "./transcriptState";
import { FramingsSheet } from "./components/FramingsSheet";
import { framingsFor, setFramingsFor } from "./extrasState";
import { addLongformEndCard } from "./components/EndCardLongform";
import { groupSelected, ungroupSelected } from "./partGroups";
import { RecordingOverlay, RecordSheet } from "./components/Recording";
import { recordSheet, resumeRecording, setRecordSheet } from "./recordingState";

/** A top bar destination: icon and name; in a cut (`.tb-icons`) the name hides and the
 * tooltip says it. */
function NavButton(props: { icon: string; label: string; on: boolean; compact: boolean; hint?: string; testid?: string; onClick: () => void }) {
  return (
    <button
      type="button"
      class={`ghost tb-nav${props.on ? " nav-on" : ""}`}
      aria-label={props.label}
      aria-current={props.on ? "page" : undefined}
      data-tip={props.hint ?? (props.compact ? props.label : undefined)}
      data-testid={props.testid}
      onClick={() => props.onClick()}
    >
      <Icon name={props.icon} size={16} />
      <span class="tb-nav-label">{props.label}</span>
    </button>
  );
}

export function App() {
  // the first render is done (U.18's cold start measures to here)
  onMount(() => requestAnimationFrame(() => void actions.appReady().catch(() => undefined)));
  // the shared tooltip layer (design system): [data-tip] anywhere gets a styled tooltip
  onMount(() => onCleanup(installTooltips()));
  // the top bar counts running agent sessions (decision 0035)
  onMount(() => void watchSessions());
  // a recording still running (a reload) gets its bar back (decision 0045)
  onMount(() => void resumeRecording());
  // playback runs in the core, so leaving a view that plays (Home, Settings, …) stops it
  createEffect(on(() => route().view, () => void attempt(actions.viewerPause), { defer: true }));
  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      const mod = e.metaKey || e.ctrlKey;
      // ⌘K works from anywhere, even a text field
      if (mod && !e.shiftKey && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPaletteOpen(!paletteOpen());
        return;
      }
      // ⌘⇧F: search everything said across the library (lane L), from anywhere
      if (mod && e.shiftKey && e.key.toLowerCase() === "f") {
        e.preventDefault();
        setRoute({ view: "search" });
        return;
      }
      if (e.defaultPrevented || t.closest("input, textarea, select, [contenteditable]")) return;
      const r = route();
      const project = r.view === "cut" ? r.project : null;
      if (mod && e.shiftKey && e.key.toLowerCase() === "s" && r.view === "cut") {
        e.preventDefault();
        setHistoryOpen({ nameFirst: true });
        return;
      }
      if (mod && e.key.toLowerCase() === "z" && project) {
        e.preventDefault();
        noteKeyUndo();
        void undoRedo(project, e.shiftKey, r.view === "cut");
        return;
      }
      if (r.view !== "cut") return;
      if (helpOpen()) return;
      // Space on a focused control activates it; elsewhere it plays.
      if (e.key === " " && t.closest("button, a, [role=button], summary")) return;
      if (cutKey(e, () => setHelpOpen(true))) e.preventDefault();
    };
    addEventListener("keydown", onKey);
    onCleanup(() => removeEventListener("keydown", onKey));
  });

  // agents' asks waiting for you (decision 0050: Home's "Needs you" lists them too)
  const asks = () => home().needs_you.filter((n) => n.approval).length;
  const needYou = () => waitingSessions() + asks();
  const inCut = () => route().view === "cut";
  // the cut last open in each project: the top bar's way back goes to it, else the
  // project's first framed cut
  const lastCut = new Map<string, string>();
  // the project last open: Agents, Search and Settings keep it in the top bar, one click
  // away (AJ 2026-10-19)
  const [lastProject, setLastProject] = createSignal<string | null>(null);
  createEffect(() => {
    const r = route();
    if (r.view === "cut") {
      lastCut.set(r.project, r.cut);
      setLastProject(r.project);
    }
    if (r.view === "home") setLastProject(null);
  });
  const backTo = () => {
    const p = lastProject();
    return route().view !== "cut" && p && projects().some((x) => x.id === p) ? p : null;
  };
  const projectName = () => projects().find((x) => x.id === lastProject())?.name ?? "";
  /** The video Draw your layout opens on: this cut's own stream's video, else the first landscape one. */
  const layoutSource = () => {
    const srcs = (projectMapData()?.sources ?? []).filter((x) => !x.offline && (x.width ?? 0) > (x.height ?? 0));
    return (srcs.find((x) => x.stream === cutView()?.cut) ?? srcs[0])?.id;
  };
  const forkLong = async () => {
    const v = cutView();
    if (!v) return;
    const r = await attempt(() => ideaActions.longformFork(v.project, v.cut));
    if (!r) return;
    say(`Forked “${v.title}” — reorder its parts, then Compare`);
    await refreshMap(v.project);
    await openCut(v.project, r.cut);
  };
  const copyChapters = async () => {
    const v = cutView();
    if (!v) return;
    const c = await attempt(() => packagingActions.chapters(v.project, v.cut));
    if (!c) return;
    if (!c.youtube) return say(c.why_not ?? "No chapters YouTube would show yet", "error");
    say((await copyText(c.youtube)) ? `Chapters copied (${c.youtube_chapters.length})` : "Could not copy", "info");
  };
  // the layouts the person saved (View menu)
  const layoutActions = (): PaletteAction[] =>
    savedWorkspaces().map((w) => ({
      id: `layout_${w.name}`,
      label: `Layout: ${w.name}`,
      also: "workspace panes arrangement preset",
      when: inCut,
      run: () => applyWorkspace(w),
    }));
  const paletteActions: PaletteAction[] = [
    { id: "open_video", label: "Open video…", also: "import file stream", keys: "⌘O", run: () => void onMenu("open_video") },
    { id: "add_video", label: "Add video to this project…", also: "import file source multiple videos second camera", keys: "⌘⇧O", run: () => void onMenu("add_video") },
    { id: "export", label: "Export", also: "render mp4", keys: "⌘E", when: inCut, run: () => void onMenu("export") },
    { id: "endcard_longform", label: "End card to the long-form", also: "full video link outro arrow promote youtube", when: inCut, run: () => void addLongformEndCard() },
    { id: "export_both", label: "Export 9:16 and 16:9…", also: "both framings vertical horizontal shorts youtube render twice", when: inCut, run: () => { const v = cutView(); if (v) setFramingsFor({ project: v.project, only: v.cut }); } },
    { id: "save", label: "Save a restore point", also: "snapshot backup", keys: "⌘S", run: () => void onMenu("save") },
    { id: "name", label: "Name this version…", also: "named snapshot", keys: "⌘⇧S", when: inCut, run: () => setHistoryOpen({ nameFirst: true }) },
    { id: "history", label: "History: earlier versions", also: "restore undo versions", when: inCut, run: () => setHistoryOpen({}) },
    { id: "undo", label: "Undo", keys: "⌘Z", run: () => void onMenu("undo") },
    { id: "redo", label: "Redo", keys: "⌘⇧Z", run: () => void onMenu("redo") },
    { id: "keys", label: "Keyboard shortcuts", also: "help keys", keys: "?", run: () => setHelpOpen(true) },
    { id: "home", label: "Go to Home", also: "projects library", run: () => { setRoute({ view: "home" }); void refreshHome(); } },
    { id: "cut_like", label: "New cut like this…", also: "template look copy layout style same", when: inCut, run: () => setCutLikeOpen(true) },
    { id: "library_search", label: "Search everything you said", also: "library transcripts find streams meaning", keys: "⌘⇧F", run: () => setRoute({ view: "search" }) },
    { id: "settings", label: "Settings", also: "preferences owner", run: () => setRoute({ view: "settings" }) },
    { id: "agents", label: "Agents: your sessions", also: "claude code codex pi opencode acp running sessions chat", run: () => setRoute({ view: "agents" }) },
    // starting an agent inside gmclip: the developer setting's (decision 0054)
    { id: "agent_new", label: "New agent session…", also: "claude code codex pi opencode ask agent run", when: () => prefs.developer, run: () => setLaunchFor({}) },
    { id: "agent_cut", label: "Ask an agent about this cut…", also: "claude code codex pi opencode edit", when: () => inCut() && prefs.developer, run: () => { const v = cutView(); if (v) setLaunchFor({ project: v.project, cut: v.cut }); } },
    { id: "hover_scrub", label: "Hover scrub on / off", also: "skim skimming timeline ruler follow pointer", keys: "⌥S", when: inCut, run: () => toggleHoverScrub() },
    { id: "follow", label: "Words: follow the playhead on / off", also: "transcript scroll keep the playing word in view", when: inCut, run: () => { setTrFollow(!trFollow()); say(trFollow() ? "Words follow the playhead" : "Words stay where you scroll"); } },
    { id: "reading", label: "Read the full transcript", also: "prose text no times reading view whole transcript", keys: "⌥R", when: inCut, run: toggleReading },
    { id: "focus", label: "Focus: viewer and timeline only", also: "hide panels distraction free", keys: "⌥F", when: inCut, run: toggleFocus },
    { id: "pane_map", label: "Project map on / off", also: "sidebar left panel hide show", keys: "⌥1", when: inCut, run: () => togglePane("map") },
    { id: "pane_side", label: "Side panel on / off", also: "sidebar right panel inspector transcript hide show", keys: "⌥2", when: inCut, run: () => togglePane("side") },
    { id: "pane_tl", label: "Timeline on / off", also: "hide show lanes", keys: "⌥3", when: inCut, run: () => togglePane("timeline") },
    { id: "reset_layout", label: "Reset layout", also: "panes panels sizes default", keys: "⌥0", when: inCut, run: resetLayout },
    { id: "group", label: "Group the selected parts", also: "link parts move together timeline clips", keys: "⌘G", when: inCut, run: () => void groupSelected() },
    { id: "ungroup", label: "Ungroup", also: "unlink parts timeline clips group", keys: "⌘⇧G", when: inCut, run: () => void ungroupSelected() },
    { id: "split", label: "Split at the playhead", also: "cut part", keys: "S", when: inCut, run: () => dispatchEvent(new KeyboardEvent("keydown", { key: "s" })) },
    { id: "play", label: "Play / pause", also: "transport", keys: "Space", when: inCut, run: () => void attempt(actions.viewerToggle) },
    // every side-panel tab by name, shown (never hidden) from here; the ones off the bar
    // are the "More panels" section (decision 0054)
    ...SIDE_TABS.map((t): PaletteAction => ({
      id: `tab_${t.id}`,
      label: `${t.label} panel`,
      also: t.what,
      section: DEFAULT_TABS.includes(t.id) ? undefined : "More panels",
      keys: t.id === "words" ? "⌥T" : t.id === "look" ? "⌥I" : undefined,
      when: inCut,
      run: () => showTab(t.id),
    })),
    { id: "tighten", label: "Tighten: pauses, fillers and retakes", also: "sweep silence um uh dead air words said again tidy", when: inCut, run: () => openTighten() },
    // the panels' own sections, so a feature is found by its name
    { id: "openings", label: "Compare openings…", also: "alternate openings versions hook first seconds short", when: inCut, run: () => showTab("captions", "openings") },
    { id: "hook_lines", label: "Hook lines from the clip's numbers", also: "text hook suggestions measured said short", when: inCut, run: () => showTab("captions", "hook-lines") },
    { id: "copy_drafts", label: "Titles and descriptions", also: "copy drafts youtube tiktok instagram shorts publish caption", when: inCut, run: () => showTab("thumbnail", "copy-drafts") },
    { id: "speed", label: "Speed and ramp", also: "slow motion fast faster slower time stretch inspector part", when: inCut, run: () => showTab("look", "inspector-sound") },
    { id: "looks", label: "Backdrop, plate and video overlay", also: "background gradient plate picture in picture blur cover pin to motion track inspector", when: inCut, run: () => showTab("look", "plate-row") },
    { id: "draw_layout", label: "Draw your layout…", also: "boxes screen camera face chat private stream layout", when: () => inCut() && !!layoutSource(), run: () => { const v = cutView(); const src = layoutSource(); if (v && src) setDrawLayout({ project: v.project, source: src }); } },
    { id: "compare_longs", label: "Compare long-forms…", also: "two long-forms side by side order pick fork", when: () => inCut() && (projectMapData()?.longs.length ?? 0) > 1, run: () => { const v = cutView(); if (v) compareLongsFor(v.project, projectMapData()?.longs ?? [], v.cut); } },
    { id: "fork_long", label: "Fork this long-form", also: "copy sibling another order try", when: () => inCut() && cutView()?.kind === "long", run: () => void forkLong() },
    { id: "chapters", label: "Chapters: copy for YouTube", also: "timestamps description 00:00 long-form", when: () => inCut() && cutView()?.kind === "long", run: () => void copyChapters() },
    { id: "broll", label: "B-roll for the selected words", also: "other recordings library footage cutaway", when: inCut, run: () => {
      const b = document.querySelector<HTMLButtonElement>('[data-testid="broll-open"]');
      const v = cutView();
      const t = trSel();
      if (b && !b.disabled && b.offsetParent) b.click();
      else if (v && t && t.hi - t.lo >= 2) askBroll({ project: v.project, cut: v.cut, lo: t.lo, hi: t.hi });
      else say("Select three or more words (in the transcript or the words strip) first");
    } },
  ];

  return (
    <div class="app" data-size={size()}>
      <nav class="topbar" aria-label="Main">
        {/* the project map's toggle sits at the far left, over the map it opens */}
        <Show when={route().view === "cut"}>
          <IconButton
            icon="panel-left"
            class={`tb-map${paneShown("map") ? " nav-on" : ""}`}
            label={paneShown("map") ? "Hide the project map" : "Show the project map"}
            keys="⌥1"
            pressed={paneShown("map")}
            data-testid="map-toggle"
            onClick={() => togglePane("map")}
          />
        </Show>
        {/* one sun (AJ 2026-11-22): the app's yellow sun is the Agents button (the outline
            sun that was Agents is gone); then Home and Settings, named. In a cut the View
            menu is the "⋯" at the right; in a narrow cut the names hide (tooltips say them). */}
        <span class="tb-group" classList={{ "tb-cut": inCut() }}>
          <span class="tb-agents">
            <button
              type="button"
              class={`ghost tb-nav tb-sun${route().view === "agents" ? " nav-on" : ""}`}
              aria-label="Agents"
              aria-current={route().view === "agents" ? "page" : undefined}
              data-tip={liveSessions() || asks() ? `Agents: ${liveSessions()} running${needYou() ? `, ${needYou()} need you` : ""}` : "Agents: your agent sessions"}
              data-testid="nav-agents"
              onClick={() => setRoute({ view: "agents" })}
            >
              <img class="brand" src={sun} alt="" aria-hidden="true" />
              {/* named like Home and Settings: everywhere but a narrow cut window */}
              <span class="tb-nav-label">Agents</span>
            </button>
            <Show when={liveSessions() || asks()}>
              <span class={`tb-badge${needYou() ? " waiting" : ""}`} data-testid="agents-badge" aria-hidden="true">{needYou() || liveSessions()}</span>
            </Show>
          </span>
          <NavButton icon="home" label="Home" compact={inCut()} on={route().view === "home"} onClick={() => { setRoute({ view: "home" }); void refreshHome(); }} />
          {/* Search is always here (it was a crumb only on its own page): the library is
              searched from anywhere, and ⌘⇧F says so in its tooltip */}
          <NavButton icon="search" label="Search" compact={inCut()} hint="Search everything you said (⌘⇧F)" on={route().view === "search"} onClick={() => setRoute({ view: "search" })} />
          <NavButton icon="settings" label="Settings" compact={inCut()} on={route().view === "settings"} onClick={() => setRoute({ view: "settings" })} />
        </span>
        <Show when={backTo()}>
          {(p) => (
            <button type="button" class="ghost small tb-project" data-testid="tb-project" aria-label={`Back to ${projectName()}`} data-tip={`Back to ${projectName()}`} onClick={() => void openProject(p(), lastCut.get(p()))}>
              <Icon name="film" size={13} />
              <span>{projectName()}</span>
            </button>
          )}
        </Show>
        <div class="tb-slot" ref={setTopbarSlot} />
        <Show when={route().view !== "cut"}>
          <span class="tb-right">
            <ExportDoneChip />
          </span>
        </Show>
      </nav>
      {/* every window size: the views reflow (shell.css); nothing is unmounted on resize */}
      <div class="app-body">
        <Switch>
          <Match when={route().view === "settings"}>
            <Settings />
          </Match>
          <Match when={route().view === "home"}>
            <Home />
          </Match>
          <Match when={route().view === "cut"}>
            <CutView />
          </Match>
          <Match when={route().view === "search" ? route() : null}>
            {(r) => <Search q={(r() as { q?: string }).q} project={(r() as { project?: string }).project} />}
          </Match>
          <Match when={route().view === "agents"}>
            <Agents />
          </Match>
        </Switch>
      </div>
      <Show when={helpOpen()}>
        <KeysHelp onClose={() => setHelpOpen(false)} />
      </Show>
      <UndoAsk />
      <Show when={historyOpen() && cutView() ? historyOpen() : null}>
        {(h) => <History reason={h().reason} nameFirst={h().nameFirst} onClose={() => setHistoryOpen(null)} />}
      </Show>
      {/* Home's big sun (decision 0035); the floating one in a project's views only with the
          developer setting (decision 0054: the top bar's Agents is the way in) */}
      <Show when={route().view === "home"}>
        <AgentDock big />
      </Show>
      <Show when={(() => { const r = route(); return r.view === "cut" && prefs.developer ? r.project : null; })()} keyed>
        {(p) => <AgentDock project={p} />}
      </Show>
      <Show when={launchFor()}>{(at) => <AgentLaunch at={at()} onClose={() => setLaunchFor(null)} />}</Show>
      <Show when={paletteOpen()}>
        <Palette actions={[...paletteActions, ...layoutActions()]} onClose={() => setPaletteOpen(false)} />
      </Show>
      <Show when={(() => { const f = framingsFor(); const p = f && projects().find((x) => x.id === f.project); return p ? { p, only: f!.only } : null; })()}>
        {(x) => <FramingsSheet project={x().p} only={x().only} onClose={() => setFramingsFor(null)} />}
      </Show>
      <Show when={importSheet()}>
        {(x) => <ImportSheet sheet={x().sheet} onOpen={(v) => x().answer(v)} onCancel={() => x().answer(undefined)} />}
      </Show>
      <Show when={importMany()}>
        {(x) => (
          <ImportManySheet
            sheet={x().sheet}
            mode={x().mode}
            project={x().project}
            onDone={(a) => x().answer(a)}
            onElsewhere={() => x().answer("elsewhere")}
            onCancel={() => x().answer(undefined)}
          />
        )}
      </Show>
      <Show when={drawLayout()}>{(d) => <DrawLayoutSheet project={d().project} source={d().source} onClose={() => setDrawLayout(null)} />}</Show>
      <Show when={compareLongs()}>{(c) => <LongformCompare project={c().project} a={c().a} b={c().b} onClose={() => setCompareLongs(null)} />}</Show>
      {/* recording (decision 0045): the Record sheet, the countdown and the bar over every view */}
      <Show when={recordSheet()}>
        <RecordSheet onClose={() => setRecordSheet(false)} />
      </Show>
      <RecordingOverlay />
      <Show when={toast()}>
        {(t) => (
          <div class={`toast ${t().kind}`} role={t().kind === "error" ? "alert" : "status"}>
            <Icon name={t().kind === "error" ? "circle-alert" : "circle-check"} size={15} />
            <span>{t().text}</span>
            <Show when={t().undo}>
              {(p) => (
                <button type="button" class="toast-action" data-testid="toast-undo" onClick={() => { setToast(null); void attempt(() => actions.undo(p())); }}>
                  Undo
                </button>
              )}
            </Show>
            <Show when={t().act}>
              {(a) => (
                <button type="button" class="toast-action" data-testid="toast-act" onClick={() => { setToast(null); a().run(); }}>
                  {a().label}
                </button>
              )}
            </Show>
          </div>
        )}
      </Show>
    </div>
  );
}
