// B-roll (K.05): moments from your other recordings that talk about the selected words,
// one click to add one (its frame over the words, as a part, or as a silent clip). A
// popover over the window, opened from the words strip's selection, the transcript's
// selection or ⌘K, so it works in every layout (the strip is hidden in small windows).
import { createResource, createSignal, For, Show } from "solid-js";
import { ideaActions, libraryActions, type BrollSuggestion } from "../actions";
import { Button, IconButton } from "../design/components";
import { attempt, refreshCut, say } from "../store";
import "./ideas.css";

interface Ask {
  project: string;
  cut: string;
  /** The selected words (transcript ordinals). */
  lo: number;
  hi: number;
}

const [ask, setAsk] = createSignal<Ask | null>(null);
/** Opens (or with null closes) the B-roll popover for these words. */
export const askBroll = (a: Ask | null) => setAsk(a);

export function BrollPop() {
  return <Show when={ask()}>{(a) => <Pop ask={a()} />}</Show>;
}

function Pop(props: { ask: Ask }) {
  const [res] = createResource(
    () => props.ask,
    async (a) => {
      const r = await attempt(() => ideaActions.brollSuggest(a.project, a.cut, a.lo, a.hi));
      if (!r) return { rows: [] as BrollSuggestion[], note: "B-roll could not be looked up just now" };
      const m = r.meaning?.state;
      const note =
        m === "no_model" ? "Suggestions need the search model (Search › Download)" :
        m === "building" ? `The meaning index is still building (${r.meaning?.pending ?? 0} to go)` :
        !r.suggestions.length ? "Nothing in your other recordings talks about this" : null;
      return { rows: r.suggestions, note };
    },
  );
  const close = () => setAsk(null);
  const add = async (h: BrollSuggestion, asClip = false) => {
    const a = props.ask;
    const r = await attempt(() => ideaActions.brollAdd(a.project, a.cut, a.lo, a.hi, h, asClip));
    if (!r) return;
    close();
    say(r.added === "part" ? `B-roll added as a part (${r.seconds.toFixed(1)} s) after the selection — ⌘Z undoes it`
      : r.added === "clip" ? `B-roll clip added over the selection (${r.seconds.toFixed(1)} s, silent) — ⌘Z undoes it`
      : `B-roll frame added over the selection (${r.seconds.toFixed(1)} s) — ⌘Z undoes it`);
    void refreshCut();
  };
  return (
    <section
      class="broll-pop"
      aria-label="B-roll from your library"
      data-testid="broll-pop"
      tabindex="-1"
      ref={(el) => queueMicrotask(() => el.focus({ preventScroll: true }))}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          close();
        }
      }}
    >
      <div class="row">
        <b class="small">B-roll from your other recordings</b>
        <span class="spacer" />
        <IconButton icon="x" size="sm" label="Close" keys="Esc" onClick={close} />
      </div>
      <Show when={!res.loading} fallback={<p class="muted small" role="status" data-testid="broll-busy">Looking through your library…</p>}>
        <Show when={res()?.note}>{(n) => <p class="muted small" role="status" data-testid="broll-note">{n()}</p>}</Show>
        <ul class="broll-list">
          <For each={res()?.rows ?? []}>
            {(h) => (
              <li data-testid="broll-row" data-add={h.add_as}>
                <span class="broll-meta">
                  <b>{h.project_name}</b>
                  <span class="muted">{h.media_name}</span>
                  <span class="muted">{Math.floor(h.start_s / 60)}:{String(Math.floor(h.start_s % 60)).padStart(2, "0")}</span>
                </span>
                <span class="broll-actions">
                  <IconButton icon="play" size="sm" label="Play it" onClick={() => void attempt(() => libraryActions.play(h))} />
                  <Button size="sm" icon="plus" data-testid="broll-add" tooltip={h.note} onClick={() => void add(h)}>
                    {h.add_as === "part" ? "Add as part" : "Add its frame"}
                  </Button>
                  <Show when={h.also === "clip"}>
                    <Button size="sm" variant="ghost" icon="play" data-testid="broll-add-clip" tooltip="Its own video, silent, picture-in-picture over the words" onClick={() => void add(h, true)}>
                      As a clip
                    </Button>
                  </Show>
                </span>
                <p class="broll-text">{h.text}</p>
                <span class="muted small broll-note">{h.note}</span>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </section>
  );
}
