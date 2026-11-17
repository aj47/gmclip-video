// Library search (lane L, L.01/L.02): everything said across every project in the library.
// ⌘⇧F from anywhere, or the search box on Home. A hit plays its span (▶), opens the cut that
// shows it, or becomes a moment (＋). Words are exact; Meaning finds what was talked about;
// Both mixes them. Transcript text is shown as text, never as markup.
import { createEffect, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { ideaActions, libraryActions, type LibraryHit, type LibrarySearchResult, type LibraryTopic } from "../actions";
import { Viewer } from "../components/Viewer";
import { Button, Chip, Empty, IconButton, SegmentedControl } from "../design/components";
import { attempt, openCut, refreshProjects, say, setRoute, watchProject } from "../store";
import { actions } from "../actions";
import "./library.css";
import "../components/ideas.css";

type Mode = "hybrid" | "words" | "meaning";

const MODE_KEY = "gmclip.search.mode";

function savedMode(): Mode {
  try {
    const m = localStorage.getItem(MODE_KEY);
    return m === "words" || m === "meaning" || m === "hybrid" ? m : "hybrid";
  } catch {
    return "hybrid";
  }
}

export function clock(s: number): string {
  const t = Math.floor(s);
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const sec = String(t % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

export function Search(props: { q?: string; project?: string }) {
  // a project's map searches only its videos (every video of it); ✕ widens to the library
  const [only, setOnly] = createSignal(props.project);
  const [q, setQ] = createSignal(props.q ?? "");
  const [mode, setMode] = createSignal<Mode>(savedMode());
  const [res, setRes] = createSignal<LibrarySearchResult | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [playing, setPlaying] = createSignal<string | null>(null);
  const [at, setAt] = createSignal(0);
  let input: HTMLInputElement | undefined;
  let seq = 0;

  const run = async () => {
    const text = q().trim();
    if (!text) {
      setRes(null);
      return;
    }
    const n = ++seq;
    setBusy(true);
    const r = await attempt(() => libraryActions.search(text, mode(), 30, only()));
    if (n !== seq) return;
    setBusy(false);
    if (r) {
      setRes(r);
      setAt(0);
    }
  };
  // search as you type, settled for 250 ms
  let timer: ReturnType<typeof setTimeout> | undefined;
  createEffect(
    on([q, mode, only], () => {
      clearTimeout(timer);
      timer = setTimeout(() => void run(), 250);
    }),
  );
  onCleanup(() => clearTimeout(timer));
  onMount(() => input?.focus());

  const key = (h: LibraryHit) => `${h.project}:${h.media.source ?? h.media.group}:${h.w0}`;
  const play = async (h: LibraryHit) => {
    const r = await attempt(() => libraryActions.play(h));
    if (r) {
      setPlaying(key(h));
      if (!h.open) void refreshProjects();
    }
  };
  const openIn = async (h: LibraryHit) => {
    const r = await attempt(() => libraryActions.open(h));
    if (!r) return;
    await attempt(() => watchProject(r.project));
    await refreshProjects();
    await openCut(r.project, r.cut);
    if (r.frame !== null) await attempt(() => actions.viewerSeek(r.frame!));
  };
  // a topic result (K.04): opens in the long-form that plays it, at its chapter; else in
  // the stream edit at the topic's start
  const openTopic = async (t: LibraryTopic) => {
    const r = await attempt(() => ideaActions.topicOpen(t));
    if (!r) return;
    await attempt(() => watchProject(r.project));
    await refreshProjects();
    await openCut(r.project, r.cut);
    if (r.frame !== null) await attempt(() => actions.viewerSeek(r.frame!));
    say(r.in === "long" ? `“${t.title}” in the long-form “${r.title ?? ""}”` : `“${t.title}” in the stream edit (no long-form plays it yet)`);
  };
  const topics = () => res()?.topics ?? [];
  const makeMoment = async (h: LibraryHit) => {
    const r = await attempt(() => libraryActions.moment(h, q().trim()));
    if (!r) return;
    await attempt(() => watchProject(r.project));
    say(`Moment added to ${h.project_name} · ${r.moment.words} words`);
  };

  const hits = () => res()?.hits ?? [];
  const onKey = (e: KeyboardEvent) => {
    const list = hits();
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setAt(Math.min(at() + 1, list.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setAt(Math.max(at() - 1, 0));
    } else if (e.key === "Enter" && list[at()]) {
      e.preventDefault();
      void play(list[at()]!);
    } else if (e.key === "Escape") {
      e.preventDefault();
      void attempt(actions.viewerPause);
      setRoute({ view: "home" });
    }
  };

  const meaningNote = () => {
    const m = res()?.meaning;
    if (!m || mode() === "words") return null;
    if (m.state === "ready") return null;
    if (m.state === "building") return `Meaning index: ${m.pending ?? 0} transcripts to go — word hits meanwhile`;
    if (m.state === "no_model") return "Meaning search needs its model (90 MB, once); word hits meanwhile";
    if (m.state === "failed") return `Meaning search failed: ${m.error ?? "unknown"}`;
    return null;
  };

  return (
    <main class="libsearch" aria-label="Search the library" data-testid="library-search">
      <header class="libsearch-head">
        <form class="libsearch-form" role="search" onSubmit={(e) => { e.preventDefault(); void run(); }}>
          <input
            ref={input}
            type="search"
            aria-label="Search everything you said"
            data-testid="library-search-input"
            placeholder="Search everything you said…"
            value={q()}
            onInput={(e) => setQ(e.currentTarget.value)}
            onKeyDown={onKey}
          />
        </form>
        <SegmentedControl<Mode>
          label="Search by"
          testid="library-search-mode"
          size="sm"
          value={mode()}
          onChange={(m) => {
            setMode(m);
            try {
              localStorage.setItem(MODE_KEY, m);
            } catch {
              // private storage: the mode is just not remembered
            }
          }}
          options={[
            { value: "hybrid", label: "Both", hint: "Exact words and what you talked about" },
            { value: "words", label: "Words", hint: "Exactly these words, in this order" },
            { value: "meaning", label: "Meaning", hint: "What you talked about, in any words" },
          ]}
        />
        <Show when={only()}>
          <Chip tone="accent" testid="search-only" tooltip="Only this project's videos">
            <span>This project</span>
            <button type="button" class="chip-x" aria-label="Search the whole library" onClick={() => setOnly(undefined)}>✕</button>
          </Chip>
        </Show>
        <Show when={res()}>
          {(r) => (
            <span class="muted small libsearch-count" role="status">
              {r().hits.length} hit{r().hits.length === 1 ? "" : "s"}{r().topics?.length ? ` · ${r().topics!.length} topic${r().topics!.length === 1 ? "" : "s"}` : ""} · {r().index.projects} project{r().index.projects === 1 ? "" : "s"}
            </span>
          )}
        </Show>
      </header>
      <Show when={meaningNote()}>
        {(t) => (
          <p class="libsearch-note muted small" role="status">
            {t()}
            <Show when={res()?.meaning?.state === "no_model"}>
              {" "}
              <Button size="sm" variant="ghost" icon="upload" data-testid="meaning-model" onClick={() => void attempt(actions.modelsDownload).then((r) => r && say("Downloading the search model…"))}>
                Download
              </Button>
            </Show>
          </p>
        )}
      </Show>
      <Show when={topics().length}>
        <section aria-label="Topics" data-testid="library-topics">
          <h3 class="small muted">Topics</h3>
          <ul class="libsearch-topics">
            <For each={topics()}>
              {(t) => (
                <li data-testid="library-topic">
                  <Chip tone={t.state === "accepted" ? "accent" : "neutral"} tooltip={t.state === "accepted" ? "Topic" : "Proposed topic"}>topic</Chip>
                  <span class="topic-title">{t.title}</span>
                  <span class="muted small">{t.project_name}{t.media_name ? ` · ${t.media_name}` : ""} · {clock(t.start_s)}–{clock(t.end_s)}</span>
                  <Show when={t.long} fallback={<span class="muted small">in no long-form yet</span>}>
                    {(l) => <span class="small">in “{l().title}” at {clock(l().at_s)}</span>}
                  </Show>
                  <Show when={t.by === "meaning"}><Chip tone="neutral" tooltip="Matched the meaning of its title">meaning</Chip></Show>
                  <span class="spacer" />
                  <Button size="sm" icon="folder-open" data-testid="topic-open" tooltip={t.long ? "Open the long-form at this chapter" : "Open the stream edit at this topic"} onClick={() => void openTopic(t)}>
                    {t.long ? "Open in long-form" : "Open in stream edit"}
                  </Button>
                </li>
              )}
            </For>
          </ul>
        </section>
      </Show>
      <div class="libsearch-body" classList={{ playing: !!playing() }}>
        <ul class="libsearch-list" role="listbox" aria-label="Hits" aria-busy={busy()}>
          <For
            each={hits()}
            fallback={
              <li class="libsearch-empty">
                <Show
                  when={q().trim()}
                  fallback={<Empty icon="search" title="Search every stream you recorded" compact />}
                >
                  <Show when={!busy() && !topics().length}>
                    <Empty icon="search" title="Nothing said like that" compact />
                  </Show>
                </Show>
              </li>
            }
          >
            {(h, i) => (
              <li
                role="option"
                aria-selected={i() === at()}
                classList={{ hit: true, on: i() === at(), now: playing() === key(h) }}
                data-testid="library-hit"
                onPointerDown={() => setAt(i())}
              >
                <div class="hit-meta">
                  <span class="hit-project">{h.project_name}</span>
                  <span class="muted">{h.media_name}</span>
                  <span class="hit-time">{clock(h.start_s)}</span>
                  <Show when={h.topic}>{(t) => <Chip tone={t().state === "accepted" ? "accent" : "neutral"} tooltip="Topic">{t().title}</Chip>}</Show>
                  <Show when={h.by !== "words"}>
                    <Chip tone="neutral" tooltip={h.by === "both" ? "Matched the words and the meaning" : "Matched the meaning"}>{h.by === "both" ? "words + meaning" : "meaning"}</Chip>
                  </Show>
                </div>
                <p class="hit-text">
                  <span class="muted">{h.before ? `…${h.before} ` : ""}</span>
                  <mark>{h.text}</mark>
                  <span class="muted">{h.after ? ` ${h.after}…` : ""}</span>
                </p>
                <div class="hit-actions">
                  <IconButton icon="play" label="Play these words" keys="Enter" size="sm" data-testid="hit-play" onClick={() => void play(h)} />
                  <IconButton icon="folder-open" label="Open in its project" size="sm" data-testid="hit-open" onClick={() => void openIn(h)} />
                  <Button size="sm" icon="plus" data-testid="hit-moment" tooltip="Make a moment of these words (word-edged)" onClick={() => void makeMoment(h)}>Moment</Button>
                </div>
              </li>
            )}
          </For>
        </ul>
        <Show when={playing()}>
          <section class="libsearch-viewer" aria-label="Preview">
            <Viewer aspect={() => 16 / 9} measure={false} />
          </section>
        </Show>
      </div>
    </main>
  );
}
