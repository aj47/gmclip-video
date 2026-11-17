// The project map (decision 0011, SPEC §5.17): always on the left of the cut view at
// desktop size. Videos → Main cut → Topics → Long-forms → Shorts grouped by topic, each
// with its framings, duration and export / review state, so AJ can jump between the main
// cut, a long-form and a short in one project. The videos come first — thumbnail,
// length, rate, transcription, relink when the file moved, remove (refused while cuts
// play it) — and every one of them is on the main cut, the project's stream edit, in the
// order they were added (decision 0036). Titles an agent wrote are text nodes only.
import { createEffect, createSignal, For, on, onCleanup, Show } from "solid-js";
import { actions, ideaActions, type MapCut, type MapSource, type MapStream, type MapTopic } from "../actions";
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
      classList={{ "map-item": true, on: props.c.cut === current(), lit: lit().includes(props.c.cut) }}
      data-testid={`map-${props.c.kind}`}
    >
      <button type="button" class="map-open" onClick={() => open(props.c.cut)} title={props.c.title}>
        <span class="map-title">{props.c.title || "Untitled"}</span>
        <span class="muted small">
          {framings(props.c) || "no framing"} · {clock(props.c.duration_ms)}
          {props.c.kind === "long" && props.c.topics.length ? ` · topics ${topicNames(props.c.topics)}` : ""}
        </span>
      </button>
      <ExportBadge e={props.c.export} />
      <Show when={props.c.parent_edits > 0}>
        <span class="map-badge edits" data-testid="map-edits" title="The stream edit changed inside this long-form's topics">{props.c.parent_edits} edits</span>
      </Show>
      <Show when={props.c.review}>
        <span class="map-badge review" title="Last review">{props.c.review}</span>
      </Show>
      <Show when={props.c.kind === "long"}>
        <IconButton icon="fork" size="sm" class="map-fork" data-testid="longform-fork" label={`Fork ${props.c.title}`} hint="Fork: a sibling long-form to try another order (same topics; this one is untouched)" onClick={() => void fork(props.c)} />
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
  return (
    <nav class="project-map" aria-label="Project map" data-testid="project-map">
      <Sources />
      <Show
        when={streams().length}
        fallback={
          <div class="map-sec">
            <h2 title="The whole stream, where topics are labelled and fluff struck for everything made from it">Stream edit</h2>
            <button type="button" class="small" data-testid="stream-adopt" title="Make this cut the project's stream edit" onClick={() => void makeStream(true)}>Use this cut</button>{" "}
            <button type="button" class="ghost small" title="A new cut covering the whole stream" onClick={() => void makeStream(false)}>New</button>
          </div>
        }
      >
        <For each={streams()}>
          {(s) => (
            <div class="map-sec">
              <button
                type="button"
                classList={{ "map-head": true, on: s.cut === current() }}
                data-testid="map-stream"
                data-tip="Every video of the project, in the order added: cut, strike and label topics here; long-forms and shorts are made from it"
                onClick={() => open(s.cut)}
              >
                <b>Main cut</b>
                <span class="muted small">
                  {(s.videos?.length ?? 1) > 1 ? `${s.videos!.length} videos · ` : ""}
                  {clock(s.total_ms)} → {clock(s.kept_ms)} kept
                </span>
              </button>
              <h3>
                Topics ({s.topics.filter((t) => t.state !== "rejected").length})
                <Show when={s.fluff_proposed > 0}>
                  <span class="map-badge edits" title="Fluff your agent proposed, waiting for you">{s.fluff_proposed} fluff?</span>
                </Show>
              </h3>
              <ol class="map-topics">
                <For each={s.topics.filter((t) => t.state !== "rejected")} fallback={<li class="muted small" title="Select words in the transcript and press T">None yet</li>}>
                  {(t) => (
                    <li classList={{ proposed: t.state === "proposed", conflict: t.conflict }}>
                      <button type="button" class="map-open" onClick={() => goTopic(t, s)} title={t.title}>
                        <span class="dot" style={{ background: t.state === "proposed" ? "transparent" : t.colour, "border-color": t.colour }} />
                        <span class="map-title">{t.title}</span>
                        <span class="muted small">
                          {clock(t.kept_ms)}
                          {(s.videos?.length ?? 1) > 1 ? ` · ${videoName(t.media)}` : ""}
                          {t.state === "proposed" ? " · proposed" : ""}
                        </span>
                      </button>
                    </li>
                  )}
                </For>
              </ol>
            </div>
          )}
        </For>
      </Show>
      <div class="map-sec">
        <h3>
          Long-forms ({m()?.longs.length ?? 0})
          <button type="button" class="ghost small" data-testid="longform-new" disabled={!topics().some((t) => t.state === "accepted")} onClick={() => setLongformOpen(!longformOpen())}>
            ＋ From topics…
          </button>
          <Show when={(m()?.longs.length ?? 0) > 1}>
            <button type="button" class="ghost small" data-testid="longform-compare-open" data-tip="Two long-forms side by side: topics, order, lengths; pick one or keep both" onClick={compare}>
              Compare…
            </button>
          </Show>
        </h3>
        <ul class="map-list">
          <For each={m()?.longs ?? []} fallback={<li class="muted small">None yet</li>}>{(c) => <Row c={c} />}</For>
        </ul>
      </div>
      <Framings />
      <div class="map-sec">
        <h3>Shorts ({m()?.shorts.length ?? 0})</h3>
        <For each={[...topics(), null]}>
          {(t) => (
            <Show when={shortsOf(t).length}>
              <div class="map-group">
                <span class="small map-group-title">{t ? t.title : "No topic"}</span>
                <ul class="map-list">
                  <For each={shortsOf(t)}>{(c) => <Row c={c} />}</For>
                </ul>
              </div>
            </Show>
          )}
        </For>
      </div>
    </nav>
  );
}

/** This cut's framings: which one the viewer shows and exports, and another one of the
 *  same shape ("screen focus" and "face big" on one short, sharing words and captions). */
function Framings() {
  const [adding, setAdding] = createSignal(false);
  const [name, setName] = createSignal("");
  const list = () => cutView()?.framing?.list ?? [];
  const active = () => cutView()?.framing?.active ?? null;
  const add = async () => {
    const v = cutView();
    const n = name().trim();
    if (!v || !n) return;
    const portrait = list().find((f) => f.h > f.w);
    const aspect = v.kind === "short" || portrait ? "9:16" : "16:9";
    const r = await attempt(() => actions.framingAdd(v.project, v.cut, n, aspect));
    if (r) {
      setAdding(false);
      setName("");
      await attempt(() => actions.framingPick(v.project, v.cut, r.framing));
      await refreshCut();
      say(`Framing “${n}” added — its own crops and layouts, the same words and captions`);
    }
  };
  return (
    <div class="map-sec" data-testid="map-framings">
      <h3>
        Framings of this cut
        <button type="button" class="ghost small" data-testid="framing-new" onClick={() => setAdding(!adding())}>＋</button>
      </h3>
      <ul class="map-list">
        <For each={list()}>
          {(f) => (
            <li classList={{ "map-item": true, on: f.id === active() }}>
              <button
                type="button"
                class="map-open"
                aria-pressed={f.id === active()}
                onClick={() => { const v = cutView(); if (v) void attempt(() => actions.framingPick(v.project, v.cut, f.id)).then(() => refreshCut()); }}
              >
                <span class="map-title">{f.name}</span>
                <span class="muted small">{f.w}×{f.h}</span>
              </button>
            </li>
          )}
        </For>
      </ul>
      <Show when={adding()}>
        <form class="row" onSubmit={(e) => { e.preventDefault(); void add(); }}>
          <input type="text" class="map-input" aria-label="Framing name" placeholder="e.g. face big" value={name()} onInput={(e) => setName(e.currentTarget.value)} />
          <button type="submit" class="small" data-testid="framing-add">Add</button>
        </form>
      </Show>
    </div>
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

/** The project's videos: one row each (a same-recording pair shows both files, one
 * video). ＋ adds more; a moved file relinks; remove is refused while cuts play it. */
function Sources() {
  const m = () => projectMapData();
  const list = () => m()?.sources ?? [];
  const [refused, setRefused] = createSignal<string | null>(null);
  const [confirm, setConfirm] = createSignal<string | null>(null);
  const [q, setQ] = createSignal("");
  const titleOf = (cut: string) => [...(m()?.longs ?? []), ...(m()?.shorts ?? [])].find((c) => c.cut === cut)?.title ?? "a cut";
  const remove = async (x: MapSource) => {
    const v = cutView();
    if (!v) return;
    if (x.used_by.length) {
      setRefused(x.id);
      return;
    }
    if (list().length < 2) {
      say("This is the project's only video");
      return;
    }
    if (confirm() !== x.id) {
      setConfirm(x.id);
      return;
    }
    setConfirm(null);
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
  return (
    <div class="map-sec map-sources" data-testid="map-sources">
      <h3>
        Videos ({list().length})
        <button type="button" class="ghost small" data-testid="add-video" data-tip="Add videos to this project (several at once): each goes on the end of the main cut, with its transcript" data-kbd="⌘⇧O" onClick={() => void addVideosPick()}>
          ＋ Add video…
        </button>
      </h3>
      <ul class="map-list">
        <For each={list()}>
          {(x) => (
            <li class="map-source" data-testid="map-source" data-name={x.name} data-asr={asrText(x).tone} data-offline={x.offline ? "yes" : "no"}>
              <button
                type="button"
                classList={{ "map-open": true, "map-source-open": true, on: !!x.stream && x.stream === cutView()?.cut }}
                disabled={!x.stream}
                data-tip={x.path}
                onClick={() => x.stream && void openCut(cutView()!.project, x.stream)}
              >
                <span class="map-thumb"><RecentThumb path={x.path} missing={!!x.offline} /></span>
                <span class="map-source-meta">
                  <span class="map-title">{x.name}</span>
                  <span class="muted small">
                    {clock(x.duration_ms)}
                    {x.width ? ` · ${x.width}×${x.height}` : ""}
                    {x.fps ? ` · ${fmtFps(x.fps)}` : ""}
                    {x.group ? ` · same recording (${x.group_role === "picture" ? "picture" : "sound"})` : ""}
                  </span>
                  <span class={`map-asr small ${asrText(x).tone}`} data-testid="map-source-asr">{x.offline ? "file moved or deleted" : asrText(x).text}</span>
                </span>
              </button>
              <Show when={!x.offline && (x.width ?? 0) > (x.height ?? 0)}>
                <span class="map-layout small" data-testid="map-source-layout" data-layout={x.layout ?? ""}>
                  <Show
                    when={x.layout}
                    fallback={
                      <button type="button" class="small primary" data-testid="draw-layout-open" data-tip="Box the screen, camera, chat and private parts of this video once: every new cut uses them" onClick={() => { const p = cutView()?.project; if (p) setDrawLayout({ project: p, source: x.id }); }}>
                        Draw layout…
                      </button>
                    }
                  >
                    <button type="button" class="ghost small" data-testid="draw-layout-open" data-tip={`Its layout boxes: ${(x.layout_roles ?? []).join(", ")}. Click to redraw`} onClick={() => { const p = cutView()?.project; if (p) setDrawLayout({ project: p, source: x.id }); }}>
                      Layout: {x.layout}
                    </button>
                  </Show>
                </span>
              </Show>
              <span class="map-source-actions">
                <Show when={x.offline}>
                  <button type="button" class="small" data-testid="relink" data-tip="Find the moved file" onClick={() => void relink(x)}>Relink…</button>
                </Show>
                <Show when={!x.offline}>
                  <button
                    type="button"
                    class="ghost small"
                    data-testid="video-agent-open"
                    aria-label={`Ask an agent about ${x.name}`}
                    data-tip="Start your own agent on this video"
                    onClick={() => { const v = cutView(); if (v) setLaunchFor({ project: v.project, source: x.id }); }}
                  >
                    <Icon name="smile" size={13} />
                  </button>
                </Show>
                <button
                  type="button"
                  class="ghost small"
                  data-testid="remove-video"
                  aria-label={`Take ${x.name} out of this project`}
                  data-tip={x.used_by.length ? `Used by ${x.used_by.length} cut${x.used_by.length === 1 ? "" : "s"}` : "Take it and its stretch of the main cut out (⌘Z brings it back)"}
                  onClick={() => void remove(x)}
                >
                  {confirm() === x.id ? "Remove?" : "✕"}
                </button>
              </span>
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
