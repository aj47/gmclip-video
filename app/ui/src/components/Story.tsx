// The story panel (SPEC §5.2.1, backlog U.07): brief (autosaves), ✨ gather (the
// grow-story pass, agent-executed: arrives with A.02/E.02), related spans, ⏱ chronological,
// part rows with typed in/out (12:34, 1:02:03.5), ‹ › ±0.5 s (⇧ ±0.1 s), ▶ ↑ ↓ ✕, and the
// duration chip coloured by the 9:16 band (green 20–100 s, red below, gold above).
import { createEffect, createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { actions } from "../actions";
import { attempt, cutView, say, setSelectedPart } from "../store";

export function Story() {
  const key = () => cutView() && { p: cutView()!.project, c: cutView()!.cut, rev: cutView()!.rev };
  const [story] = createResource(key, async (k) => attempt(() => actions.story(k.p, k.c)));
  const [brief, setBrief] = createSignal("");
  let typed = false;
  createEffect(() => {
    const s = story();
    if (s && !typed) setBrief(s.brief);
  });
  let timer: ReturnType<typeof setTimeout> | undefined;
  const onBrief = (v: string) => {
    typed = true;
    setBrief(v);
    clearTimeout(timer);
    timer = setTimeout(() => {
      const c = cutView();
      if (c) void attempt(() => actions.briefSet(c.project, c.cut, v)).then(() => (typed = false));
    }, 800);
  };
  const act = (f: (p: string, c: string) => Promise<unknown>) => {
    const c = cutView();
    if (c) void attempt(() => f(c.project, c.cut));
  };
  const [sort, setSort] = createSignal<"arc" | "time" | "relevance">("arc");
  const [hideIn, setHideIn] = createSignal(false);
  const [watching, setWatching] = createSignal<string | null>(null);
  type Sug = NonNullable<ReturnType<typeof story>>["suggestions"][number];
  const spans = (all: Sug[]) => {
    const v = all.filter((g) => !(hideIn() && g.in_cut));
    const k = sort();
    return [...v].sort((a, b) =>
      k === "time" ? (a.in_s ?? 0) - (b.in_s ?? 0) : k === "relevance" ? b.relevance - a.relevance : (a.arc ?? 1e9) - (b.arc ?? 1e9),
    );
  };
  // ▶ out of the cut: the stream around the span plays in the viewer
  const watch = async (at: number, label: string) => {
    const c = cutView();
    if (!c) return;
    const r = await attempt(() => actions.streamView(c.project, c.cut, at));
    if (r) {
      setWatching(label);
      void attempt(actions.viewerPlay);
    }
  };
  const backToCut = async () => {
    const c = cutView();
    setWatching(null);
    if (c) await attempt(() => actions.viewerLoad(c.project, c.cut, { keep: true }));
  };
  onCleanup(() => { if (watching()) void backToCut(); });
  const play = (n: number) => {
    const part = cutView()?.parts[n - 1];
    if (!part) return;
    setSelectedPart(part.id);
    void attempt(() => actions.viewerSeek(part.out[0])).then(() => attempt(actions.viewerPlay));
  };

  return (
    <section class="story" aria-label="Story">
      <Show when={story()}>
        {(s) => (
          <>
            <header class="story-head">
              <span class={`dur-chip ${s().band}`} title={`9:16 band ${s().band_range[0]}–${s().band_range[1]} s`} data-testid="duration-chip">
                {s().duration_s.toFixed(1)} s
              </span>
              <button type="button" class="ghost" onClick={() => act(actions.chronological)} title="Order the parts by source time">⏱ Chronological</button>
            </header>
            <label class="brief">
              <span class="muted">Brief</span>
              <textarea rows="3" aria-label="Brief" value={brief()} onInput={(e) => onBrief(e.currentTarget.value)} placeholder="What this cut is about, what to keep, what to cut." />
            </label>
            <div class="row">
              <button
                type="button"
                data-testid="grow-story"
                title="Your agent (Claude Code) grows this cut into a story, live on the cut; ⌘Z undoes its run"
                onClick={() => act((p, c) => actions.agentRun(p, c, "grow_story", brief()).then(() => say("Your agent is growing the story (Agent tab shows it)")))}
              >
                ✨ Grow story with my agent
              </button>
            </div>
            <div class="row spans-head">
              <h3>Related spans</h3>
              <select aria-label="Sort spans" value={sort()} onChange={(e) => setSort(e.currentTarget.value as "arc" | "time" | "relevance")}>
                <option value="arc">Proposed order</option>
                <option value="time">Time</option>
                <option value="relevance">Relevance</option>
              </select>
              <label class="check small"><input type="checkbox" checked={hideIn()} onChange={(e) => setHideIn(e.currentTarget.checked)} /> Hide spans in the cut</label>
              <button type="button" class="primary" data-testid="use-proposed" disabled={!s().arc_len} title="The proposed order becomes the cut's parts after the hook, in one undo step; nothing is removed, rejected spans are never added" onClick={() => act((p, c) => actions.storyApply(p, c).then((r) => say(`Used the proposed cut: ${r.added.length} added${r.skipped_rejected ? `, ${r.skipped_rejected} rejected skipped` : ""}`)))}>
                ✨ Use the proposed cut
              </button>
            </div>
            <Show when={watching()}>
              <p class="row small">Watching the stream at {watching()} <button type="button" data-testid="back-to-cut" onClick={() => void backToCut()}>Back to the cut</button></p>
            </Show>
            <ul class="spans" aria-label="Related spans">
              <For each={spans(s().suggestions)} fallback={<li class="empty">None yet: “Grow story with my agent” gathers the spans that tell this story.</li>}>
                {(g) => (
                  <li class="item" data-testid="span-row" classList={{ rejected: g.state === "rejected", incut: g.in_cut }}>
                    <span class="muted">{g.arc !== null ? `${g.arc + 1}.` : "·"}</span>
                    <span class="muted">{g.in ?? "?"}</span>
                    <span class="muted">{g.role}</span>
                    <span class="quote">{g.quote}</span>
                    <span class="tools">
                      <button type="button" aria-label={`Watch ${g.in ?? "span"}`} disabled={g.in_s === null} onClick={() => void watch(g.in_s!, g.in ?? "")}>▶</button>
                      <Show
                        when={!g.in_cut}
                        fallback={<span class="ok" title="In the cut">✓</span>}
                      >
                        <button type="button" aria-label={`Add ${g.in ?? "span"}`} disabled={g.state === "rejected"} onClick={() => act((p, c) => actions.storyApply(p, c, [g.id]))}>＋</button>
                      </Show>
                      <Show
                        when={g.state === "rejected"}
                        fallback={<button type="button" class="ghost" aria-label={`Reject ${g.in ?? "span"}`} onClick={() => act((p) => actions.suggestionState(p, g.id, "rejected"))}>✕</button>}
                      >
                        <button type="button" class="ghost" aria-label={`Restore ${g.in ?? "span"}`} onClick={() => act((p) => actions.suggestionState(p, g.id, "suggested"))}>↺</button>
                      </Show>
                    </span>
                  </li>
                )}
              </For>
            </ul>
            <h3>Parts</h3>
            <ol class="part-rows" aria-label="Story parts">
              <For each={s().parts}>
                {(pt) => (
                  <li class="part-row">
                    <span class="n">{pt.n}</span>
                    <span class="muted role">{pt.role}</span>
                    <span class="edge">
                      <button type="button" aria-label={`Part ${pt.n} in earlier`} onClick={(e) => act((p, c) => actions.partNudge(p, c, pt.id, "in", e.shiftKey ? -100 : -500))}>‹</button>
                      <input
                        type="text"
                        aria-label={`Part ${pt.n} in`}
                        value={pt.in}
                        onChange={(e) => { const v = e.currentTarget.value; act((p, c) => actions.partTime(p, c, pt.id, "in", v)); }}
                      />
                      <button type="button" aria-label={`Part ${pt.n} in later`} onClick={(e) => act((p, c) => actions.partNudge(p, c, pt.id, "in", e.shiftKey ? 100 : 500))}>›</button>
                    </span>
                    <span class="edge">
                      <button type="button" aria-label={`Part ${pt.n} out earlier`} onClick={(e) => act((p, c) => actions.partNudge(p, c, pt.id, "out", e.shiftKey ? -100 : -500))}>‹</button>
                      <input
                        type="text"
                        aria-label={`Part ${pt.n} out`}
                        value={pt.out}
                        onChange={(e) => { const v = e.currentTarget.value; act((p, c) => actions.partTime(p, c, pt.id, "out", v)); }}
                      />
                      <button type="button" aria-label={`Part ${pt.n} out later`} onClick={(e) => act((p, c) => actions.partNudge(p, c, pt.id, "out", e.shiftKey ? 100 : 500))}>›</button>
                    </span>
                    <span class="muted dur">{pt.dur_s.toFixed(1)} s</span>
                    <span class="tools">
                      <button type="button" aria-label={`Play part ${pt.n}`} onClick={() => play(pt.n)}>▶</button>
                      <button type="button" aria-label={`Move part ${pt.n} up`} disabled={pt.n === 1} onClick={() => act((p, c) => actions.partMove(p, c, pt.id, "up"))}>↑</button>
                      <button type="button" aria-label={`Move part ${pt.n} down`} disabled={pt.n === s().parts.length} onClick={() => act((p, c) => actions.partMove(p, c, pt.id, "down"))}>↓</button>
                      <button type="button" class="danger" aria-label={`Delete part ${pt.n}`} onClick={() => act((p) => actions.deletePart(p, pt.id))}>✕</button>
                    </span>
                  </li>
                )}
              </For>
            </ol>
          </>
        )}
      </Show>
    </section>
  );
}
