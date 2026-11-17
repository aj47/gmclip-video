// The Polish tab (Ideas 2026-10-16, lane K2; decision 0025): five suggestions that move a
// short toward AJ's style — stressed words as keywords, a punch-in on the strongest of
// them, callouts where he names a number, a product or something on screen, the last
// frame looping back to the opening, and holds where the screen jumps mid-sentence. Each
// says what it measured; each is one click and one undo step; nothing applies itself
// (stressed words do when the caption style asks for it).
import { createResource, createSignal, For, Show } from "solid-js";
import { actions, polishActions, type HoldSuggestion, type PolishKind } from "../actions";
import { Banner, Button, Switch } from "../design/components";
import { attempt, cutView, say, sayUndo } from "../store";
import "./polish.css";

export function PolishPanel() {
  const key = () => {
    const v = cutView();
    return v ? { p: v.project, c: v.cut, rev: v.rev } : undefined;
  };
  const [data, { refetch }] = createResource(key, async (k) => {
    const r = (await attempt(() => polishActions.view(k.p, k.c))) ?? null;
    if (r && r.auto_applied > 0) sayUndo(`Starred ${r.auto_applied} stressed word${r.auto_applied === 1 ? "" : "s"} (your caption style asks for it)`, k.p);
    return r;
  });
  const [busy, setBusy] = createSignal<string | null>(null);
  const [holds, setHolds] = createSignal<{ holds: HoldSuggestion[]; samples: number } | null>(null);
  const [held, setHeld] = createSignal<Set<string>>(new Set());

  const apply = async (kind: PolishKind, k: string, done: string) => {
    const v = cutView();
    if (!v || busy()) return;
    setBusy(`${kind}:${k}`);
    const r = await attempt(() => polishActions.apply(v.project, v.cut, kind, k));
    setBusy(null);
    if (r) sayUndo(done, v.project);
    void refetch();
  };
  const seek = (frame: number) => void attempt(() => actions.viewerSeek(frame));
  const findHolds = async () => {
    const v = cutView();
    if (!v || busy()) return;
    setBusy("holds");
    const r = await attempt(() => polishActions.holdsFind(v.project, v.cut));
    setBusy(null);
    if (!r) return;
    setHeld(new Set<string>());
    setHolds({ holds: r.holds, samples: r.samples });
    if (!r.holds.length) say(r.samples ? `No screen jumps mid-sentence (read ${r.samples} frames)` : "No screen layer to read here");
  };
  const hold = async (h: HoldSuggestion) => {
    const v = cutView();
    if (!v || busy()) return;
    setBusy(`hold:${h.key}`);
    const r = await attempt(() => actions.holdToggle(v.project, v.cut, h.frame));
    setBusy(null);
    if (r?.held) {
      setHeld(new Set([...held(), h.key]));
      sayUndo(`Screen held on ${h.at} until "${h.until_word}" (camera and sound stay live)`, v.project);
    }
  };
  const em = () => data()?.emphasis;
  const unstarred = () => (em()?.proposals ?? []).filter((w) => !w.keyword).length;

  return (
    <section class="polish-panel" aria-label="Polish" aria-busy={busy() !== null || data.loading}>
      <p class="muted small polish-intro">Suggestions that move a short toward your style. Each says what it measured; each is one click and one ⌘Z.</p>
      <Show when={data.loading && !data()}>
        <p class="muted small" role="status" data-testid="polish-loading">Reading this cut…</p>
      </Show>
      <Show when={!data.loading && data() === null}>
        <Banner tone="warning" testid="polish-error" actions={<Button size="sm" variant="ghost" icon="restore" onClick={() => void refetch()}>Try again</Button>}>
          The suggestions could not be read for this cut.
        </Banner>
      </Show>
      <div class="polish-sec" data-testid="polish-emphasis">
        <h3>Stressed words</h3>
        <p class="muted small">{em()?.measured ?? "Words said louder or slower than the speaker's usual here"} — proposed as keywords (yellow).</p>
        <Show when={em() && em()!.state !== "ok"}>
          <p class="muted small" data-testid="polish-emphasis-state">
            {em()!.state === "no_transcript" ? "No transcript for this cut yet." : em()!.state === "few_words" ? "Too few words to tell." : "No level frames for this recording yet: judged on pace alone."}
          </p>
        </Show>
        <Switch
          label="Star stressed words on their own"
          testid="polish-auto"
          hint="Your caption style marks the stressed words as keywords by itself (a word you unstar stays unstarred)"
          on={em()?.auto ?? false}
          disabled={!data() || busy() !== null}
          onChange={(on) => void apply("emphasis", on ? "auto_on" : "auto_off", on ? "Stressed words are starred by your caption style" : "Stressed words are proposals only")}
        />
        <ul class="polish-list" aria-label="Stressed words">
          <For each={em()?.proposals ?? []}>
            {(w) => (
              <li class="polish-row" data-testid="polish-stress">
                <button type="button" class="ghost tc" onClick={() => seek(w.frame)} aria-label={`Go to ${w.at}`}>{w.at}</button>
                <span class="polish-main">
                  <b classList={{ kw: w.keyword }}>{w.text}</b>
                  <span class="muted small polish-why">{w.label}</span>
                </span>
                <Button size="sm" variant={w.keyword ? "ghost" : "secondary"} icon="star" data-testid="polish-star" disabled={w.keyword || busy() !== null}
                  tooltip="Make it a keyword (yellow in the captions)" onClick={() => void apply("emphasis", w.key, `"${w.text}" is a keyword`)}>{w.keyword ? "Starred" : "Star"}</Button>
              </li>
            )}
          </For>
        </ul>
        <Show when={unstarred() > 1}>
          <Button size="sm" icon="star" class="polish-all" data-testid="polish-star-all" disabled={busy() !== null}
            onClick={() => void apply("emphasis", "all", `${unstarred()} stressed words are keywords`)}>Star all {unstarred()}</Button>
        </Show>
        <Show when={em()?.state === "ok" && !(em()?.proposals.length)}>
          <p class="muted small">Nothing stands out from the speaker's usual level and pace.</p>
        </Show>
      </div>

      <div class="polish-sec" data-testid="polish-punch">
        <h3>Punch-in</h3>
        <p class="muted small">{data()?.punch.rule ?? "A small zoom on the screen at the strongest stressed words, held for the phrase"}</p>
        <Switch
          label="Punch in on emphasis"
          testid="polish-punch-on"
          hint="Every suggested punch-in at once (on) or none (off) — one undo step"
          on={data()?.punch.on ?? false}
          disabled={!data() || busy() !== null || (!data()!.punch.on && !data()!.punch.suggestions.some((s) => !s.why_not))}
          onChange={(on) => void apply("punch", on ? "all" : "off", on ? "Punch-ins added on the stressed words" : "Punch-ins removed")}
        />
        <ul class="polish-list" aria-label="Punch-in suggestions">
          <For each={data()?.punch.suggestions ?? []}>
            {(s) => (
              <li class="polish-row" data-testid="polish-punch-row">
                <button type="button" class="ghost tc" onClick={() => seek(s.frame)} aria-label={`Go to ${s.at}`}>{s.at}</button>
                <span class="polish-main">
                  <b>{s.text}</b>
                  <span class="muted small polish-why">{s.why_not ?? s.label}</span>
                </span>
                <Show
                  when={s.added}
                  fallback={<Button size="sm" icon="plus" data-testid="polish-punch-add" disabled={!!s.why_not || busy() !== null}
                    onClick={() => void apply("punch", s.key, `Punch-in at "${s.text}"`)}>Add</Button>}
                >
                  {(id) => <Button size="sm" variant="ghost" icon="x" data-testid="polish-punch-remove" disabled={busy() !== null}
                    onClick={() => void apply("punch", `rm:${id()}`, "Punch-in removed")}>Remove</Button>}
                </Show>
              </li>
            )}
          </For>
        </ul>
      </div>

      <div class="polish-sec" data-testid="polish-callouts">
        <h3>Callouts</h3>
        <p class="muted small">Where a number, a product or something on screen is named: a label or an arrow, inside the safe zones, clear of the captions and the face.</p>
        <ul class="polish-list" aria-label="Callout suggestions">
          <For each={data()?.callouts ?? []}>
            {(c) => (
              <li class="polish-row" data-testid="polish-callout">
                <button type="button" class="ghost tc" onClick={() => seek(c.frame)} aria-label={`Go to ${c.at}`}>{c.at}</button>
                <span class="polish-main">
                  <b>{c.kind === "arrow" ? "→ " : ""}{c.text}</b>
                  <span class="muted small polish-why">{c.label} · {c.kind === "arrow" ? "arrow into the screen" : "label"} for {c.seconds.toFixed(1)} s</span>
                </span>
                <Button size="sm" variant={c.added ? "ghost" : "secondary"} icon={c.added ? "check" : "plus"} data-testid="polish-callout-add" disabled={c.added || busy() !== null}
                  onClick={() => void apply("callout", c.key, `${c.kind === "arrow" ? "Arrow" : "Label"} "${c.text}" added — move it on the canvas`)}>{c.added ? "Added" : "Add"}</Button>
              </li>
            )}
          </For>
        </ul>
        <Show when={data() && !data()!.callouts.length}>
          <p class="muted small">No numbers, products or on-screen things named in this cut.</p>
        </Show>
      </div>

      <div class="polish-sec" data-testid="polish-loop">
        <h3>Loop</h3>
        <Switch
          label="Loop: the last frame matches the opening"
          testid="polish-loop-on"
          hint="Checks that the short ends on part one's layout and crop, so it loops cleanly (a check in the export checks)"
          on={data()?.loop.on === true}
          disabled={!data() || busy() !== null}
          onChange={(on) => void apply("loop", on ? "on" : "off", on ? "This cut loops" : "This cut does not loop")}
        />
        <Show when={data()?.loop.state === "ok"}>
          <p class="muted small" data-testid="polish-loop-state">
            {data()!.loop.matches ? "The last frame shows the opening's layout and crop." : `The last frame differs from the opening: ${(data()!.loop.differences ?? []).join(", ")}.`}
          </p>
          <div class="row">
            <For each={data()!.loop.fixes ?? []}>
              {(f) => <Button size="sm" data-testid={`polish-loop-${f.key}`} disabled={busy() !== null}
                onClick={() => void apply("loop", f.key, f.key === "tail" ? "A 0.6 s tail ends on the opening's layout" : "The last part takes the opening's layout")}>{f.label}</Button>}
            </For>
          </div>
        </Show>
      </div>

      <div class="polish-sec" data-testid="polish-holds">
        <h3>Hold screen</h3>
        <p class="muted small">Where the screen jumps to something else while a sentence is still being said: hold it on the last frame before the jump (camera and sound stay live).</p>
        <Button size="sm" icon={busy() === "holds" ? "loader" : "search"} class="polish-all" data-testid="polish-holds-find" disabled={busy() !== null} onClick={() => void findHolds()}>
          {busy() === "holds" ? "Reading frames…" : "Find screen jumps"}
        </Button>
        <Show when={holds()}>
          {(h) => (
            <>
              <p class="muted small" data-testid="polish-holds-summary">{h().holds.length} jump{h().holds.length === 1 ? "" : "s"} mid-sentence · {h().samples} frames read</p>
              <ul class="polish-list" aria-label="Hold suggestions">
                <For each={h().holds}>
                  {(x) => (
                    <li class="polish-row" data-testid="polish-hold">
                      <button type="button" class="ghost tc" onClick={() => seek(x.change_frame)} aria-label={`Go to ${x.at}`}>{x.at}</button>
                      <span class="polish-main"><span class="muted small polish-why">{x.label}</span></span>
                      <Button size="sm" variant={held().has(x.key) ? "ghost" : "secondary"} icon={held().has(x.key) ? "check" : "pointer"} data-testid="polish-hold-apply" disabled={held().has(x.key) || busy() !== null} onClick={() => void hold(x)}>
                        {held().has(x.key) ? "Held" : "Hold"}
                      </Button>
                    </li>
                  )}
                </For>
              </ul>
            </>
          )}
        </Show>
      </div>
    </section>
  );
}
