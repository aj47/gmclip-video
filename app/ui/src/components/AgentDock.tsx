// The floating agent (decision 0035): the sun in the corner of the project views, and a
// big one on Home; a click opens a small chat. Dropdowns pick what the agent works on
// (this cut, this project, another project, the library), which agent (the default
// first) and its model (the agent's own default first; the list is what that agent
// offered in its last session); the first message starts a session. While one runs, the chat draws what it does —
// status, questions, plan, the cuts it touches — and "Follow" opens each cut as the agent
// edits it, so the timeline changes in front of you. The full log is behind "Show log".
import { createEffect, createMemo, createResource, createSignal, For, on, Show } from "solid-js";
import { sessionActions, type AgentClient, type SessionAccess } from "../actions";
import { IconButton } from "../design/components";
import { canPickUp, isLive, lastAccess, lastModel, loadSession, openSession, sessions, setLastAccess, setLastModel, startSession, watchSessions } from "../agentsState";
import { attempt, cutView, openCut, projects, route, setRoute } from "../store";
import { AgentFace, Composer, SessionBody } from "./SessionWork";
import "./agents.css";

const FOLLOW = "gmclip.agents.follow";

const STARTERS = ["Tighten this cut: drop pauses and fillers, hook first.", "Find the strongest moments and make shorts from them.", "Fix misheard words in the captions."];

/** `project`: the project views' dock; none (Home): it starts on the library. `big`: Home's sun. */
export function AgentDock(props: { project?: string; big?: boolean }) {
  onMountWatch();
  const [open, setOpen] = createSignal(false);
  const [dockId, setDockId] = createSignal<string | null>(null);
  const [target, setTarget] = createSignal<string>("");
  const [harness, setHarnessRaw] = createSignal<AgentClient["harness"] | "">("");
  const [model, setModelRaw] = createSignal("");
  const setHarness = (h: AgentClient["harness"]) => {
    setHarnessRaw(h);
    setModelRaw(lastModel(h));
  };
  const setModel = (m: string) => {
    setModelRaw(m);
    if (harness()) setLastModel(harness(), m);
  };
  const models = () => agents()?.agents.find((x) => x.harness === harness())?.models ?? [];
  const [access, setAccessRaw] = createSignal<SessionAccess>(lastAccess());
  const setAccess = (a: SessionAccess) => {
    setAccessRaw(a);
    setLastAccess(a);
  };
  const [message, setMessage] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [follow, setFollowRaw] = createSignal(localStorage.getItem(FOLLOW) !== "0");
  const setFollow = (on: boolean) => {
    setFollowRaw(on);
    localStorage.setItem(FOLLOW, on ? "1" : "0");
  };
  const [agents] = createResource(open, async () => (await attempt(sessionActions.agents)) ?? null);
  createEffect(() => {
    const a = agents();
    if (a && !harness()) setHarness(a.agents.find((x) => x.harness === a.default && x.available)?.harness ?? a.agents.find((x) => x.available)?.harness ?? a.default);
  });

  // on Home every session is "here"
  const here = () => (props.project ? sessions().filter((s) => s.target.project === props.project || s.work?.some((w) => w.project === props.project)) : sessions());
  const liveHere = () => here().filter((s) => isLive(s.state));
  const waitingHere = () => liveHere().filter((s) => s.state === "waiting").length;
  const current = createMemo(() => sessions().find((s) => s.id === dockId()) ?? null);

  // opening the chat shows the newest live session here, if there is one; Home's sun
  // opens on a new one (its running sessions are under "Recent here")
  createEffect(
    on(open, (o) => {
      if (!o) return;
      const id = dockId() ?? (props.project ? liveHere()[0]?.id : null) ?? null;
      setDockId(id);
      if (id && openSession() !== id) void loadSession(id);
      const v = cutView();
      if (!target()) setTarget(!props.project ? "library" : v && v.project === props.project ? `cut:${v.project}:${v.cut}` : `project:${props.project}`);
    }),
  );
  // the Agents view may have opened another session: the dock follows its own
  createEffect(() => {
    const id = dockId();
    if (open() && id && openSession() !== id) void loadSession(id);
  });

  // Follow: open the cut the agent is editing now
  createEffect(
    on(
      () => {
        const s = current();
        if (!s || s.state !== "working" || !follow()) return null;
        const w = [...(s.work ?? [])].sort((a, b) => b.last_ms - a.last_ms)[0];
        return w ? `${w.project}|${w.cut}|${w.last_ms}` : null;
      },
      (k) => {
        if (!k) return;
        const [p, c] = k.split("|");
        const r = route();
        if (p && c && r.view === "cut" && r.cut !== c) void openCut(p, c);
      },
    ),
  );

  const targetArgs = () => {
    const t = target();
    if (t === "library") return {};
    const [kind, p, c] = t.split(":");
    if (kind === "cut" && p && c) return { project: p, cut: c };
    if (kind === "project" && p) return { project: p };
    return props.project ? { project: props.project } : {};
  };
  // Enter before the agents have loaded: start once the default one is known
  const [pending, setPending] = createSignal(false);
  createEffect(() => {
    if (pending() && harness()) {
      setPending(false);
      void start();
    }
  });
  const start = async () => {
    const m = message().trim();
    const h = harness();
    if (m && !h) setPending(true);
    if (!m || !h || busy()) return;
    setBusy(true);
    const id = await startSession(targetArgs(), h, access(), m, false, undefined, models().some((x) => x.id === model()) ? model() : undefined);
    setBusy(false);
    if (id) {
      setDockId(id);
      setMessage("");
    }
  };
  const cut = () => {
    const v = cutView();
    return props.project && v && v.project === props.project ? v : null;
  };
  const projName = (id: string) => projects().find((p) => p.id === id)?.name ?? "project";

  return (
    <div class={`ag-dock${props.big ? " big" : ""}`} data-testid="agent-dock">
      <Show when={open()}>
        <section class="ag-dock-panel" role="dialog" aria-label="Agent" data-testid="agent-dock-panel">
          <header class="ag-dock-head">
            <Show
              when={!current()}
              fallback={
                <div class="ag-dock-title">
                  <AgentFace state={current()!.state} size={16} />
                  <span class="ag-dock-name" title={current()!.title}>
                    {current()!.harness_label} · {current()!.target.label}
                  </span>
                </div>
              }
            >
              <div class="ag-dock-pickers">
              <select aria-label="Work on" data-testid="dock-target" value={target()} onChange={(e) => setTarget(e.currentTarget.value)}>
                <Show when={!props.project}>
                  <option value="library">The whole library</option>
                </Show>
                <Show when={cut()}>{(v) => <option value={`cut:${v().project}:${v().cut}`}>This cut: {v().title}</option>}</Show>
                <Show when={props.project}>{(p) => <option value={`project:${p()}`}>This project: {projName(p())}</option>}</Show>
                <For each={projects().filter((p) => p.id !== props.project)}>{(p) => <option value={`project:${p.id}`}>{p.name}</option>}</For>
                <Show when={props.project}>
                  <option value="library">The whole library</option>
                </Show>
              </select>
              <select aria-label="Agent" data-testid="dock-agent" value={harness()} onChange={(e) => setHarness(e.currentTarget.value as AgentClient["harness"])}>
                <For each={agents()?.agents ?? []}>
                  {(a) => (
                    <option value={a.harness} disabled={!a.available} selected={a.harness === harness()}>
                      {a.label}
                      {agents()?.default === a.harness ? " (default)" : ""}
                      {a.available ? "" : " — not found"}
                    </option>
                  )}
                </For>
              </select>
              <ModelSelect models={models()} value={model()} onChange={setModel} testid="dock-model" />
              </div>
            </Show>
            <span class="spacer" />
            <Show when={current()}>
              <IconButton icon="plus" size="sm" label="New session" data-testid="dock-new" onClick={() => setDockId(null)} />
            </Show>
            <IconButton icon="panel-right" size="sm" label="Open in Agents" data-testid="dock-expand" onClick={() => { setOpen(false); setRoute({ view: "agents" }); }} />
            <IconButton icon="x" size="sm" label="Close" onClick={() => setOpen(false)} />
          </header>
          <Show
            when={current()}
            fallback={
              <div class="ag-dock-start">
                <Show when={here().length}>
                  <p class="ag-sec">Recent here</p>
                  <For each={here().slice(0, 3)}>
                    {(s) => (
                      <button type="button" class="ag-row" data-testid="dock-recent" data-state={s.state} onClick={() => { setDockId(s.id); void loadSession(s.id); }}>
                        <span class={`ag-dot ${s.state}`} aria-hidden="true" />
                        <span class="ag-row-main">
                          <span class="ag-row-title">{s.title}</span>
                          <span class="ag-row-sub muted">{s.harness_label}</span>
                        </span>
                      </button>
                    )}
                  </For>
                </Show>
                <div class="ag-starters">
                  <For each={STARTERS}>{(t) => <button type="button" class="ag-starter" onClick={() => setMessage(t)}>{t}</button>}</For>
                </div>
                <form
                  class="ag-composer compact"
                  onSubmit={(e) => {
                    e.preventDefault();
                    void start();
                  }}
                >
                  <textarea
                    rows="2"
                    aria-label="What should it do?"
                    data-testid="dock-message"
                    placeholder="What should it do?"
                    value={message()}
                    onInput={(e) => setMessage(e.currentTarget.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
                        e.preventDefault();
                        void start();
                      }
                    }}
                  />
                  <IconButton icon="send" variant="primary" label="Start" data-testid="dock-start" disabled={!message().trim() || !harness() || busy()} onClick={() => void start()} />
                </form>
                <label class="ag-dock-access small muted">
                  Access
                  <select aria-label="Access" data-testid="dock-access" value={access()} onChange={(e) => setAccess(e.currentTarget.value as SessionAccess)}>
                    <option value="gmclip" selected={access() === "gmclip"}>gmclip tools only</option>
                    <option value="full" selected={access() === "full"}>Full access (each tool asks first)</option>
                    <option value="bypass" selected={access() === "bypass"}>Full access, no asking</option>
                  </select>
                </label>
              </div>
            }
          >
            {(s) => (
              <>
                <SessionBody s={s()} compact />
                <div class="ag-dock-foot">
                  <Show when={(s().models?.length ?? 0) > 1 && isLive(s().state)}>
                    <ModelSelect models={s().models ?? []} value={s().model ?? ""} onChange={(m) => void attempt(() => sessionActions.model(s().id, m))} testid="dock-session-model" live />
                  </Show>
                  <label class="small muted" data-tip="Open each cut as the agent edits it">
                    <input type="checkbox" data-testid="dock-follow" checked={follow()} onChange={(e) => setFollow(e.currentTarget.checked)} /> Follow
                  </label>
                  <Show when={isLive(s().state)}>
                    <button type="button" class="ghost small" data-testid="dock-end" onClick={() => void attempt(() => sessionActions.stop(s().id))}>End</button>
                  </Show>
                  <Show when={canPickUp(s())}>
                    <button type="button" class="ghost small" data-testid="dock-resume" onClick={() => void attempt(() => sessionActions.resume(s().id)).then(() => loadSession(s().id))}>Pick up again</button>
                  </Show>
                </div>
                <Composer s={s()} compact />
              </>
            )}
          </Show>
        </section>
      </Show>
      <button
        type="button"
        class={`ag-fab${open() ? " open" : ""}${liveHere().some((s) => s.state === "working" || s.state === "starting") ? " working" : ""}${waitingHere() ? " waiting" : ""}`}
        aria-label={open() ? "Close the agent" : "Ask an agent"}
        aria-expanded={open()}
        data-tip={liveHere().length ? `${liveHere().length} agent session${liveHere().length === 1 ? "" : "s"} here` : "Ask an agent"}
        data-testid="agent-fab"
        onClick={() => setOpen(!open())}
      >
        <AgentFace state={current()?.state ?? (liveHere()[0]?.state ?? "none")} size={props.big ? 64 : 34} />
        <Show when={liveHere().length && !open()}>
          <span class={`ag-fab-badge${waitingHere() ? " waiting" : ""}`} data-testid="agent-fab-badge">{waitingHere() || liveHere().length}</span>
        </Show>
      </button>
    </div>
  );
}

/** The agent's model: "" is its own default; the list is what it offered. A live session
 *  shows only what it offers now. */
export function ModelSelect(props: { models: { id: string; name: string }[]; value: string; onChange: (m: string) => void; testid: string; live?: boolean }) {
  return (
    <select aria-label="Model" data-testid={props.testid} value={props.value} disabled={!props.live && !props.models.length} data-tip={!props.live && !props.models.length ? "Its models show once it has run here" : undefined} onChange={(e) => props.onChange(e.currentTarget.value)}>
      <Show when={!props.live}>
        <option value="">Default model</option>
      </Show>
      {/* (selected on the option: the select's value is set before its options render) */}
      <For each={props.models}>{(m) => <option value={m.id} selected={m.id === props.value}>{m.name}</option>}</For>
    </select>
  );
}

function onMountWatch() {
  void watchSessions();
}
