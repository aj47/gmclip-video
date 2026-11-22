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
import { Switch } from "../design/components";
import { packagingActions } from "../actions";
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
    r === "text_hook" ? "Hook" : r === "section_pill" ? "Section label" : r === "run_clock" ? "Stopwatch" : r ?? "Text";

  // the overlays each row owns (the "On screen" list is gone: a row shows its own)
  const ovs = (role: string) => (data()?.overlays ?? []).filter((o) => o.role === role);
  const hookOv = () => ovs("text_hook")[0];
  // a row the person switched on before it has anything (null: it follows what is there)
  const [hlAsk, setHlAsk] = createSignal<boolean | null>(null);
  const [hookAsk, setHookAsk] = createSignal(false);
  const [labelAsk, setLabelAsk] = createSignal(false);
  const [clockAsk, setClockAsk] = createSignal(false);
  const [openAsk, setOpenAsk] = createSignal<boolean | null>(null);
  const [openings] = createResource(key, async (k) => ((cutView()?.kind ?? "short") === "short" ? ((await packagingActions.openings(k.p, k.c).catch(() => null))?.versions.length ?? 0) : 0));
  const hlOn = () => hlAsk() ?? (data()?.keyword_terms ?? []).length > 0;
  const hookOn = () => !!hookOv() || hookAsk();
  const labelOn = () => ovs("section_pill").length > 0 || labelAsk();
  const clockOn = () => ovs("run_clock").length > 0 || clockAsk();
  const openOn = () => openAsk() ?? (openings() ?? 0) > 1;
  /** Removes a row's overlays (switching it off); ⌘Z brings them back. */
  const removeAll = async (role: string, what: string) => {
    for (const o of ovs(role)) await run((p, c) => actions.overlayDelete(p, c, o.id));
    if (ovs(role).length) say(`${what} off — ⌘Z brings it back`);
  };
  const setHookNow = () => {
    const w = selWord();
    const text = hook().trim() || hookOv()?.text || "";
    if (!text) return;
    void run((p, c) => actions.hookSet(p, c, text, w ? w.i : null, w ? null : Math.round(Number(hookSecs() || "5") * 1000)), w ? `Hook set, until “${w.text}”` : `Hook set for ${hookSecs() || "5"} s`).then(() => setHook(""));
  };
  /** One overlay of a row: its words, when, ▶ and ✕. */
  const OvLine = (props: { o: { id: string; role: string | null; text: string; start: number | null; end: number | null; counter?: { running: string; done: string } | null } }) => (
    <div class="row overlay-row" data-role={props.o.role ?? ""}>
      <span class="ov-text">{props.o.counter ? `${props.o.counter.running} → ${props.o.counter.done}` : props.o.text.replace("\n", " ")}</span>
      <span class="muted small">{tc(props.o.start, fps())}–{tc(props.o.end, fps())}</span>
      <button type="button" class="ghost" aria-label={`Seek to the ${roleName(props.o.role)}`} onClick={() => props.o.start !== null && void attempt(() => actions.viewerSeek(props.o.start!))}>▶</button>
      <button type="button" class="ghost" aria-label={`Remove the ${roleName(props.o.role)}`} onClick={() => void run((p, c) => actions.overlayDelete(p, c, props.o.id))}>✕</button>
    </div>
  );

  return (
    <section class="short-panel cap-rows" aria-label="Captions">
      {/* Every feature is a row with a switch (AJ 2026-11-22): its settings show only while
          it is on. Switching off a row that made something removes it (⌘Z brings it back). */}
      <div class="cap-row" data-on={data()?.captions ? "1" : "0"}>
        <Switch label={<span class="cap-row-title">Captions</span>} hint="1–3 words at a time, from what you said" on={data()?.captions ?? false} disabled={!data()} testid="captions-on" onChange={(on) => void run((p, c) => actions.captionsSet(p, c, on), on ? "Captions on" : "Captions off")} />
        <Show when={data()?.captions}>
          <div class="cap-row-body">
            <span class="muted small" data-testid="caption-count">{(cutView()?.captions ?? []).filter((c) => c.role === "caption").length} captions on the cut</span>
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
              <h4 class="cap-sub">Names to fix</h4>
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
              <h4 class="cap-sub">Corrections</h4>
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
              <h4 class="cap-sub">Names</h4>
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
              <h4 class="cap-sub">New correction</h4>
              <p class="muted small">Fix a word the speech model misheard (audio is untouched).</p>
              <form class="row" onSubmit={(e) => { e.preventDefault(); void run((p, c) => actions.captionFix(p, c, fixFrom(), fixTo()), "Corrected everywhere in this cut").then(() => { setFixFrom(""); setFixTo(""); }); }}>
                <input type="text" aria-label="Heard as" placeholder="heard as (XD.dev)" value={fixFrom()} onInput={(e) => setFixFrom(e.currentTarget.value)} />
                <input type="text" aria-label="Show as" placeholder="show as (exe.dev)" value={fixTo()} onInput={(e) => setFixTo(e.currentTarget.value)} />
                <button type="submit" disabled={!fixFrom().trim() || !fixTo().trim()}>Fix</button>
              </form>
            </details>
          </div>
        </Show>
        <Show when={!data()?.captions && data()?.imported_captions}>
          <p class="muted small">This cut shows imported captions; switching captions on builds them from the transcript instead.</p>
        </Show>
      </div>

      <div class="cap-row" data-on={hlOn() ? "1" : "0"}>
        <Switch label={<span class="cap-row-title">Highlight words</span>} hint="Starred words show in yellow: list them, or select words in the transcript and press *" on={hlOn()} disabled={!data()} testid="row-highlight" onChange={(on) => { setHlAsk(on); if (!on && (data()?.keyword_terms ?? []).length) void run((p, c) => actions.keywordsSet(p, c, ""), "Highlights off — ⌘Z brings them back").then(() => setTerms(null)); }} />
        <Show when={hlOn()}>
          <div class="cap-row-body">
            <form class="row" onSubmit={(e) => { e.preventDefault(); void run((p, c) => actions.keywordsSet(p, c, shownTerms()), "Keywords starred across the cut").then(() => setTerms(null)); }}>
              <input type="text" aria-label="Keywords" data-testid="keywords" placeholder="keywords: two minutes, NVIDIA, 5080, GPU" value={shownTerms()} onInput={(e) => setTerms(e.currentTarget.value)} />
              <button type="submit" data-testid="keywords-star" disabled={!data() || terms() === null}>Star</button>
            </form>
          </div>
        </Show>
      </div>

      <div class="cap-row" data-on={hookOn() ? "1" : "0"}>
        <Switch label={<span class="cap-row-title">Text hook</span>} hint="The line on screen from the first frame" on={hookOn()} disabled={!data()} testid="row-hook" onChange={(on) => { setHookAsk(on); if (!on) void removeAll("text_hook", "Hook"); }} />
        {/* one field: Enter sets it (5 s, or until the word selected in the transcript) */}
        <Show when={hookOn()}>
          <form class="cap-row-body" onSubmit={(e) => { e.preventDefault(); setHookNow(); }}>
            <For each={ovs("text_hook")}>{(o) => <OvLine o={o} />}</For>
            {/* (two fields and no button: a form would not submit on Enter by itself) */}
            <input type="text" aria-label="Hook text" placeholder={hookOv()?.text ?? "Type the hook, press Enter"} value={hook()} onInput={(e) => setHook(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); setHookNow(); } }} />
            <div class="row hook-time">
              <Show
                when={selWord()}
                fallback={<label class="hook-secs">Shows for <input type="number" class="num" aria-label="Hook seconds" min="1" max="60" step="0.5" value={hookSecs()} onInput={(e) => setHookSecs(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); setHookNow(); } }} /> s</label>}
              >
                {(w) => <span class="muted small">Until “{w().text}” (the selected word)</span>}
              </Show>
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
            <HookLines
              until={() => selWord()?.i ?? null}
              durMs={() => Math.round(Number(hookSecs()) * 1000)}
              onUsed={(t) => {
                setHook(t);
                void refetch();
              }}
            />
          </form>
        </Show>
      </div>

      <div class="cap-row" data-on={labelOn() ? "1" : "0"}>
        <Switch label={<span class="cap-row-title">Section label</span>} hint="A tag on screen that names what this part shows" on={labelOn()} disabled={!data()} testid="row-label" onChange={(on) => { setLabelAsk(on); if (!on) void removeAll("section_pill", "Section labels"); }} />
        <Show when={labelOn()}>
          <div class="cap-row-body">
            <For each={ovs("section_pill")}>{(o) => <OvLine o={o} />}</For>
            <form class="row label-add" onSubmit={(e) => { e.preventDefault(); void run((p, c) => actions.pillAdd(p, c, viewer()?.frame ?? 0, pill()), "Section label added to this part").then(() => setPill("")); }}>
              <input type="text" aria-label="Pill text" placeholder="e.g. MiniMax H3 on my PC" value={pill()} onInput={(e) => setPill(e.currentTarget.value)} />
              <button type="submit" disabled={!pill().trim()}>Add to this part</button>
            </form>
          </div>
        </Show>
      </div>

      <div class="cap-row" data-on={clockOn() ? "1" : "0"}>
        <Switch label={<span class="cap-row-title">Stopwatch</span>} hint="How long something really took, even across your cuts: mark the frame it starts and the frame the result shows" on={clockOn()} disabled={!data()} testid="row-stopwatch" onChange={(on) => { setClockAsk(on); if (!on) void removeAll("run_clock", "Stopwatch"); }} />
        <Show when={clockOn()}>
          <div class="cap-row-body">
            <For each={ovs("run_clock")}>{(o) => <OvLine o={o} />}</For>
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
                  void run((p, c) => actions.clockAdd(p, c, measure()!, running(), doneLabel(), pick(clockFrom()), pick(clockUntil())), "Stopwatch made");
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
              <h4 class="cap-sub">A run the cut does not show</h4>
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
        </Show>
      </div>

      <Show when={(cutView()?.kind ?? "short") === "short"}>
        <div class="cap-row" data-on={openOn() ? "1" : "0"}>
          <Switch label={<span class="cap-row-title">Openings</span>} hint="Up to three other openings, each sharing everything after it: compare them and make one the main version" on={openOn()} disabled={!data()} testid="row-openings" onChange={(on) => setOpenAsk(on)} />
          <Show when={openOn()}>
            <div class="cap-row-body"><Openings /></div>
          </Show>
        </div>
        <EndCardLongform />
      </Show>
    </section>
  );
}
