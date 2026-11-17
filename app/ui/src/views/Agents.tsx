// The Agents workspace (decision 0035): every session of the user's own agents — live
// and earlier — and the open one's conversation: the agent's messages, its tool calls
// (gmclip's own marked), its plan, the questions it asks before running a tool, and a
// box for the next message. What an agent writes is untrusted and shown as text, never
// markup. Outside agents (connected over MCP from a terminal) and their approvals are in
// the activity rail at the bottom of the list.
import { createEffect, createResource, createSignal, For, Match, on, onMount, Show, Switch } from "solid-js";
import { sessionActions, type SessionEntry, type SessionView } from "../actions";
import { ActivityRail } from "../components/ActivityRail";
import { ConnectAgent } from "../components/AgentPanel";
import { Banner, Button, Chip, Empty, IconButton } from "../design/components";
import { Icon } from "../design/Icon";
import { ago } from "../eta";
import { canPickUp, entries, isLive, loadSession, openSession, sessions, setLaunchFor, watchSessions } from "../agentsState";
import { attempt, openCut, openProject, say, setRoute } from "../store";
import { Composer, SessionBody, statusLine } from "../components/SessionWork";
import "../components/agents.css";

const STATE_TEXT: Record<SessionView["state"], string> = {
  starting: "starting",
  working: "working",
  waiting: "needs you",
  idle: "your turn",
  ended: "ended",
  failed: "failed",
};

const STATE_TONE: Record<SessionView["state"], "neutral" | "accent" | "success" | "warning" | "danger"> = {
  starting: "neutral",
  working: "accent",
  waiting: "warning",
  idle: "success",
  ended: "neutral",
  failed: "danger",
};

export function SessionRow(props: { s: SessionView; on: boolean; onPick: () => void }) {
  return (
    <button type="button" class={`ag-row${props.on ? " on" : ""}`} data-testid="session-row" data-state={props.s.state} onClick={props.onPick}>
      <span class={`ag-dot ${props.s.state}`} aria-hidden="true" />
      <span class="ag-row-main">
        <span class="ag-row-title">{props.s.title}</span>
        <span class="ag-row-sub muted">
          {isLive(props.s.state) ? statusLine(props.s) : `${props.s.harness_label} · ${props.s.target.label}`}
        </span>
      </span>
      <span class="ag-row-when muted small">{props.s.state === "waiting" ? "needs you" : ago(props.s.last_ms)}</span>
    </button>
  );
}

export function Agents() {
  onMount(() => void watchSessions());
  const [connect, setConnect] = createSignal(false);
  const live = () => sessions().filter((s) => isLive(s.state));
  const earlier = () => sessions().filter((s) => !isLive(s.state));
  const current = () => sessions().find((s) => s.id === openSession()) ?? null;
  // the newest live session opens by itself when nothing is open
  createEffect(() => {
    const first = live()[0];
    if (!openSession() && first) void loadSession(first.id);
  });
  return (
    <div class="agents-view" data-testid="agents-view">
      <aside class="ag-list" aria-label="Sessions">
        <header class="ag-list-head">
          <h2>Agents</h2>
          <span class="spacer" />
          <Button variant="primary" size="sm" icon="plus" data-testid="session-new" onClick={() => setLaunchFor({})}>
            New
          </Button>
        </header>
        <Show when={live().length}>
          <h4 class="ag-sec">Running</h4>
          <For each={live()}>{(s) => <SessionRow s={s} on={s.id === openSession()} onPick={() => void loadSession(s.id)} />}</For>
        </Show>
        <Show when={earlier().length}>
          <h4 class="ag-sec">Earlier</h4>
          <For each={earlier()}>{(s) => <SessionRow s={s} on={s.id === openSession()} onPick={() => void loadSession(s.id)} />}</For>
        </Show>
        <details class="ag-outside">
          <summary class="ag-sec">Outside agents and approvals</summary>
          <p class="muted small">
            Agents you run yourself (in a terminal) reach gmclip over MCP.{" "}
            <button type="button" class="ghost small" onClick={() => setConnect(true)}>
              Connect one…
            </button>
          </p>
          <ActivityRail />
        </details>
      </aside>
      <main class="ag-main">
        <Show when={current()} keyed fallback={<Welcome />}>
          {(s) => <SessionPane s={s} />}
        </Show>
      </main>
      <Show when={connect()}>
        <ConnectAgent onClose={() => setConnect(false)} />
      </Show>
    </div>
  );
}

function Welcome() {
  const [agents] = createResource(async () => (await attempt(sessionActions.agents)) ?? null);
  return (
    <Empty
      icon="smile"
      title="Put your own agent to work"
      testid="agents-empty"
      action={
        <Button variant="primary" icon="plus" onClick={() => setLaunchFor({})}>
          New session
        </Button>
      }
    >
      <p>Claude Code, Codex, pi or opencode, on your own plan: pick a project, a video or a cut, say what you want, and watch it work. Every edit shows live and can be undone.</p>
      <Show when={agents()}>
        {(a) => (
          <p class="ag-avail">
            <For each={a().agents}>
              {(x) => (
                <Chip tone={x.available ? "success" : "neutral"} dot tooltip={x.available ? `via ${x.via}` : (x.problem ?? undefined)}>
                  {x.label}
                </Chip>
              )}
            </For>
          </p>
        )}
      </Show>
    </Empty>
  );
}

function SessionPane(props: { s: SessionView }) {
  const s = () => props.s;
  const goTarget = () => {
    const t = s().target;
    if (t.project && t.cut) void openCut(t.project, t.cut);
    else if (t.project) void openProject(t.project);
  };
  const undo = async () => {
    const r = await attempt(() => sessionActions.undo(s().id));
    if (r) say(`Undid this session's edits (${r.undone} run${r.undone === 1 ? "" : "s"})`);
  };
  return (
    <section class="ag-session" aria-label={`Session: ${s().title}`} data-testid="session-pane" data-state={s().state}>
      <header class="ag-head">
        <div class="ag-head-main">
          <h2 class="ag-title" title={s().title}>{s().title}</h2>
          <p class="ag-meta">
            <Chip tone={STATE_TONE[s().state]} dot testid="session-state">{STATE_TEXT[s().state]}</Chip>
            <Chip icon="smile" tooltip={[s().agent, s().via ? `via ${s().via}` : null].filter(Boolean).join(" · ") || undefined}>{s().harness_label}</Chip>
            <button type="button" class="ag-target" disabled={!s().target.project} onClick={goTarget} data-tip="Open it">
              <Icon name={s().target.kind === "cut" ? "scissors" : s().target.kind === "video" ? "video" : s().target.kind === "project" ? "folder-open" : "book"} size={13} />
              {s().target.label}
            </button>
            <Chip
              tone={s().access === "bypass" ? "danger" : s().access === "full" ? "warning" : "neutral"}
              tooltip={s().access === "bypass" ? "Its own tools and settings too, run without asking you; gmclip's tools may open files in your home folder" : s().access === "full" ? "Its own tools and settings too; each tool asks you first; gmclip's tools may open files in your home folder" : "gmclip's tools only"}
            >
              {s().access === "bypass" ? "full access · no asking" : s().access === "full" ? "full access" : "gmclip tools"}
            </Chip>
            <Show when={s().media}><Chip icon="video">sees footage</Chip></Show>
          </p>
        </div>
        <div class="ag-head-actions">
          <Show when={s().modes.length > 1 && isLive(s().state)}>
            <label class="ag-mode">
              <span class="muted small">Mode</span>
              <select
                aria-label="Agent mode"
                value={s().mode ?? ""}
                onChange={(e) => void attempt(() => sessionActions.mode(s().id, e.currentTarget.value))}
              >
                <For each={s().modes}>{(m) => <option value={m.id}>{m.name}</option>}</For>
              </select>
            </label>
          </Show>
          <Show when={(s().models?.length ?? 0) > 1 && isLive(s().state)}>
            <label class="ag-mode">
              <span class="muted small">Model</span>
              <select aria-label="Model" data-testid="session-model" value={s().model ?? ""} onChange={(e) => void attempt(() => sessionActions.model(s().id, e.currentTarget.value))}>
                <For each={s().models ?? []}>{(m) => <option value={m.id} selected={m.id === s().model}>{m.name}</option>}</For>
              </select>
            </label>
          </Show>
          <Show when={(s().core_runs?.length ?? 0) > 0}>
            <Button size="sm" variant="ghost" icon="restore" data-testid="session-undo" tooltip="Put back everything this session changed" onClick={() => void undo()}>
              Undo its edits
            </Button>
          </Show>
          <Switch>
            <Match when={isLive(s().state)}>
              <Button size="sm" variant="secondary" icon="x" data-testid="session-stop" tooltip="Ends the agent; the conversation stays here" onClick={() => void attempt(() => sessionActions.stop(s().id))}>
                End
              </Button>
            </Match>
            <Match when={!isLive(s().state)}>
              <Show when={canPickUp(s())}>
                <Button size="sm" variant="secondary" icon="repeat" data-testid="session-resume" tooltip="Opens it again where it left off (or say what next below)" onClick={() => void attempt(() => sessionActions.resume(s().id)).then(() => loadSession(s().id))}>
                  Pick up again
                </Button>
              </Show>
              <IconButton icon="x" label="Remove from the list" data-testid="session-forget" onClick={() => void attempt(() => sessionActions.forget(s().id))} />
            </Match>
          </Switch>
        </div>
      </header>
      <Show when={s().error}>
        <Banner tone="danger" role="alert" testid="session-error">{s().error!}</Banner>
      </Show>
      <SessionBody s={s()} />
      <Composer s={s()} />
    </section>
  );
}
