// The Captions tab, the Short tab before decision 0054 (backlog U.09; the "Reel / Short style"): captions (story: 1–3
// words from the cut's own words, keywords yellow with * in the transcript, corrections),
// the text hook (frame 0 until a word), section pills and a measured run clock. Every
// change is one command and shows in the preview and the export. Each section shows its
// main action; the tuning and the entry forms wait behind Advanced.
import { createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { actions, rate } from "../actions";
import { attempt, cutView, say, streamViewer, viewer } from "../store";
import { trData, trSel } from "../transcriptState";
import { HookLines, Openings } from "./Packaging";
import { EndCardLongform } from "./EndCardLongform";

function tc(frame: number | null, fps: number): string {
  if (frame === null) return "—";
  const s = frame / fps;
  return `${Math.floor(s / 60)}:${(s % 60).toFixed(1).padStart(4, "0")}`;
}

export function ShortPanel() {
  const key = () => {
    const v = cutView();
    return v ? { p: v.project, c: v.cut, rev: v.rev } : undefined;
  };
  const [data, { refetch }] = createResource(key, async (k) => (await attempt(() => actions.short(k.p, k.c))) ?? null);
  const fps = () => rate(cutView()?.fps);
  /** An output frame as m:ss.s. */
  const at = (f: number) => {
    const sec = f / fps();
    return `${Math.floor(sec / 60)}:${(sec % 60).toFixed(1).padStart(4, "0")}`;
  };
  const run = async <T,>(f: (p: string, c: string) => Promise<T>, done?: string) => {
    const v = cutView();
    if (!v) return undefined;
    const r = await attempt(() => f(v.project, v.cut));
    if (r !== undefined && done) say(done);
    void refetch();
    return r;
  };

  const [terms, setTerms] = createSignal<string | null>(null); // null: the stored list
  const shownTerms = () => terms() ?? (data()?.keyword_terms ?? []).join(", ");
  const [keepText, setKeepText] = createSignal("");
  const [termW, setTermW] = createSignal("");
  const [termH, setTermH] = createSignal("");
  const [gate, { refetch: regate }] = createResource(key, async (k) => (await attempt(() => actions.termGate(k.p, k.c))) ?? null);
  const [capY, setCapY] = createSignal("");
  const [fixFrom, setFixFrom] = createSignal("");
  const [fixTo, setFixTo] = createSignal("");
  const [hook, setHook] = createSignal("");
  const [hookSecs, setHookSecs] = createSignal("5");
  const [pill, setPill] = createSignal("");
  const [runStart, setRunStart] = createSignal<number | null>(null);
  // optional: the take the clock starts on and the take its DONE stays until
  const [clockFrom, setClockFrom] = createSignal("");
  const [clockUntil, setClockUntil] = createSignal("");
  const [running, setRunning] = createSignal("GENERATING");
  const [doneLabel, setDoneLabel] = createSignal("DONE IN");
  const [measure, setMeasure] = createSignal<string | null>(null);
  // The stream viewer: the stream around a time, to mark a run the cut does not show.
  const [stream, setStream] = createSignal<{ t0_s: number; fps: number } | null>(null);
  const [goTo, setGoTo] = createSignal("");
  const [srcFrom, setSrcFrom] = createSignal("");
  const [srcTo, setSrcTo] = createSignal("");
  const streamNow = () => {
    const st = stream();
    const sv = streamViewer();
    return st && sv ? st.t0_s + sv.frame / st.fps : null;
  };
  const selWordS = () => {
    const sw = trSel();
    const t = trData();
    return sw && t ? (t.start_ms[sw.lo] ?? 0) / 1000 : null;
  };
  const openStream = async () => {
    const v = cutView();
    const at = goTo().trim() !== "" ? Number(goTo()) : selWordS();
    if (!v || at === null || !Number.isFinite(at)) return;
    const r = await attempt(() => actions.streamView(v.project, v.cut, at));
    if (r) {
      setStream({ t0_s: r.t0_s, fps: r.fps });
      say("The stream is in the viewer: step to the frame, then mark");
    }
  };
  const backToCut = async () => {
    const v = cutView();
    setStream(null);
    if (v) await attempt(() => actions.viewerLoad(v.project, v.cut, { keep: true }));
  };
  // leaving the tab puts the cut back in the viewer
  onCleanup(() => { if (stream()) void backToCut(); });
  const measureSrc = () => {
    const a = Number(srcFrom()), b = Number(srcTo());
    void run((p, c) => actions.runMeasureSrc(p, c, a, b)).then((r) => {
      if (r) {
        setMeasure(r.measurement);
        say(`Measured ${r.seconds.toFixed(1)} s`);
      }
    });
  };

  const selWord = () => {
    const s = trSel();
    const t = trData();
    return s && t ? { i: s.hi, text: t.text[s.hi] ?? "" } : null;
  };
  const roleName = (r: string | null) =>
    r === "text_hook" ? "Hook" : r === "section_pill" ? "Pill" : r === "run_clock" ? "Run clock" : r ?? "Text";

  return (
    <section class="short-panel" aria-label="Captions">
      <div>
        <h3>Captions</h3>
        <label class="check">
          <input
            type="checkbox"
            data-testid="captions-on"
            checked={data()?.captions ?? false}
            disabled={!data()}
            onChange={(e) => void run((p, c) => actions.captionsSet(p, c, e.currentTarget.checked), e.currentTarget.checked ? "Captions on (story)" : "Captions off")}
          />
          Story captions — 1–3 words from the cut's own words
        </label>
        <Show when={data()?.imported_captions}>
          <p class="muted small">This cut shows imported captions; turning story captions on builds them from the transcript instead.</p>
        </Show>
        <p class="muted small" data-testid="caption-count">{(cutView()?.captions ?? []).filter((c) => c.role === "caption").length} captions on the cut. List keywords below to star every match, or select words in the transcript and press <kbd>*</kbd> to star just those.</p>
        <form class="row" onSubmit={(e) => { e.preventDefault(); void run((p, c) => actions.keywordsSet(p, c, shownTerms()), "Keywords starred across the cut").then(() => setTerms(null)); }}>
          <input type="text" aria-label="Keywords" data-testid="keywords" placeholder="keywords: two minutes, NVIDIA, 5080, GPU" value={shownTerms()} onInput={(e) => setTerms(e.currentTarget.value)} />
          <button type="submit" data-testid="keywords-star" disabled={!data() || terms() === null}>Star</button>
        </form>
        <Show when={data()?.keep.length}>
          <p class="muted small row" data-testid="keep-list">
            <For each={data()?.keep ?? []}>
              {(k) => (
                <span class="tag">“{k}” <button type="button" class="ghost" aria-label={`Stop keeping ${k} together`} onClick={() => void run((p) => actions.keepSet(p, k, false))}>✕</button></span>
              )}
            </For>
          </p>
        </Show>
        <Show when={(data()?.cues.length ?? 0) > 0}>
          <details class="cues" data-testid="cue-list">
            <summary>Cues ({data()?.cues.length}){(data()?.hand_breaks ?? 0) > 0 ? ` · ${data()?.hand_breaks} set by hand` : ""}</summary>
            <p class="muted small">Grouped the story way. Click a word to start a cue there; ⤵ joins a cue with the next.</p>
            <ol class="cue-rows">
              <For each={data()?.cues ?? []}>
                {(cue, i) => {
                  const next = () => data()?.cues[i() + 1];
                  return (
                    <li class="row cue" data-testid="cue-row">
                      <span class="muted small tc">{tc(cue.start, fps())}</span>
                      <span class="cue-words">
                        <For each={cue.words}>
                          {(w, j) => (
                            <button
                              type="button"
                              class={`ghost cue-word${w.break ? ` hand-${w.break}` : ""}${w.star ? " star" : ""}`}
                              title={w.break ? `Set by hand (${w.break}); click to clear` : j() === 0 ? "Starts this cue" : "Start a cue here"}
                              disabled={j() === 0 && !w.break}
                              onClick={() => void run((p, c) => actions.cueBreak(p, c, w, w.break ? null : "split"), w.break ? "Back to the automatic grouping" : "Split here")}
                            >
                              {w.text}
                            </button>
                          )}
                        </For>
                      </span>
                      <Show when={next()?.words[0]}>
                        {(w0) => (
                          <button
                            type="button"
                            class="ghost"
                            aria-label={`Join "${cue.text}" with the next cue`}
                            data-testid="cue-join"
                            onClick={() => void run((p, c) => actions.cueBreak(p, c, w0(), "join"), "Joined with the next cue")}
                          >
                            ⤵
                          </button>
                        )}
                      </Show>
                    </li>
                  );
                }}
              </For>
            </ol>
          </details>
        </Show>
        <Show when={gate()?.findings.length}>
          <h4>Names to fix</h4>
          <div data-testid="names">
            <For each={gate()?.findings ?? []}>
              {(f) => (
                <div class="row name-fix" data-testid="name-row">
                  <span>{f.shown} → <b>{f.written}</b> <span class="muted">{f.kind === "near" ? "(close)" : ""}</span></span>
                  <button type="button" class="ghost" aria-label={`Spell it ${f.written}`} onClick={() => void run((p, c) => actions.captionReplace(p, c, gate()!.rev, [f.ordinals], f.written), `Captions show ${f.written}`).then(() => void regate())}>Fix</button>
                </div>
              )}
            </For>
          </div>
        </Show>
        <Show when={data()?.fixes.length}>
          <h4>Corrections</h4>
          <For each={data()?.fixes ?? []}>
            {(f) => (
              <div class="row fix">
                <span>{f.from} → <b>{f.to}</b> <span class="muted">×{f.n}</span></span>
                <button type="button" class="ghost" aria-label={`Remove the correction of ${f.from}`} onClick={() => void run((p, c) => actions.captionFix(p, c, f.from, ""))}>✕</button>
              </div>
            )}
          </For>
        </Show>
        {/* the tuning and the entry forms wait behind Advanced; what they made shows above */}
        <details class="more" data-testid="captions-more">
          <summary>Advanced</summary>
          <form class="row" onSubmit={(e) => { e.preventDefault(); const y = Number(capY()); if (Number.isFinite(y)) void run((p, c) => actions.captionY(p, c, y), "Captions moved").then(() => setCapY("")); }}>
            <label class="muted small">Caption height (px from the top)
              <input type="number" class="num" aria-label="Caption height (px)" data-testid="caption-y" step="5"
                placeholder={data()?.caption_y != null ? String(Math.round(data()!.caption_y!)) : "1065"} value={capY()} onInput={(e) => setCapY(e.currentTarget.value)} />
            </label>
            <button type="submit" disabled={!capY()}>Move</button>
          </form>
          <form class="row" onSubmit={(e) => { e.preventDefault(); void run((p) => actions.keepSet(p, keepText(), true), "Kept together in captions").then(() => setKeepText("")); }}>
            <input type="text" aria-label="Keep together" data-testid="keep" placeholder="keep together: web app, two bots" value={keepText()} onInput={(e) => setKeepText(e.currentTarget.value)} />
            <button type="submit" data-testid="keep-add" disabled={!keepText().includes(" ")}>Keep</button>
          </form>
          <h4>Names</h4>
          <Show when={gate()} fallback={<p class="muted small">Checking the names in the captions…</p>}>
            {(g) => (
              <Show when={g().findings.length === 0}>
                <p class="muted small">{g().terms ? `No misspelt names (${g().terms} terms checked).` : "No terms yet: add the names the speech model gets wrong."}</p>
              </Show>
            )}
          </Show>
          <form class="row" onSubmit={(e) => { e.preventDefault(); void run((p) => actions.termAdd(p, termW(), termH()), "Term added").then(() => { setTermW(""); setTermH(""); void regate(); }); }}>
            <input type="text" class="short" aria-label="Term" data-testid="term-written" placeholder="exe.dev" value={termW()} onInput={(e) => setTermW(e.currentTarget.value)} />
            <input type="text" aria-label="Term heard as" data-testid="term-heard" placeholder="heard as: XD.dev, x e dev" value={termH()} onInput={(e) => setTermH(e.currentTarget.value)} />
            <button type="submit" data-testid="term-add" disabled={!termW().trim()}>Add term</button>
          </form>
          <h4>New correction</h4>
          <p class="muted small">Fix a word the speech model misheard (audio is untouched).</p>
          <form class="row" onSubmit={(e) => { e.preventDefault(); void run((p, c) => actions.captionFix(p, c, fixFrom(), fixTo()), "Corrected everywhere in this cut").then(() => { setFixFrom(""); setFixTo(""); }); }}>
            <input type="text" aria-label="Heard as" placeholder="heard as (XD.dev)" value={fixFrom()} onInput={(e) => setFixFrom(e.currentTarget.value)} />
            <input type="text" aria-label="Show as" placeholder="show as (exe.dev)" value={fixTo()} onInput={(e) => setFixTo(e.currentTarget.value)} />
            <button type="submit" disabled={!fixFrom().trim() || !fixTo().trim()}>Fix</button>
          </form>
        </details>
      </div>

      <div>
        <h3>Text hook</h3>
        <form class="col" onSubmit={(e) => {
          e.preventDefault();
          const w = selWord();
          void run((p, c) => actions.hookSet(p, c, hook(), w ? w.i : null, w ? null : Math.round(Number(hookSecs()) * 1000)), "Hook set");
        }}>
          <input type="text" aria-label="Hook text" placeholder="My PC made this in 67 seconds." value={hook()} onInput={(e) => setHook(e.currentTarget.value)} />
          <div class="row">
            <Show
              when={selWord()}
              fallback={
                <label>Shows for <input type="number" class="num" aria-label="Hook seconds" min="1" max="60" step="0.5" value={hookSecs()} onInput={(e) => setHookSecs(e.currentTarget.value)} /> s (or select the word it lasts until)</label>
              }
            >
              {(w) => <span class="muted">until “{w().text}” (the selected word)</span>}
            </Show>
            <button type="submit" disabled={!hook().trim()}>Set hook</button>
          </div>
          <Show when={data()?.hook_layout}>
            {(h) => (
              <p class="hook-warn" role="status" data-testid="hook-layout-warn">
                The layout changes at {at(h().changes_at)} (take {h().take}) while the hook is still on screen (until {at(h().hook_end)}).
              </p>
            )}
          </Show>
          <Show when={data()?.overlays.some((o) => o.role === "text_hook")}>
            <button
              type="button"
              class={data()?.hook_layout ? "small" : "ghost small"}
              data-testid="hook-layout"
              title="AGENTS.md: when the hook outlasts the opening take, the next take keeps the opening's layout until the hook ends (split at the nearest word)"
              onClick={() => void run((p, c) => actions.hookLayoutKeep(p, c)).then((r) => {
                if (r) say(r.parts ? `The opening layout stays until the hook clears (${r.parts} take${r.parts === 1 ? "" : "s"})` : (r.note ?? "Nothing to change"));
              })}
            >
              Keep the opening layout until the hook clears
            </button>
          </Show>
        </form>
        <HookLines
          until={() => selWord()?.i ?? null}
          durMs={() => Math.round(Number(hookSecs()) * 1000)}
          onUsed={(t) => {
            setHook(t);
            void refetch();
          }}
        />
      </div>

      <Show when={(cutView()?.kind ?? "short") === "short"}>
        <div>
          <h3>Openings</h3>
          <p class="muted small">Up to three other openings for this short: each is a version that shares everything after the opening. Compare them side by side and make one the main version.</p>
          <Openings />
        </div>
      </Show>

      <div>
        <h3>Section pill</h3>
        <form class="row" onSubmit={(e) => { e.preventDefault(); void run((p, c) => actions.pillAdd(p, c, viewer()?.frame ?? 0, pill()), "Pill added to this part").then(() => setPill("")); }}>
          <input type="text" aria-label="Pill text" placeholder="MiniMax H3 running on my PC" value={pill()} onInput={(e) => setPill(e.currentTarget.value)} />
          <button type="submit" disabled={!pill().trim()}>Add to this part</button>
        </form>
      </div>

      <div>
        <h3>Run clock</h3>
        <p class="muted small">Mark the frame the run starts (the app's state change) and the first frame the result shows; the clock counts real time through your cuts.</p>
        <div class="row">
          <button type="button" data-testid="run-start" onClick={() => setRunStart(viewer()?.frame ?? 0)}>Mark run start{runStart() !== null ? ` (${tc(runStart(), fps())})` : ""}</button>
          <button
            type="button"
            data-testid="run-end"
            disabled={runStart() === null}
            onClick={() => {
              const a = runStart();
              const b = viewer()?.frame ?? 0;
              if (a === null) return;
              void run((p, c) => actions.runMeasure(p, c, a, b)).then((r) => {
                if (r) {
                  setMeasure(r.measurement);
                  say(`Measured ${r.seconds.toFixed(1)} s`);
                }
              });
            }}
          >
            Mark run end
          </button>
        </div>
        <Show when={stream() && streamNow() !== null}>
          <div class="row small" data-testid="stream-bar">
            <span class="muted">Stream <b data-testid="stream-now">{streamNow()?.toFixed(3)}</b> s</span>
            <button type="button" data-testid="stream-mark-start" onClick={() => setSrcFrom(streamNow()!.toFixed(3))}>Mark run start</button>
            <button type="button" data-testid="stream-mark-end" onClick={() => setSrcTo(streamNow()!.toFixed(3))}>Mark run end</button>
            <button type="button" class="ghost" data-testid="stream-back" onClick={() => void backToCut()}>Back to the cut</button>
          </div>
        </Show>
        <div class="row">
          <Show when={data()?.measurements.length}>
            <label>
              Measurement{" "}
              <select aria-label="Measurement" value={measure() ?? ""} onChange={(e) => setMeasure(e.currentTarget.value || null)}>
                <option value="">choose…</option>
                <For each={data()?.measurements ?? []}>{(m) => <option value={m.id}>{m.value.toFixed(1)} s</option>}</For>
              </select>
            </label>
          </Show>
          <button
            type="button"
            data-testid="make-clock"
            disabled={!measure()}
            onClick={() => {
              const parts = cutView()?.parts ?? [];
              const pick = (n: string) => (n ? parts[Number(n) - 1]?.id : undefined);
              void run((p, c) => actions.clockAdd(p, c, measure()!, running(), doneLabel(), pick(clockFrom()), pick(clockUntil())), "Run clock made");
            }}
          >
            Make run clock
          </button>
        </div>
        {/* the clock's labels and takes, and a run measured in the stream */}
        <details class="more" data-testid="clock-more">
          <summary>Advanced</summary>
          <div class="row">
            <input type="text" class="short" aria-label="Running label" value={running()} onInput={(e) => setRunning(e.currentTarget.value)} />
            <input type="text" class="short" aria-label="Done label" value={doneLabel()} onInput={(e) => setDoneLabel(e.currentTarget.value)} />
            <label class="small">from take <input aria-label="Clock from take" type="number" min="1" class="tiny" value={clockFrom()} onInput={(e) => setClockFrom(e.currentTarget.value)} /></label>
            <label class="small">until take <input aria-label="Clock until take" type="number" min="1" class="tiny" value={clockUntil()} onInput={(e) => setClockUntil(e.currentTarget.value)} /></label>
          </div>
          <h4>A run the cut does not show</h4>
          <form class="row small" onSubmit={(e) => { e.preventDefault(); void openStream(); }}>
            <input aria-label="Stream time to browse (s)" data-testid="stream-at" type="number" step="0.001" class="short"
              placeholder={selWordS() !== null ? `${selWordS()!.toFixed(1)} (word)` : "stream s"} value={goTo()} onInput={(e) => setGoTo(e.currentTarget.value)} />
            <button type="submit" data-testid="stream-open" disabled={goTo().trim() === "" && selWordS() === null} title="Shows the stream around that time (or the word selected in the transcript, in the cut or not)">Browse the stream there</button>
          </form>
          <form class="row small" onSubmit={(e) => { e.preventDefault(); measureSrc(); }}>
            <span class="muted">from the stream (s):</span>
            <input name="from" aria-label="Run start in the stream (s)" type="number" step="0.001" class="short" value={srcFrom()} onInput={(e) => setSrcFrom(e.currentTarget.value)} />
            <input name="to" aria-label="Run end in the stream (s)" type="number" step="0.001" class="short" value={srcTo()} onInput={(e) => setSrcTo(e.currentTarget.value)} />
            <button type="submit" data-testid="stream-measure" disabled={!srcFrom() || !srcTo()}>Measure</button>
          </form>
        </details>
      </div>

      <Show when={(cutView()?.kind ?? "short") === "short"}>
        <EndCardLongform />
      </Show>

      <div>
        <h3>On screen</h3>
        <For each={data()?.overlays ?? []} fallback={<p class="muted small">No hook, pills or clock yet.</p>}>
          {(o) => (
            <div class="row overlay-row" data-role={o.role ?? ""}>
              <span class="tag">{roleName(o.role)}</span>
              <span class="ov-text">{o.counter ? `${o.counter.running} → ${o.counter.done}` : o.text.replace("\n", " ")}</span>
              <span class="muted small">{tc(o.start, fps())}–{tc(o.end, fps())}</span>
              <button type="button" class="ghost" aria-label={`Seek to the ${roleName(o.role)}`} onClick={() => o.start !== null && void attempt(() => actions.viewerSeek(o.start!))}>▶</button>
              <button type="button" class="ghost" aria-label={`Remove the ${roleName(o.role)}`} onClick={() => void run((p, c) => actions.overlayDelete(p, c, o.id))}>✕</button>
            </div>
          )}
        </For>
      </div>
    </section>
  );
}
