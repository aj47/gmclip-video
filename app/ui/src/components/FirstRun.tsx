// First run for a stranger (SPEC §12.5, adapted to AJ's direction; lane S): four questions
// with pictures, the speech model downloading while they are answered, one summary line
// per default, one system sheet. Also the Home welcome card, the first-run checklist and
// the Settings summary that reopens the sheet. Help is in tooltips, not inline text.
import { thisMachine } from "../platform";
import { createEffect, createResource, createSignal, For, onMount, Show, type JSX } from "solid-js";
import { actions, type AgentClient, type Checklist, type SetupAnswers, type SetupPreview, type SummaryLine } from "../actions";
import { Banner, Button } from "../design/components";
import { Icon } from "../design/Icon";
import { coachSeen, markCoachSeen } from "../prefs";
import { attempt, modelsProgress, say } from "../store";
import { bytes } from "./Reliability";
import "./firstrun.css";

// ---- pictures (inline SVG, drawn in the theme's colours) ------------------------------

function Frame(props: { w: number; h: number; children: JSX.Element }) {
  return (
    <svg class="fr-pic" viewBox={`0 0 ${props.w} ${props.h}`} aria-hidden="true">
      <rect x="1" y="1" width={props.w - 2} height={props.h - 2} rx="6" class="fr-frame" />
      {props.children}
    </svg>
  );
}

function Face(props: { x: number; y: number; r: number }) {
  return (
    <>
      <circle cx={props.x} cy={props.y} r={props.r} class="fr-face" />
      <path d={`M${props.x - props.r * 1.6} ${props.y + props.r * 2.6} q${props.r * 1.6} ${-props.r * 2.2} ${props.r * 3.2} 0`} class="fr-face" />
    </>
  );
}

export function KitPicture(props: { kit: string }) {
  return (
    <Frame w={64} h={96}>
      {props.kit === "gaming-variety" ? (
        <>
          <rect x="4" y="4" width="56" height="26" rx="3" class="fr-cam" />
          <Face x={32} y={13} r={5} />
          <line x1="4" y1="31" x2="60" y2="31" class="fr-seam" />
          <rect x="4" y="34" width="56" height="58" rx="3" class="fr-screen" />
          <path d="M14 80 l10 -14 l8 9 l7 -6 l12 11z" class="fr-glyph" />
        </>
      ) : props.kit === "screen-face" ? (
        <>
          <rect x="4" y="12" width="56" height="34" rx="3" class="fr-screen" />
          <path d="M10 22 h22 M10 28 h30 M10 34 h16" class="fr-code" />
          <rect x="12" y="54" width="40" height="30" rx="3" class="fr-cam" />
          <Face x={32} y={64} r={5} />
        </>
      ) : props.kit === "talking-head" ? (
        <>
          <rect x="4" y="4" width="56" height="88" rx="3" class="fr-cam" />
          <Face x={32} y={38} r={9} />
          <path d="M14 72 h36 M18 78 h28" class="fr-code" />
        </>
      ) : (
        <>
          <rect x="4" y="4" width="56" height="88" rx="3" class="fr-blank" />
          <path d="M32 40 v16 M24 48 h16" class="fr-code" />
        </>
      )}
    </Frame>
  );
}

export function PlatformPicture(props: { id: string }) {
  const wide = () => props.id === "youtube";
  return wide() ? (
    <Frame w={96} h={60}>
      <rect x="4" y="4" width="88" height="52" rx="3" class="fr-screen" />
      <path d="M42 20 l16 10 l-16 10z" class="fr-glyph" />
    </Frame>
  ) : (
    <Frame w={56} h={96}>
      <rect x="4" y="4" width="48" height="88" rx="4" class="fr-screen" />
      {/* the app's own buttons: the zones captions stay out of */}
      <rect x="40" y="46" width="9" height="34" rx="2" class="fr-zone" />
      <rect x="4" y="74" width="34" height="18" class="fr-zone" />
      <text x="28" y="40" text-anchor="middle" class="fr-label">
        {props.id === "tiktok" ? "TT" : props.id === "shorts" ? "S" : "IG"}
      </text>
    </Frame>
  );
}

// ---- the speech model -----------------------------------------------------------------

/** The speech model download: started when setup opens, with size, licence, a metered
 * warning, progress, Pause / Resume and "Don't transcribe for now". */
export function ModelDownload(props: { autoStart?: boolean; noTranscribe: boolean; onNoTranscribe: (on: boolean) => void }) {
  const [st, { refetch }] = createResource(async () => (await attempt(actions.modelsStatus)) ?? null);
  const [started, setStarted] = createSignal(false);
  const start = async () => {
    setStarted(true);
    await attempt(actions.modelsDownload);
    void refetch();
  };
  // a human opened the setup: start the download (unless they said not now)
  createEffect(() => {
    const s = st();
    if (props.autoStart && s && !s.installed && !s.running && !started() && !props.noTranscribe) void start();
  });
  const p = () => modelsProgress();
  const running = () => p()?.state === "running" || (!p() && st()?.running);
  const pct = () => {
    const x = p();
    return x?.total ? Math.min(100, Math.round(((x.done ?? 0) / x.total) * 100)) : null;
  };
  createEffect(() => {
    if (p()?.state === "done") void refetch();
  });
  const licences = () => (st()?.licences ?? []).map((l) => `${l.id}: weights ${l.weights}, code ${l.code}`).join(" · ");
  return (
    <section class="fr-model" aria-label="Speech model" data-testid="setup-model">
      <Show when={st()} fallback={<p class="muted small">Checking the speech model…</p>}>
        {(s) => (
          <Show
            when={!s().installed && p()?.state !== "done"}
            fallback={<p class="small"><Icon name="circle-check" size={14} /> Speech model ready — transcription runs on {thisMachine}.</p>}
          >
            <div class="row">
              <Icon name="audio-lines" size={16} />
              <b>Speech model</b>
              <span class="muted small" data-tip={licences() || undefined}>{bytes(s().bytes ?? 0)} · runs on {thisMachine} · open licences</span>
              <span class="spacer" />
              <Show when={!props.noTranscribe}>
                <Show
                  when={running()}
                  fallback={<Button size="sm" icon="upload" data-testid="model-download" onClick={() => void start()}>{(s().partial_bytes ?? 0) > 0 || p()?.state === "paused" ? "Resume" : "Download"}</Button>}
                >
                  <Button size="sm" variant="ghost" icon="pause" data-testid="model-pause" onClick={() => void attempt(actions.modelsPause)}>Pause</Button>
                </Show>
              </Show>
              <Button size="sm" variant="ghost" data-testid="model-later" tooltip={props.noTranscribe ? "Transcribe new videos again" : "Videos open and play without a transcript; each shows “Not transcribed” with one button"} onClick={() => props.onNoTranscribe(!props.noTranscribe)}>
                {props.noTranscribe ? "Transcribe after all" : "Don't transcribe for now"}
              </Button>
            </div>
            <Show when={running() && pct() !== null}>
              <progress max={100} value={pct()!} aria-label="Speech model download" />
            </Show>
            <Show when={p()?.state === "failed"}>
              <p class="small err">{p()!.error}</p>
            </Show>
            <p class="muted small" data-testid="model-metered">
              On a phone hotspot or a capped plan? Pause now and resume later — it continues where it stopped.
            </p>
          </Show>
        )}
      </Show>
    </section>
  );
}

// ---- the sheet ------------------------------------------------------------------------

const AGENT_TIP =
  "gmclip starts your agent CLI only when you press “Run with my agent”, on its own plan or subscription, with only gmclip's tools for that run (MCP). No API key, nothing billed by gmclip. To use gmclip from a session you run yourself, use Home › Other ways to open › Connect an agent.";

function Tile(props: { on: boolean; onClick: () => void; label: string; sub?: string; disabled?: boolean; tip?: string; testid?: string; multi?: boolean; children?: JSX.Element }) {
  return (
    <button
      type="button"
      class={`fr-tile${props.on ? " on" : ""}`}
      role={props.multi ? "checkbox" : "radio"}
      aria-checked={props.on}
      disabled={props.disabled}
      data-testid={props.testid}
      data-tip={props.tip}
      onClick={() => props.onClick()}
    >
      {props.children}
      <span class="fr-tile-label">{props.label}</span>
      <Show when={props.sub}><span class="fr-tile-sub">{props.sub}</span></Show>
    </button>
  );
}

export function SetupSheet(props: { onClose: () => void; onDone: () => void }) {
  const [pv] = createResource(async () => (await attempt(actions.setupPreview)) ?? null);
  const [kit, setKit] = createSignal<string | null>(null);
  const [platforms, setPlatforms] = createSignal<string[]>([]);
  const [finder, setFinder] = createSignal<SetupAnswers["finder"] | null>(null);
  const [harness, setHarness] = createSignal<AgentClient["harness"] | null>(null);
  const [rec, setRec] = createSignal("");
  // typed by the user: the prefill (which can resolve late) never overwrites it
  let recTyped = false;
  const [noTx, setNoTx] = createSignal(false);
  const [summary, setSummary] = createSignal<SummaryLine[] | null>(null);
  const [busy, setBusy] = createSignal(false);
  // prefill from earlier answers (Settings › Change…)
  createEffect(() => {
    const p = pv();
    if (!p) return;
    const a = p.answers;
    setNoTx(p.no_transcribe);
    if (!recTyped) setRec(a?.recordings ?? p.recordings_default ?? "");
    if (a) {
      setKit(a.kit);
      setPlatforms(a.platforms);
      setFinder(a.finder);
      setHarness(a.harness ?? null);
    }
  });
  const answers = (): SetupAnswers | null => {
    const k = kit();
    const f = finder();
    if (!k || !f || !platforms().length || (f === "agent" && !harness())) return null;
    return { kit: k, platforms: platforms(), finder: f, harness: f === "agent" ? harness() : null, recordings: rec().trim() || null };
  };
  // the summary follows the answers: the latest request's answer only (an earlier
  // one can arrive after it)
  let summarySeq = 0;
  createEffect(() => {
    const a = answers();
    const mine = ++summarySeq;
    if (!a) return setSummary(null);
    void actions.setupApply(a, true).then(
      (r) => {
        if (mine === summarySeq) setSummary(r.summary);
      },
      () => {
        if (mine === summarySeq) setSummary(null);
      },
    );
  });
  const togglePlatform = (id: string) => setPlatforms(platforms().includes(id) ? platforms().filter((x) => x !== id) : [...platforms(), id]);
  const noTranscribe = async (on: boolean) => {
    const r = await attempt(() => actions.setupNoTranscribe(on));
    if (r) setNoTx(r.no_transcribe);
    if (on) void attempt(actions.modelsPause);
  };
  const apply = async () => {
    const a = answers();
    if (!a) return;
    setBusy(true);
    const r = await attempt(() => actions.setupApply(a));
    setBusy(false);
    if (r) {
      say("gmclip is set up — change any of it in Settings");
      props.onDone();
    }
  };
  const usable = (c: AgentClient) => c.installed && c.status !== "blocked";
  return (
    <div class="keys-backdrop" onPointerDown={(e) => { if (e.target === e.currentTarget) props.onClose(); }}>
      <section class="keys-dialog fr-sheet" role="dialog" aria-modal="true" aria-label="Set up gmclip" data-testid="setup-sheet">
        <header>
          <h2>Set up gmclip</h2>
          <button type="button" class="ghost" onClick={() => props.onClose()} aria-label="Close">✕</button>
        </header>
        <Show when={pv()} fallback={<p class="muted">Loading…</p>}>
          {(p) => (
            <>
              <ModelDownload autoStart noTranscribe={noTx()} onNoTranscribe={(on) => void noTranscribe(on)} />

              <fieldset class="fr-q" data-testid="q-kit">
                <legend>1 · What do you record?</legend>
                <div class="fr-tiles" role="radiogroup" aria-label="What do you record?">
                  <For each={p().kits}>
                    {(k) => (
                      <Tile on={kit() === k.id} onClick={() => setKit(k.id)} label={k.label} tip={k.blurb} testid={`kit-${k.id}`}>
                        <KitPicture kit={k.id} />
                      </Tile>
                    )}
                  </For>
                </div>
              </fieldset>

              <fieldset class="fr-q" data-testid="q-platforms">
                <legend>2 · Where do you post?</legend>
                <div class="fr-tiles" role="group" aria-label="Where do you post?">
                  <For each={p().platforms}>
                    {(x) => (
                      <Tile multi on={platforms().includes(x.id)} onClick={() => togglePlatform(x.id)} label={x.label} sub={x.framing} tip={`${x.framing} at −14 LUFS; captions and faces kept out of ${x.label}'s buttons`} testid={`platform-${x.id}`}>
                        <PlatformPicture id={x.id} />
                      </Tile>
                    )}
                  </For>
                </div>
              </fieldset>

              <fieldset class="fr-q" data-testid="q-finder">
                <legend>
                  3 · Find moments with
                  <span class="fr-help" data-tip={AGENT_TIP} tabindex="0" aria-label="How your agent connects"><Icon name="circle-alert" size={13} /></span>
                </legend>
                <div class="fr-tiles" role="radiogroup" aria-label="Find moments with">
                  <For each={p().agents.clients}>
                    {(c) => (
                      <Tile
                        on={finder() === "agent" && harness() === c.harness}
                        disabled={!usable(c)}
                        onClick={() => { setFinder("agent"); setHarness(c.harness); }}
                        label={c.label}
                        sub={c.installed ? (c.version ?? "found") : "not found"}
                        tip={usable(c) ? `Runs on ${c.login}; ${c.how}.` : (c.reason ?? `not on ${thisMachine}`)}
                        testid={`agent-${c.harness}`}
                      >
                        <span class="fr-agent-icon"><Icon name="bot" size={22} /></span>
                      </Tile>
                    )}
                  </For>
                  <Tile on={finder() === "self"} onClick={() => { setFinder("self"); setHarness(null); }} label="I'll pick moments myself" tip="No agent runs; you scrub, search and mark moments yourself" testid="agent-self">
                    <span class="fr-agent-icon"><Icon name="pointer" size={22} /></span>
                  </Tile>
                </div>
                <p class="muted small fr-media" data-tip="Off by default and asked per run: when you allow it for a run, your agent may look at frames and listen to the audio of that cut only">
                  <input type="checkbox" disabled aria-label="Let the agent see and hear the footage" /> Let the agent see and hear the footage — asked per run
                </p>
              </fieldset>

              <fieldset class="fr-q" data-testid="q-recordings">
                <legend>4 · Where are your recordings?</legend>
                <div class="row">
                  <Icon name="folder-open" size={16} />
                  <input type="text" class="fr-path" aria-label="Recordings folder" value={rec()} onInput={(e) => {
                    recTyped = true;
                    setRec(e.currentTarget.value);
                  }} placeholder={p().recordings_default ?? "/Users/you/Movies"} data-tip="The file picker starts here, and your agent may read the videos in it (never write)" />
                </div>
              </fieldset>

              <section class="fr-summary" aria-label="What this sets" data-testid="setup-summary">
                <h3>
                  What this sets
                  <span class="fr-help" tabindex="0" aria-label="About these settings" data-tip={`Every line can be changed later on the Settings page. Data leaving ${thisMachine}: only what your own agent sends on its own plan when you run it; gmclip uploads nothing.`}><Icon name="circle-alert" size={13} /></span>
                </h3>
                <Show when={summary()} fallback={<p class="muted small">Answer the four questions to see every default.</p>}>
                  {(s) => (
                    <dl>
                      <For each={s()}>
                        {(l) => (
                          <>
                            <dt>{l.label}</dt>
                            <dd data-id={l.id}>{l.value}</dd>
                          </>
                        )}
                      </For>
                    </dl>
                  )}
                </Show>
              </section>
              <div class="row">
                <Button variant="primary" icon="check" data-testid="setup-go" disabled={!answers() || busy()} tooltip="One system dialog lists every permission this gives; nothing changes until you approve it there" onClick={() => void apply()}>
                  {busy() ? "Setting up…" : "Set up…"}
                </Button>
                <Button variant="ghost" onClick={() => props.onClose()}>Later</Button>
              </div>
            </>
          )}
        </Show>
      </section>
    </div>
  );
}

// ---- Home -----------------------------------------------------------------------------

/** Home's welcome card until the setup is done. */
/** One line until setup is done or it is dismissed (a coach mark: Settings › Setup stays). */
export function WelcomeCard(props: { tick: number; onSetup: () => void }) {
  const [pv] = createResource(() => props.tick + 1, async () => (await attempt(actions.setupState)) ?? null);
  return (
    <Show when={pv() && !pv()!.done && !coachSeen("welcome")}>
      <section class="fr-welcome" aria-label="Welcome" data-testid="welcome">
        <Icon name="sparkles" size={15} />
        <span><strong>Welcome to gmclip</strong> <span class="muted">· Four questions set it up for what you record and where you post.</span></span>
        <span class="spacer" />
        <Button size="sm" variant="primary" data-testid="setup-open" onClick={() => props.onSetup()}>Set up…</Button>
        <Button size="sm" variant="ghost" icon="x" data-testid="welcome-dismiss" tooltip="Hide this; Settings › Setup asks the same" onClick={() => markCoachSeen("welcome")}>Dismiss</Button>
      </section>
    </Show>
  );
}

/** After the first real import: layout → pick a moment → trim by words → check captions → export. */
export function FirstChecklist(props: { tick: number }) {
  const [cl, { refetch, mutate }] = createResource(() => props.tick, async () => (await attempt(actions.checklistView)) ?? null);
  onMount(() => void refetch());
  // the first step not done yet: marked, and its how said under the steps
  const next = () => cl()?.steps.find((x) => !x.done);
  const dismiss = async () => {
    const r = await attempt(() => actions.checklistDismiss(true));
    if (r) mutate(r as Checklist);
  };
  return (
    <Show when={cl()?.show}>
      <section class="fr-checklist" aria-label="Your first short" data-testid="checklist">
        <div class="fr-cl-row">
          <h2>Your first short</h2>
          <ol>
            <For each={cl()!.steps}>
              {(s) => (
                <li classList={{ done: s.done, next: s.id === next()?.id }} data-testid="checklist-step" data-step={s.id} data-done={s.done ? "yes" : "no"} data-tip={s.how}>
                  <span class="fr-cl-n" aria-hidden="true" />
                  <span>{s.label}</span>
                </li>
              )}
            </For>
          </ol>
          <span class="spacer" />
          <Button size="sm" variant="ghost" icon="x" data-testid="checklist-dismiss" tooltip="Hide this checklist" onClick={() => void dismiss()}>Dismiss</Button>
        </div>
        <Show when={next()}>{(n) => <p class="fr-cl-next muted small">Next: {n().how}</p>}</Show>
      </section>
    </Show>
  );
}

// ---- Settings -------------------------------------------------------------------------

/** Settings › Setup: one line per default, all changed from one sheet. */
export function SetupSummary(props: { tick: number; onChange: () => void }) {
  const [lines] = createResource(() => props.tick + 1, async () => {
    const p = await attempt(actions.setupState);
    if (!p?.answers) return { p, lines: null as SummaryLine[] | null };
    try {
      return { p, lines: (await actions.setupApply(p.answers, true)).summary };
    } catch {
      return { p, lines: null };
    }
  });
  return (
    <section class="settings-section" aria-label="Setup" data-testid="settings-setup">
      <h3>Setup</h3>
      <Show when={lines()?.lines} fallback={<p class="small">Not set up yet. <Button size="sm" icon="sparkles" data-testid="settings-setup-open" onClick={() => props.onChange()}>Set up…</Button></p>}>
        {(l) => (
          <>
            <dl class="setup-defaults">
              <For each={l()}>
                {(x) => (
                  <>
                    <dt>{x.label}</dt>
                    <dd data-id={x.id}>{x.value}</dd>
                  </>
                )}
              </For>
            </dl>
            <Button size="sm" icon="pencil" data-testid="settings-setup-change" tooltip="Reopens the four questions with your answers; changes go through one system dialog" onClick={() => props.onChange()}>Change…</Button>
          </>
        )}
      </Show>
    </section>
  );
}

export type { SetupPreview };
