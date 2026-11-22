// Home (SPEC §12.6): running work with ETA, what needs you (one word each), what is done
// since you last looked (one Activity card, only when there is any); plus opening and
// importing. Empty states: one sentence, one action (§12.5).
import { baseName, isMac, isWindows } from "../platform";
import { about, ago } from "../eta";
import { createEffect, createMemo, createResource, createSignal, For, on, onMount, Show } from "solid-js";
import { ConnectAgent } from "../components/AgentPanel";
import { FirstChecklist, SetupSheet, WelcomeCard } from "../components/FirstRun";
import { RestoreSheet } from "../components/Reliability";
import { setLaunchFor } from "../agentsState";
import { setFramingsFor } from "../extrasState";
import { ConsolidateSheet } from "../components/ConsolidateSheet";
import { BatchSheet } from "../components/BatchSheet";
import { actions, type Home as HomeData, type PairProposal, type ProjectInfo, type RecentProject } from "../actions";
import { Button, Menu } from "../design/components";
import { Icon } from "../design/Icon";
import { humanTitle } from "./CutView";
import { RecentThumb } from "../components/VideoThumb";
import { prefs } from "../prefs";
import { openRecording, setRecordSheet } from "../recordingState";
import { attempt, dropped, home, homeTick, openCut, openProject, openVideo, projects, refreshHome, refreshProjects, say, setDropped, setRoute, watchProject } from "../store";

/** An agent pass, said plainly. */
const PASS: Record<string, string> = {
  find_moments: "Finding moments",
  full_sweep: "Sweeping for moments",
  grow_story: "Growing the story",
  edit_pass: "Editing pass",
  topics_fluff: "Finding topics",
  thumbnail: "Thumbnail",
  publish_copy: "Writing titles",
};
const pass = (p: string) => PASS[p] ?? p.replace(/_/g, " ");
/** " · <project>" when it says which project a row is in. */
const inProject = (name: string | null | undefined) => (name ? ` · ${name}` : "");

function dur(s: number): string {
  const t = Math.round(s);
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const sec = String(t % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

export function Home() {
  const [searchQ, setSearchQ] = createSignal("");
  // a batch of agent runs over a project's cuts (the developer setting's, decision 0054)
  const [batchFor, setBatchFor] = createSignal<ProjectInfo | null>(null);
  // lane L: open a consolidated folder
  const [consolidating, setConsolidating] = createSignal(false);
  const [openPath, setOpenPath] = createSignal("");
  const [recent, { refetch: refetchRecent }] = createResource(homeTick, async () => (await attempt(actions.recentVideos))?.projects ?? []);
  const [videoPath, setVideoPath] = createSignal("");
  const [opening, setOpening] = createSignal(false);
  const [connecting, setConnecting] = createSignal(false);
  // the project folder field, behind "Open folder…" (no native folder picker yet)
  const [folderOpen, setFolderOpen] = createSignal(false);
  const [pair, setPair] = createSignal(false);
  // lane S: the stranger setup, restoring a backup, the checklist's refresh tick
  const [stranger, setStranger] = createSignal(false);
  const [restoring, setRestoring] = createSignal(false);
  const [welcomeKey, setWelcomeKey] = createSignal(0);
  // a recording's session folder (decision 0045), behind "A recording folder…"
  const [recPathOpen, setRecPathOpen] = createSignal(false);
  const [recPath, setRecPath] = createSignal("");
  const openRec = async () => {
    // the folder, or its session.json
    const dir = recPath().trim().replace(/[\\/]session\.json$/, "");
    if (!dir) return;
    setOpening(true);
    await openRecording(dir);
    setOpening(false);
    setRecPath("");
    setRecPathOpen(false);
    void refetchRecent();
  };
  const open_ = async (path?: string) => {
    setOpening(true);
    await openVideo(path);
    setOpening(false);
    void refetchRecent();
  };
  onMount(() => void refreshHome());
  const active = () => home().running.length + home().needs_you.length + home().done.length > 0;
  // a project shows once: an open one as its own card (with its picture when it has a
  // recent video), a closed one in "Opened before", the rest of the library after that
  const projectIds = createMemo(() => projects().map((p) => p.id), undefined, { equals: (a, b) => a.length === b.length && a.every((x, i) => x === b[i]) });
  const recentOf = (id: string) => recent()?.find((r) => r.project === id);
  const moreLibrary = () => (home().library ?? []).filter((l) => !l.open);
  // a closed project in the library is its row (exports, moments); a card only otherwise
  const closedRecent = () => (recent() ?? []).filter((r) => !projects().some((p) => p.id === r.project) && !moreLibrary().some((l) => l.id === r.project));

  const openAt = async (path: string) => {
    const p = await attempt(() => actions.openHome(path));
    if (p) {
      await watchProject(p.id);
      await refreshProjects();
      refreshHome();
    }
    return !!p;
  };
  const open = async () => {
    if (!(await openAt(openPath().trim()))) return;
    setOpenPath("");
    setFolderOpen(false);
  };
  /** A project of the library: open now, or opened from its home first (a closed one). */
  const projectFrom = async (project: string, closedHome?: string | null) => {
    if (!closedHome) return project;
    const p = await attempt(() => actions.openHome(closedHome));
    if (!p) return null;
    await watchProject(p.id);
    await refreshProjects();
    return p.id;
  };
  // Needs you → Grow: a new 9:16 cut over the staged moment, opened in the editor (the
  // story panel grows it); a closed project is opened first
  const grow = async (n: HomeData["needs_you"][number]) => {
    const pid = await projectFrom(n.project, n.closed_home);
    if (!pid || !n.moment) return;
    const r = await attempt(() => actions.cutFromMoment(pid, n.moment!));
    if (r) {
      say(`A ${r.seconds.toFixed(1)} s cut from the moment — grow it in the Story panel (⌘K)`);
      await openCut(pid, r.cut);
    }
    refreshHome();
  };
  // the project's other staged moments: its first cut (an agent drafts cuts from them)
  const toProject = async (n: HomeData["needs_you"][number]) => {
    const pid = await projectFrom(n.project, n.closed_home);
    if (pid) await openProject(pid);
  };
  // a .renderplan.json dropped on the window: Import on a project's row
  const doImport = async (project: string, path: string) => {
    const r = await attempt(() => actions.importCut(project, path.trim()));
    if (r) {
      say(`Imported “${r.title}”`);
      setDropped((d) => d.filter((x) => x !== path));
      await refreshProjects();
      await openCut(project, r.cut);
    }
  };

  return (
    <main class="home" aria-label="Home">
      <WelcomeCard tick={welcomeKey()} onSetup={() => setStranger(true)} />
      <FirstChecklist tick={projects().length + home().done.length + welcomeKey()} />
      <div class="home-hero" classList={{ solo: !active(), compact: !!(projects().length || recent()?.length) }}>
        <section class="dropzone" aria-label="Open a video">
          <span class="drop-icon"><Icon name="upload" size={24} /></span>
          <h1>Open a video</h1>
          <p>Drop a recording anywhere in this window, or choose one. MP4, MOV, MKV or WebM.</p>
          <div class="drop-actions">
            <Button variant="primary" size="lg" icon="folder-open" data-testid="open-video" disabled={opening()} keys="⌘O" onClick={() => void open_()}>
              {opening() ? "Opening…" : "Open video…"}
            </Button>
            <Show
              when={prefs.recording}
              fallback={
                <Show when={isMac}>
                  <Button size="lg" icon="record" data-testid="record-hint" tooltip="Turn on recording in Settings: screen and face as files of their own, with the cursor logged" onClick={() => setRoute({ view: "settings" })}>
                    Set up recording…
                  </Button>
                </Show>
              }
            >
              <Button size="lg" icon="record" data-testid="record-open" tooltip="Screen and face as files of their own, with the cursor logged" onClick={() => setRecordSheet(true)}>
                Record…
              </Button>
            </Show>
          </div>
          <form class="path" onSubmit={(e) => { e.preventDefault(); if (videoPath().trim()) void open_(videoPath().trim()); }}>
            <input type="text" placeholder="…or paste a path to a video" aria-label="Video path" value={videoPath()} onInput={(e) => setVideoPath(e.currentTarget.value)} />
            <button type="submit" disabled={!videoPath().trim() || opening()}>Open</button>
          </form>
          <Show when={folderOpen()}>
            <form class="path" onSubmit={(e) => { e.preventDefault(); void open(); }}>
              <input type="text" ref={(el) => queueMicrotask(() => el.focus())} placeholder="…/Library/Name.gmclip (a project folder)" aria-label="Project folder" value={openPath()} onInput={(e) => setOpenPath(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Escape") setFolderOpen(false); }} />
              <button type="submit" disabled={!openPath().trim()}>Open</button>
            </form>
          </Show>
          <Show when={recPathOpen()}>
            <form class="path" onSubmit={(e) => { e.preventDefault(); void openRec(); }}>
              <input type="text" ref={(el) => queueMicrotask(() => el.focus())} placeholder="…/Recordings/Recording … (the folder with session.json)" aria-label="Recording folder" data-testid="rec-open-path" value={recPath()} onInput={(e) => setRecPath(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Escape") setRecPathOpen(false); }} />
              <button type="submit" disabled={!recPath().trim() || opening()}>Open</button>
            </form>
          </Show>
          <Menu
            label="Other ways to open"
            text="Other ways to open"
            align="start"
            testid="open-more"
            items={() => [
              { label: "A stream with its hi-res picture…", icon: "layers", testid: "open-pair", onSelect: () => setPair(true) },
              { label: "A project folder…", icon: "folder-open", testid: "open-folder", onSelect: () => setFolderOpen(true) },
              { label: "A consolidated project…", icon: "folder-open", testid: "open-consolidated", onSelect: () => setConsolidating(true) },
              { label: "A recording folder…", icon: "video", testid: "open-recording", onSelect: () => setRecPathOpen(true) },
              { label: "Restore an earlier copy…", icon: "history", testid: "restore-open", onSelect: () => setRestoring(true) },
              { separator: true, label: "" },
              { label: "Connect an agent…", icon: "bot", testid: "connect-agent", onSelect: () => setConnecting(true) },
            ]}
          />
        </section>
        <Show when={active()}>
          <section class="col home-status" aria-label="Activity">
            <h2>Activity</h2>
            <Show when={home().running.length}>
              <div class="activity-group" aria-label="Running">
                <h3>Running</h3>
                <For each={home().running}>
                  {(r) => {
                    switch (r.kind) {
                      case "transcript":
                        return (
                          <div class="item" data-testid="running-transcript">
                            <span>{r.phase === "analysing" ? "Analysing" : r.phase === "queued" ? "Waiting to transcribe" : "Transcribing"} “{r.title ?? "a video"}”</span>
                            <progress max={Math.max(1, r.total_s)} value={r.phase === "transcribing" ? r.done_s : r.phase === "analysing" ? r.total_s : 0} />
                            <span class="muted">{r.eta_s !== null && r.phase !== "analysing" ? `transcript in ${about(r.eta_s)}` : r.state}</span>
                          </div>
                        );
                      case "agent":
                        return (
                          <div class="item" data-testid="running-agent">
                            <span>{pass(r.pass)}{inProject(r.project_name)}</span>
                            <span class="muted">{r.harness_label ? `${r.harness_label} · ` : ""}{r.state === "running" ? `started ${ago(r.started_ms)}` : r.state}</span>
                          </div>
                        );
                      case "batch":
                        return (
                          <div class="item" data-testid="running-batch">
                            <span>{pass(r.pass)} on {r.total} cut{r.total === 1 ? "" : "s"}{inProject(r.project_name)}</span>
                            <progress max={Math.max(1, r.total)} value={r.done} />
                            <span class="muted">{r.done} of {r.total}</span>
                          </div>
                        );
                      default:
                        return (
                          <div class="item" data-testid="running-export">
                            <span>Exporting “{r.title}”{inProject(r.project_name)}</span>
                            <progress max={r.frames} value={r.done} />
                            <span class="muted">{r.eta_s !== null ? `~${Math.max(0, Math.round(r.eta_s))} s left` : r.state}</span>
                          </div>
                        );
                    }
                  }}
                </For>
              </div>
            </Show>
            <Show when={home().needs_you.length}>
              <div class="activity-group" aria-label="Needs you">
                <h3>Needs you</h3>
                <For each={home().needs_you}>
                  {(n) => (
                    <div class="item" data-testid={n.ask ? "review-ask" : n.action === "grow" ? "needs-grow" : n.approval ? "needs-approval" : undefined} data-action={n.action}>
                      <span>“{n.title}” — {n.why}{inProject(n.project_name)}</span>
                      <Show when={n.approval}>
                        {(id) => (
                          <span class="row">
                            <Show when={n.exportable}>
                              <button type="button" class="small" data-testid="needs-approval-export" data-tip="Export it as you: a human export is not blocked" onClick={() => void attempt(() => actions.approvalExport(id())).then((r) => { if (r) say("Exporting — the agent can wait on it"); refreshHome(); })}>
                                Approve and export
                              </button>
                            </Show>
                            <Show when={n.cut && n.project}>
                              <button type="button" class="small ghost" onClick={() => void openCut(n.project, n.cut!)}>Open</button>
                            </Show>
                            <button type="button" class="small ghost" data-testid="needs-approval-dismiss" onClick={() => void attempt(() => actions.approvalResolve(id(), null)).then(refreshHome)}>
                              Dismiss
                            </button>
                          </span>
                        )}
                      </Show>
                      <Show when={n.ask && (n.author || n.note?.text)}>
                        <p class="small untrusted from-link" data-testid="review-ask-note">
                          <span class="muted">{(n.author ?? "a review link").replace(/^review-link:/, "review link ")} wrote (unchecked):</span> {n.note?.text ?? ""}
                        </p>
                      </Show>
                      <Show
                        when={n.ask}
                        fallback={
                          <Show
                            when={!n.approval && n.cut}
                            fallback={
                              <Show when={!n.approval}>
                              <button
                                type="button"
                                class="small"
                                data-testid={n.action === "grow" ? "needs-grow-go" : undefined}
                                data-tip={n.action === "grow" ? "A new 9:16 cut over this moment, opened in the editor" : "Open the project"}
                                onClick={() => void (n.action === "grow" ? grow(n) : toProject(n))}
                              >
                                {n.action === "grow" ? "Grow" : "Open"}
                              </button>
                              </Show>
                            }
                          >
                            {(cut) => (
                              <button type="button" class="small" onClick={() => void (n.action === "fix" ? openCut(n.project, cut()) : attempt(() => actions.exportStart(n.project, cut())))}>
                                {n.action === "re-export" ? "Re-export" : n.action === "fix" ? "Fix" : "Export"}
                              </button>
                            )}
                          </Show>
                        }
                      >
                        {(ask) => (
                          <>
                            <button type="button" class="small" data-testid="review-ask-start" onClick={() => void attempt(() => actions.reviewAsk(ask(), true)).then(refreshHome)}>
                              {n.action === "remote_send" ? "Send to Clippy" : "Start agent"}
                            </button>
                            <button type="button" class="small ghost" data-testid="review-ask-dismiss" onClick={() => void attempt(() => actions.reviewAsk(ask(), false)).then(refreshHome)}>
                              Dismiss
                            </button>
                          </>
                        )}
                      </Show>
                    </div>
                  )}
                </For>
              </div>
            </Show>
            <Show when={home().done.length}>
              <div class="activity-group" aria-label="Done">
                <h3>Done</h3>
                <For each={home().done}>
                  {(d) => (
                    <div class="item">
                      <span>Exported “{d.title}”</span>
                      <Show
                        when={d.closed_home}
                        fallback={<button type="button" class="small" onClick={() => void attempt(() => actions.reveal(d.project, d.export))}>Reveal</button>}
                      >
                        {(h) => <button type="button" class="small" onClick={() => void openAt(h())}>Open project</button>}
                      </Show>
                    </div>
                  )}
                </For>
                <button type="button" class="ghost small" onClick={() => void attempt(actions.homeSeen).then(refreshHome)}>Mark seen</button>
              </div>
            </Show>
          </section>
        </Show>
      </div>
      <Show when={pair()}>
        <PairDialog
          onClose={() => setPair(false)}
          onOpen={(sound, hires, off, segments) => {
            setPair(false);
            setOpening(true);
            void openVideo(sound, { path: hires, offset_s: off, ...(segments.length ? { segments } : {}) }).then(() => {
              setOpening(false);
              void refetchRecent();
            });
          }}
        />
      </Show>
      <Show when={stranger()}>
        <SetupSheet
          onClose={() => setStranger(false)}
          onDone={() => { setStranger(false); setWelcomeKey(welcomeKey() + 1); }}
        />
      </Show>
      <Show when={restoring()}>
        <RestoreSheet onClose={() => setRestoring(false)} />
      </Show>
      <Show when={connecting()}>
        <ConnectAgent onClose={() => setConnecting(false)} />
      </Show>

      {/* Your projects: each open project once (its picture, its cuts, its actions), then
          the ones opened before, then the rest of the library; search what you said sits
          with them (an empty library has nothing said to search: ⌘⇧F still opens Search) */}
      <section class="home-projects" aria-label="Your projects">
        <div class="home-section-head">
          <h2>Your projects</h2>
          <Show when={projects().length + closedRecent().length + moreLibrary().length}>
            {(n) => <span class="muted">{n()}</span>}
          </Show>
          <span class="spacer" />
          <Show when={projects().length || recent()?.length || home().library?.length}>
            <form class="home-search" role="search" data-testid="home-search" onSubmit={(e) => { e.preventDefault(); setRoute({ view: "search", q: searchQ().trim() }); }}>
              {/* one field: Enter searches (⌘⇧F from anywhere) */}
              <span class="home-search-field">
                <Icon name="search" size={14} />
                <input type="search" placeholder="Search everything you said (Enter)" aria-label="Search everything you said" data-tip="Search every video in your library" data-kbd="⌘⇧F" value={searchQ()} onInput={(e) => setSearchQ(e.currentTarget.value)} />
              </span>
            </form>
          </Show>
        </div>
        <Show when={!projects().length && !closedRecent().length && !moreLibrary().length}>
          <p class="empty">Your projects show here once you open a video.</p>
        </Show>
        {/* keyed by project id: Home refreshing its list (transcription, exports) keeps each
            card, so a menu open on one stays open */}
        <For each={projectIds()}>
          {(pid) => (
            <Show when={projects().find((x) => x.id === pid)}>
              {(p) => (
                <div class="project" classList={{ "has-thumb": !!recentOf(p().id) }}>
                  <Show when={recentOf(p().id)}>{(v) => <RecentCard v={v()} onOpen={() => void openProject(p().id)} />}</Show>
                  <div class="project-body">
                    <header>
                      <h3>{p().name}</h3>
                      {/* the project's actions in its menu (the card stays the picture and its cuts) */}
                      <Menu
                        label={`More for ${p().name}`}
                        icon="more"
                        testid="project-menu"
                        items={() => [
                          { label: "Export 9:16 + 16:9…", icon: "share", testid: "framings-open-home", onSelect: () => setFramingsFor({ project: p().id }) },
                          // starting agents inside gmclip: the developer setting's (decision 0054)
                          ...(prefs.developer
                            ? [
                                { label: "Ask an agent…", icon: "smile", testid: "project-agent-open", onSelect: () => setLaunchFor({ project: p().id }) },
                                { label: "Send an agent…", icon: "bot", testid: "batch-open", onSelect: () => setBatchFor(p()) },
                              ]
                            : []),
                        ]}
                      />
                    </header>
                    <For each={p().cuts} fallback={<p class="empty">No cuts yet.</p>}>
                      {(c) => (
                        // the whole row opens the cut; the project's own main cut (its name again) says "Main cut"
                        <div class="item" data-framed={c.framed ? "yes" : "no"}>
                          <button
                            type="button"
                            class="item-open"
                            disabled={!c.framed}
                            aria-label={`Open ${c.title === p().name ? "Main cut" : c.title}`}
                            data-tip={c.framed ? undefined : "A story cut: grow it into a framed cut (story panel)"}
                            onClick={() => void openCut(p().id, c.id)}
                          >
                            <span class="cut-name">{c.title === p().name ? "Main cut" : c.title}</span>
                            <Show when={c.title === p().name}><span class="sr-only">{c.title}</span></Show>
                            <span class="muted">{c.framed ? `${c.parts} part${c.parts === 1 ? "" : "s"}${c.imported ? " · imported" : ""}` : "story, not framed yet"}</span>
                            <Icon name="chevron-right" size={14} />
                          </button>
                        </div>
                      )}
                    </For>
                    <For each={dropped()}>
                      {(path) => (
                        <div class="item">
                          <span class="muted">Dropped: {baseName(path)}</span>
                          <button type="button" onClick={() => void doImport(p().id, path)}>Import</button>
                        </div>
                      )}
                    </For>
                  </div>
                </div>

              )}
            </Show>
          )}
        </For>
        <Show when={closedRecent().length || moreLibrary().length}>
          <h3 class="home-sub">Opened before</h3>
        </Show>
        <Show when={closedRecent().length}>
          <div class="recent-grid" role="list" aria-label="Recent projects">
            <For each={closedRecent()}>
              {(v) => (
                <div role="listitem">
                  <RecentCard v={v} disabled={opening()} onOpen={() => void open_(v.open ?? undefined)} />
                </div>
              )}
            </For>
          </div>
        </Show>
        <Show when={moreLibrary().length}>
          <section class="library-rows" aria-label="Library">
            <For each={moreLibrary()}>
              {(l) => (
                <div class="item" data-testid="library-row">
                  <span>{l.name}</span>
                  <span class="muted">
                    {l.exports ? `${l.exports} export${l.exports === 1 ? "" : "s"}${l.last_export ? ` · last “${l.last_export.title}”` : ""}` : "no exports"}
                    {l.moments?.total ? ` · ${l.moments.total} moment${l.moments.total === 1 ? "" : "s"}${l.moments.to_grow ? `, ${l.moments.to_grow} to grow` : ""}` : ""}
                  </span>
                  <button type="button" onClick={() => void openAt(l.path)}>Open</button>
                </div>
              )}
            </For>
          </section>
        </Show>
      </section>
      <Show when={home().recent?.length}>
        <section class="projects home-recent" aria-label="Recent exports">
          <h2>Recent exports</h2>
          <For each={home().recent}>
            {(e) => (
              <div class="item" data-testid="recent-export">
                <span class="lib-main">
                  <span class="lib-title" title={e.title}>{e.title}</span>
                  <span class="muted lib-where" title={e.project_name}>{e.project_name} · {ago(e.at_ms)}</span>
                </span>
                <Show
                  when={e.closed_home}
                  fallback={<button type="button" class="small" onClick={() => void attempt(() => actions.reveal(e.project, e.export))}>Reveal</button>}
                >
                  {(h) => <button type="button" class="small" onClick={() => void openAt(h())}>Open project</button>}
                </Show>
              </div>
            )}
          </For>
        </section>
      </Show>
      <Show when={batchFor()}>
        {(p) => <BatchSheet project={p()} onClose={() => setBatchFor(null)} />}
      </Show>
      <Show when={consolidating()}>
        <ConsolidateSheet onClose={() => setConsolidating(false)} />
      </Show>
    </main>
  );
}

/** "Are these the same recording?": the file with the voice (the transcript's clock) and
 *  the hi-res picture of the same stream, with how much later the hi-res file starts. */
function PairDialog(props: {
  onClose: () => void;
  onOpen: (sound: string, hires: string, offsetS: number, segments: { from_s: number; offset_s: number }[]) => void;
}) {
  const [sound, setSound] = createSignal("");
  const [hires, setHires] = createSignal("");
  const [off, setOff] = createSignal("0");
  // later offset changes: from (s on the voice file) → offset (s)
  const [later, setLater] = createSignal<{ from: string; off: string }[]>([]);
  const segments = () =>
    later().length ? [{ from_s: 0, offset_s: Number(off()) }, ...later().map((x) => ({ from_s: Number(x.from), offset_s: Number(x.off) }))] : [];
  const pick = async (set: (s: string) => void) => {
    const p = await attempt(() => actions.pickVideo());
    if (p?.picked) set(p.picked);
  };
  // The offset measured from the two files (sound, then pictures); the human confirms.
  const [measuring, setMeasuring] = createSignal(false);
  const [found, setFound] = createSignal<PairProposal | null>(null);
  let asked = "";
  const measure = async () => {
    const key = `${sound()}\n${hires()}`;
    if (!sound() || !hires() || measuring()) return;
    asked = key;
    setMeasuring(true);
    setFound(null);
    const r = await attempt(() => actions.pairPropose(sound(), hires()));
    setMeasuring(false);
    if (!r || asked !== key) return;
    setFound(r);
    const [first, ...rest] = r.stretches;
    if (first) setOff(first.offset_s.toFixed(3));
    setLater(rest.map((x) => ({ from: x.from_s.toFixed(1), off: x.offset_s.toFixed(3) })));
  };
  // measured once both paths are in and have stopped changing for a moment
  let timer: ReturnType<typeof setTimeout> | undefined;
  createEffect(on([sound, hires], ([a, b]) => {
    clearTimeout(timer);
    if (a && b && `${a}\n${b}` !== asked) timer = setTimeout(() => void measure(), 800);
  }, { defer: true }));
  const conf = (c: number) => (c >= 0.8 ? "high" : c >= 0.5 ? "medium" : "low");
  return (
    <div class="keys-backdrop" onPointerDown={(e) => { if (e.target === e.currentTarget) props.onClose(); }}>
      <section class="keys-dialog" role="dialog" aria-modal="true" aria-label="Same recording">
        <header>
          <h2>Same recording, two files</h2>
          <button type="button" class="ghost" aria-label="Close" onClick={() => props.onClose()}>✕</button>
        </header>
        <p class="muted small">The sound and transcript come from the first file, the picture from the second.</p>
        <div class="setup-row">
          <label>File with your voice (e.g. the proxy)</label>
          <div class="row">
            <input aria-label="Sound file path" value={sound()} onInput={(e) => setSound(e.currentTarget.value)} placeholder="/path/to/stream.proxy.mp4" />
            <button type="button" class="ghost" onClick={() => void pick(setSound)}>Choose…</button>
          </div>
        </div>
        <div class="setup-row">
          <label>Hi-res picture of the same stream</label>
          <div class="row">
            <input aria-label="Hi-res file path" value={hires()} onInput={(e) => setHires(e.currentTarget.value)} placeholder="/path/to/source.mkv" />
            <button type="button" class="ghost" onClick={() => void pick(setHires)}>Choose…</button>
          </div>
        </div>
        <div class="setup-row">
          <label>The hi-res file starts this many seconds after the voice file (negative: before)</label>
          <input aria-label="Offset (s)" type="number" step="0.001" value={off()} onInput={(e) => setOff(e.currentTarget.value)} />
          <Show when={measuring()}>
            <p class="muted small" data-testid="pair-measuring">Measuring the offset from the two files' sound and pictures (about 20 s for an hour)…</p>
          </Show>
          <Show when={found()}>
            {(f) => (
              <div class="small" data-testid="pair-found">
                <For each={f().stretches}>
                  {(x, i) => (
                    <p class="muted">
                      {i() === 0 ? "Measured" : `From ${x.from_s.toFixed(1)} s`}: {x.offset_s.toFixed(3)} s{x.frames !== null ? ` (${x.frames} frames)` : ""} · {conf(x.confidence)} confidence ({Math.round(x.confidence * 100)} %) · {x.picture ? "checked on the pictures" : "from the sound only"}
                    </p>
                  )}
                </For>
                <For each={f().unmatched}>
                  {(u) => <p class="warn">The hi-res file has no picture matching {u.from_s.toFixed(0)}–{u.to_s.toFixed(0)} s of the voice file.</p>}
                </For>
                <p class="muted">Check it and press Open; you can still change it here or frame by frame later in the Look tab.</p>
              </div>
            )}
          </Show>
          <Show when={!measuring() && !found()}>
            <p class="muted small">You can nudge it frame by frame later in the Look tab.</p>
          </Show>
          <button type="button" class="ghost small" data-testid="pair-measure" disabled={!sound() || !hires() || measuring()} onClick={() => { asked = ""; void measure(); }}>Measure the offset</button>
          <For each={later()}>
            {(x, i) => (
              <div class="row small">
                <span>from</span>
                <input aria-label={`Offset change ${i() + 1} at (s)`} type="number" step="0.1" value={x.from} onInput={(e) => setLater(later().map((y, j) => (j === i() ? { ...y, from: e.currentTarget.value } : y)))} />
                <span>s the offset is</span>
                <input aria-label={`Offset change ${i() + 1} (s)`} type="number" step="0.001" value={x.off} onInput={(e) => setLater(later().map((y, j) => (j === i() ? { ...y, off: e.currentTarget.value } : y)))} />
                <button type="button" class="ghost" aria-label="Remove this change" onClick={() => setLater(later().filter((_, j) => j !== i()))}>✕</button>
              </div>
            )}
          </For>
          <button type="button" class="ghost small" onClick={() => setLater([...later(), { from: "", off: off() }])}>＋ The offset changes later</button>
        </div>
        <div class="row">
          <button type="button" class="primary" data-testid="pair-open" disabled={!sound() || !hires() || !Number.isFinite(Number(off()))} onClick={() => props.onOpen(sound(), hires(), Number(off()), segments())}>Open</button>
          <button type="button" class="ghost" onClick={() => props.onClose()}>Cancel</button>
        </div>
      </section>
    </div>
  );
}

/** A project's card: its first video's frame, length, name and folder. */
function RecentCard(props: { v: RecentProject; disabled?: boolean; onOpen: () => void }) {
  const v = () => props.v;
  return (
    <button type="button" class="recent-card" data-missing={v().missing ? "yes" : "no"} disabled={v().missing || !v().open || props.disabled} aria-label={`Edit ${v().name}`} data-tip={v().missing ? "Its videos moved or were deleted" : v().thumb} onClick={() => props.onOpen()}>
      <span class="recent-thumb">
        <RecentThumb path={v().thumb} missing={v().missing} />
        <span class="recent-dur">{dur(v().duration_s)}</span>
      </span>
      <span class="recent-meta">
        <span class="recent-name">{humanTitle(v().name)}</span>
        <span class="recent-path">{v().missing ? "videos moved or deleted" : v().videos === 1 ? (v().thumb.split(isWindows ? /[\\/]/ : "/").slice(-2, -1)[0] ?? v().thumb) : `${v().videos} videos`}</span>
      </span>
    </button>
  );
}
