// The project map (decision 0011, SPEC §5.17): always on the left of the cut view at
// desktop size. Videos → Main cut → Topics → Long-forms → Shorts grouped by topic, each
// with its framings, duration and export / review state, so AJ can jump between the main
// cut, a long-form and a short in one project. The videos come first — thumbnail,
// length, rate, transcription, relink when the file moved, remove (refused while cuts
// play it) — and every one of them is on the main cut, the project's stream edit, in the
// order they were added (decision 0036). Titles an agent wrote are text nodes only.
import { createEffect, createSignal, For, on, onCleanup, Show, type JSX } from "solid-js";
import { actions, ideaActions, type MapCut, type MapSource, type MapStream, type MapTopic, type MediaRef } from "../actions";
import { addVideosPick, analysisTick, attempt, cutView, openCut, refreshCut, say, setRoute } from "../store";
import { RecentThumb } from "./VideoThumb";
import { IconButton } from "../design/components";
import { Icon } from "../design/Icon";
import { setLaunchFor } from "../agentsState";
import { lit, longformOpen, projectMapData, refreshMap, setLongformOpen, setPendingTopic } from "../streamState";
import { compareLongsFor, setDrawLayout } from "../ideasState";

function clock(ms: number): string {
  const t = Math.round(ms / 1000);
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const s = String(t % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

function framings(c: MapCut): string {
  const by = new Map<string, string[]>();
  for (const f of c.framings) by.set(f.aspect, [...(by.get(f.aspect) ?? []), f.name]);
  return [...by.entries()].map(([a, names]) => (names.length > 1 ? `${a} ×${names.length} (${names.join(", ")})` : a)).join(" · ");
}

function ExportBadge(props: { e?: { state: string } }) {
  const s = () => props.e?.state ?? "never";
  return (
    <span class={`map-badge ${s()}`} role="img" aria-label={s() === "fresh" ? "Exported, up to date" : s() === "stale" ? "Exported, but the cut changed since" : "Not exported yet"} data-tip={s() === "fresh" ? "Exported, up to date" : s() === "stale" ? "Exported, but the cut changed since" : "Not exported yet"}>
      {s() === "fresh" ? "✓" : s() === "stale" ? "↻" : "–"}
    </span>
  );
}

export function ProjectMap() {
  const project = () => cutView()?.project;
  createEffect(on(() => [project(), cutView()?.rev], () => {
    const p = project();
    if (p) void refreshMap(p);
  }));
  // transcription progress reaches the Videos rows, at most once a second
  let tickTimer: ReturnType<typeof setTimeout> | undefined;
  createEffect(on(analysisTick, () => {
    if (tickTimer) return;
    tickTimer = setTimeout(() => {
      tickTimer = undefined;
      const p = project();
      if (p) void refreshMap(p);
    }, 1000);
  }, { defer: true }));
  onCleanup(() => clearTimeout(tickTimer));
  const m = () => projectMapData();
  const open = (cut: string) => {
    const p = project();
    if (p) void openCut(p, cut);
  };
  const current = () => cutView()?.cut;
  const streams = (): MapStream[] => m()?.streams ?? (m()?.stream ? [m()!.stream!] : []);
  const topics = () => streams().flatMap((s) => s.topics).filter((t) => t.state !== "rejected");
  const shortsOf = (t: MapTopic | null) => (m()?.shorts ?? []).filter((s) => (t ? s.topics[0] === t.id : !s.topics.length || !topics().some((x) => x.id === s.topics[0])));
  const topicNames = (ids: string[]) =>
    ids
      .map((id) => topics().findIndex((t) => t.id === id) + 1)
      .filter((n) => n > 0)
      .join(", ");
  const makeStream = async (adopt: boolean) => {
    const v = cutView();
    if (!v) return;
    const r = await attempt(() => actions.streamEditEnsure(v.project, adopt ? v.cut : undefined));
    if (r) {
      say(r.how === "adopted" ? "This cut is now the stream edit" : "Stream edit made");
      await refreshMap(v.project);
      if (r.cut !== v.cut) await openCut(v.project, r.cut);
    }
  };
  // a video's name, as the Videos rows show it (a topic of a several-video main cut)
  const videoName = (media: MapTopic["media"]) => {
    const key = "group" in media ? media.group : media.source;
    return (m()?.sources ?? []).find((x) => x.id === key || x.group === key)?.name ?? "";
  };
  const goTopic = (t: MapTopic, s: MapStream) => {
    // the topic's words, selected once the stream edit shows its transcript
    setPendingTopic(t.id);
    if (s.cut !== current()) open(s.cut);
  };
  const Row = (props: { c: MapCut }) => (
    <li
      classList={{ "map-item": true, "map-node": true, on: props.c.cut === current(), lit: lit().includes(props.c.cut) }}
      data-testid={`map-${props.c.kind}`}
    >
      <button type="button" class="map-open" onClick={() => open(props.c.cut)} title={props.c.title} data-tip={framings(props.c) || undefined}>
        <span class="map-title">{props.c.title || "Untitled"}</span>
        <span class="muted small">
          {props.c.kind === "long" ? "Long video" : "Short"} · {clock(props.c.duration_ms)}
          {props.c.kind === "long" && props.c.topics.length ? ` · topics ${topicNames(props.c.topics)}` : ""}
        </span>
      </button>
      <Show when={(props.c.export?.state ?? "never") !== "never"}>
        <ExportBadge e={props.c.export} />
      </Show>
      <Show when={props.c.parent_edits > 0}>
        <span class="map-badge edits" data-testid="map-edits" title="The stream edit changed inside this long-form's topics">{props.c.parent_edits} edits</span>
      </Show>
      <Show when={props.c.review}>
        <span class="map-badge review" title="Last review">{props.c.review}</span>
      </Show>
      <Show when={props.c.kind === "long"}>
        <IconButton icon="fork" size="sm" class="map-fork" data-testid="longform-fork" label={`Fork ${props.c.title}`} hint="Try another order: a copy of this long video with the same topics (this one stays as it is)" onClick={() => void fork(props.c)} />
      </Show>
    </li>
  );
  const fork = async (c: MapCut) => {
    const p = project();
    if (!p) return;
    const r = await attempt(() => ideaActions.longformFork(p, c.cut));
    if (!r) return;
    say(`Forked “${c.title}” — reorder its parts, then Compare`);
    await refreshMap(p);
    await openCut(p, r.cut);
  };
  const compare = () => {
    const p = project();
    if (p) compareLongsFor(p, m()?.longs ?? [], current());
  };
  // the tree (AJ 2026-11-22): each video, its cuts under it (the main cut under the
  // first video it holds; a short or long video under the video its first part plays)
  const keyOf = (r: MediaRef) => ("group" in r ? `g:${r.group}` : `s:${r.source}`);
  const sources = () => m()?.sources ?? [];
  const homeOf = (refs: MediaRef | null | undefined): string | undefined => {
    if (!refs) return undefined;
    const k = keyOf(refs);
    return sources().find((x) => keyOf(x.media) === k)?.id;
  };
  const firstId = () => sources()[0]?.id;
  const streamHome = (s: MapStream) => homeOf(s.videos?.[0] ?? s.media ?? null) ?? firstId();
  const cutHome = (c: MapCut) => homeOf(c.media?.[0]) ?? firstId();
  const accepted = () => topics().some((t) => t.state === "accepted");
  const cutsUnder = (id: string | undefined) => ({
    streams: streams().filter((x) => streamHome(x) === id),
    longs: (m()?.longs ?? []).filter((c) => cutHome(c) === id),
    shorts: (m()?.shorts ?? []).filter((c) => cutHome(c) === id),
  });
  const StreamNode = (props: { s: MapStream }) => {
    const s = () => props.s;
    const shown = () => s().topics.filter((t) => t.state !== "rejected");
    return (
      <li class="map-node map-stream-node">
        <button
          type="button"
          classList={{ "map-head": true, "map-open": true, on: s().cut === current() }}
          data-testid="map-stream"
          data-tip="Every video of the project, in the order added: cut, strike and label topics here; long videos and shorts are made from it"
          onClick={() => open(s().cut)}
        >
          <span class="map-title">Main cut</span>
          <span class="muted small">
            {(s().videos?.length ?? 1) > 1 ? `${s().videos!.length} videos · ` : ""}
            {clock(s().total_ms)} → {clock(s().kept_ms)} kept
          </span>
        </button>
        {/* topics only once there is one (how to make one is in the keys help: T) */}
        <Show when={shown().length || s().fluff_proposed > 0}>
          <div class="map-topics-wrap">
            <h3>
              Topics ({shown().length})
              <Show when={s().fluff_proposed > 0}>
                <span class="map-badge edits" title="Fluff your agent proposed, waiting for you">{s().fluff_proposed} fluff?</span>
              </Show>
            </h3>
            <ol class="map-topics">
              <For each={shown()}>
                {(t) => (
                  <li classList={{ proposed: t.state === "proposed", conflict: t.conflict }}>
                    <button type="button" class="map-open" onClick={() => goTopic(t, s())} title={t.title}>
                      <span class="dot" style={{ background: t.state === "proposed" ? "transparent" : t.colour, "border-color": t.colour }} />
                      <span class="map-title">{t.title}</span>
                      <span class="muted small">
                        {clock(t.kept_ms)}
                        {(s().videos?.length ?? 1) > 1 ? ` · ${videoName(t.media)}` : ""}
                        {t.state === "proposed" ? " · proposed" : ""}
                      </span>
                    </button>
                  </li>
                )}
              </For>
            </ol>
          </div>
        </Show>
        <Show when={accepted() || (m()?.longs.length ?? 0) > 1}>
          <div class="map-node-actions">
            <button type="button" class="ghost small" data-testid="longform-new" disabled={!accepted()} onClick={() => setLongformOpen(!longformOpen())}>
              ＋ Long video from topics…
            </button>
            <Show when={(m()?.longs.length ?? 0) > 1}>
              <button type="button" class="ghost small" data-testid="longform-compare-open" data-tip="Two long videos side by side: topics, order, lengths; pick one or keep both" onClick={compare}>
                Compare…
              </button>
            </Show>
          </div>
        </Show>
      </li>
    );
  };
  const CutNodes = (props: { id: string | undefined }) => (
    <>
      <For each={cutsUnder(props.id).streams}>{(x) => <StreamNode s={x} />}</For>
      <For each={cutsUnder(props.id).longs}>{(c) => <Row c={c} />}</For>
      <For each={cutsUnder(props.id).shorts}>{(c) => <Row c={c} />}</For>
    </>
  );
  return (
    <nav class="project-map" aria-label="Project map" data-testid="project-map">
      <Sources cuts={(id) => <CutNodes id={id} />} />
      {/* no videos (an imported plan): the cuts on their own */}
      <Show when={!sources().length}>
        <ul class="map-tree map-cuts">
          <CutNodes id={undefined} />
        </ul>
      </Show>
      <Show when={!streams().length}>
        <div class="map-sec">
          <h2 title="The whole stream, where topics are labelled and fluff struck for everything made from it">Stream edit</h2>
          <button type="button" class="small" data-testid="stream-adopt" title="Make this cut the project's stream edit" onClick={() => void makeStream(true)}>Use this cut</button>{" "}
          <button type="button" class="ghost small" title="A new cut covering the whole stream" onClick={() => void makeStream(false)}>New</button>
        </div>
      </Show>
    </nav>
  );
}

function fmtFps(f: MapSource["fps"]): string {
  if (!f) return "";
  const r = f.num / Math.max(1, f.den);
  return `${Number.isInteger(r) ? r : r.toFixed(2)} fps`;
}

function asrText(x: MapSource): { text: string; tone: "ok" | "busy" | "wait" | "bad" } {
  const a = x.asr;
  if (!a) return { text: "not transcribed", tone: "wait" };
  if (a.asr_error) return { text: "no transcript", tone: "bad" };
  if (a.phase === "queued") return { text: "waiting to transcribe", tone: "wait" };
  if (a.phase === "transcribing") return { text: `transcribing ${clock(a.done_s * 1000)} / ${clock(a.total_s * 1000)}`, tone: "busy" };
  if (a.phase === "analysing") return { text: "transcribed · analysing audio", tone: "busy" };
  if (a.phase === "failed") return { text: "analysis failed", tone: "bad" };
  if (a.transcript || a.phase === "done") return { text: "transcribed", tone: "ok" };
  return { text: "not transcribed", tone: "wait" };
}

/** The project's videos, each with its cuts under it (the tree). A row is the video's
 * picture, name and length, and its transcription only while it is not done; its other
 * actions (draw layout, ask an agent, take it out) are in its ⋯ menu and on a right click.
 * A moved file says so and offers Relink at once. */
function Sources(props: { cuts: (id: string) => JSX.Element }) {
  const m = () => projectMapData();
  const list = () => m()?.sources ?? [];
  const [refused, setRefused] = createSignal<string | null>(null);
  const [confirm, setConfirm] = createSignal<string | null>(null);
  const [menu, setMenu] = createSignal<{ id: string; x: number; y: number } | null>(null);
  const [q, setQ] = createSignal("");
  const titleOf = (cut: string) => [...(m()?.longs ?? []), ...(m()?.shorts ?? [])].find((c) => c.cut === cut)?.title ?? "a cut";
  // the menu closes on a click elsewhere and on Esc
  const away = (e: Event) => { if (!(e.target as HTMLElement).closest(".map-menu, [data-testid=video-menu]")) setMenu(null); };
  const esc = (e: KeyboardEvent) => { if (e.key === "Escape") setMenu(null); };
  createEffect(() => {
    if (!menu()) return;
    addEventListener("pointerdown", away, true);
    addEventListener("keydown", esc);
    onCleanup(() => { removeEventListener("pointerdown", away, true); removeEventListener("keydown", esc); });
  });
  const openMenu = (x: MapSource, at: { x: number; y: number }) => {
    setConfirm(null);
    setMenu(menu()?.id === x.id ? null : { id: x.id, x: at.x, y: at.y });
  };
  const remove = async (x: MapSource) => {
    const v = cutView();
    if (!v) return;
    if (x.used_by.length) {
      setMenu(null);
      setRefused(x.id);
      return;
    }
    if (list().length < 2) {
      setMenu(null);
      say("This is the project's only video");
      return;
    }
    if (confirm() !== x.id) {
      setConfirm(x.id);
      return;
    }
    setConfirm(null);
    setMenu(null);
    const r = await attempt(() => actions.removeVideo(v.project, x.id));
    if (r) {
      say(`Removed “${x.name}”${r.moments ? ` and its ${r.moments} moment${r.moments === 1 ? "" : "s"}` : ""} — ⌘Z brings it back`);
      await refreshMap(v.project);
      await refreshCut();
    }
  };
  const relink = async (x: MapSource) => {
    const v = cutView();
    if (!v) return;
    const r = await attempt(() => actions.relinkVideo(v.project, x.id));
    if (r?.relinked) {
      say(`“${x.name}” found again`);
      await refreshMap(v.project);
    }
  };
  const draw = (x: MapSource) => {
    setMenu(null);
    const p = cutView()?.project;
    if (p) setDrawLayout({ project: p, source: x.id });
  };
  return (
    <div class="map-sec map-sources" data-testid="map-sources">
      <h3>
        Videos
        <button type="button" class="ghost small" data-testid="add-video" data-tip="Add videos to this project (several at once): each goes on the end of the main cut, with its transcript" data-kbd="⌘⇧O" onClick={() => void addVideosPick()}>
          ＋ Add video…
        </button>
      </h3>
      <ul class="map-tree">
        <For each={list()}>
          {(x) => (
            <li class="map-source" data-testid="map-source" data-name={x.name} data-asr={asrText(x).tone} data-offline={x.offline ? "yes" : "no"}>
              <div class="map-video" onContextMenu={(e) => { e.preventDefault(); openMenu(x, { x: e.clientX, y: e.clientY }); }}>
                <button
                  type="button"
                  classList={{ "map-open": true, "map-source-open": true }}
                  disabled={!x.stream}
                  data-tip={`${x.path}${x.width ? ` · ${x.width}×${x.height}` : ""}${x.fps ? ` · ${fmtFps(x.fps)}` : ""}`}
                  onClick={() => x.stream && void openCut(cutView()!.project, x.stream)}
                >
                  <span class="map-thumb"><RecentThumb path={x.path} missing={!!x.offline} /></span>
                  <span class="map-source-meta">
                    <span class="map-title">{x.name}</span>
                    <span class="muted small">
                      {clock(x.duration_ms)}
                      {x.group ? ` · same recording (${x.group_role === "picture" ? "picture" : "sound"})` : ""}
                    </span>
                    {/* the transcription only while it is not done (done is the normal state) */}
                    <span class={`map-asr small ${asrText(x).tone}`} data-testid="map-source-asr" hidden={!x.offline && asrText(x).tone === "ok"}>{x.offline ? "file moved or deleted" : asrText(x).text}</span>
                  </span>
                </button>
                <Show when={x.offline}>
                  <button type="button" class="small" data-testid="relink" data-tip="Find the moved file" onClick={() => void relink(x)}>Relink…</button>
                </Show>
                {/* the video's menu: its layout state stays readable on this wrapper */}
                <span class="map-source-layout" data-testid="map-source-layout" data-layout={x.layout ?? ""}>
                  <button
                    type="button"
                    class="ghost small ibtn ibtn-sm map-more"
                    data-testid="video-menu"
                    aria-label={`More for ${x.name}`}
                    aria-haspopup="menu"
                    aria-expanded={menu()?.id === x.id}
                    data-tip="Draw layout, ask an agent, take it out (or right-click the video)"
                    onClick={(e) => { const b = e.currentTarget.getBoundingClientRect(); openMenu(x, { x: b.left, y: b.bottom + 4 }); }}
                  >
                    <Icon name="more" size={14} />
                  </button>
                  <Show when={menu()?.id === x.id ? menu() : null}>
                    {(at) => (
                      <div class="map-menu" role="menu" aria-label={`${x.name}`} style={{ left: `${Math.min(at().x, innerWidth - 248)}px`, top: `${Math.min(at().y, innerHeight - 160)}px` }}>
                        <Show when={!x.offline && (x.width ?? 0) > (x.height ?? 0)}>
                          <button type="button" role="menuitem" class="menu-item" data-testid="draw-layout-open" data-tip={x.layout ? `Its layout boxes: ${(x.layout_roles ?? []).join(", ")}` : "Box the screen, camera, chat and private parts of this video once: every new cut uses them"} onClick={() => draw(x)}>
                            <Icon name="layout" size={14} />{x.layout ? `Layout: ${x.layout} (redraw…)` : "Draw layout…"}
                          </button>
                        </Show>
                        <Show when={!x.offline}>
                          <button type="button" role="menuitem" class="menu-item" data-testid="video-agent-open" onClick={() => { setMenu(null); const v = cutView(); if (v) setLaunchFor({ project: v.project, source: x.id }); }}>
                            <Icon name="smile" size={14} />Ask an agent about it…
                          </button>
                        </Show>
                        <button
                          type="button"
                          role="menuitem"
                          class="menu-item danger"
                          data-testid="remove-video"
                          aria-label={`Take ${x.name} out of this project`}
                          data-tip={x.used_by.length ? `Used by ${x.used_by.length} cut${x.used_by.length === 1 ? "" : "s"}` : "Take it and its stretch of the main cut out (⌘Z brings it back)"}
                          onClick={() => void remove(x)}
                        >
                          <Icon name="x" size={14} />{confirm() === x.id ? "Take it out? Click again" : "Take out of this project"}
                        </button>
                      </div>
                    )}
                  </Show>
                </span>
              </div>
              <Show when={refused() === x.id}>
                <div class="map-refused small" role="alert" data-testid="remove-refused">
                  Can’t remove: these cuts play it —{" "}
                  <For each={x.used_by}>
                    {(c, i) => (
                      <>
                        {i() ? ", " : ""}
                        <button type="button" class="linkish" onClick={() => void openCut(cutView()!.project, c)}>{titleOf(c)}</button>
                      </>
                    )}
                  </For>
                  <button type="button" class="ghost small" aria-label="Dismiss" onClick={() => setRefused(null)}>✕</button>
                </div>
              </Show>
              <ul class="map-cuts">{props.cuts(x.id)}</ul>
            </li>
          )}
        </For>
      </ul>
      <Show when={list().length > 1}>
        <form class="row map-search" role="search" onSubmit={(e) => { e.preventDefault(); const p = cutView()?.project; if (p && q().trim()) setRoute({ view: "search", q: q().trim(), project: p }); }}>
          <input type="search" class="map-input" aria-label="Search this project's videos" placeholder="Search all these videos…" value={q()} onInput={(e) => setQ(e.currentTarget.value)} />
        </form>
      </Show>
    </div>
  );
}
