// The Agent tab (backlog U.16, G.02–G.04; SPEC §10.6, §10.7): "Run with my agent" for
// the edit pass and grow story on this cut, through the launcher only — a click here is
// the human gesture (R-AGT-3); nothing starts by itself. Which agent runs is picked per
// run ("Run with"), defaulting to the Owner's choice; every one is held to gmclip's
// tools. Run progress and results; what the agent wrote back is shown as text from the
// agent (untrusted, never markup). "Let the agent see and hear the footage" is an Owner
// grant: the native sheet asks first. A finished run can be continued with a note.
import { createResource, createSignal, For, Show } from "solid-js";
import { actions, type AgentClient, type AgentRun } from "../actions";
import { agentTick, attempt, cutView, say } from "../store";

const PASSES: { id: AgentRun["pass"]; name: string; what: string }[] = [
  { id: "edit_pass", name: "Edit pass", what: "tighten pauses and fillers, hook first, trims to word edges, 20–100 s" },
  { id: "grow_story", name: "Grow story", what: "grow the cut into a ~60 s payoff-first story from the stream" },
  { id: "find_moments", name: "Find moments", what: "read the transcript and add moments to the panel" },
  { id: "full_sweep", name: "Full sweep", what: "re-read the whole transcript for the moments finding them missed" },
];

/** The passes that add moments to the video (not to this cut). */
const MOMENT_PASSES: AgentRun["pass"][] = ["find_moments", "full_sweep"];

function ago(ms: number): string {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  return s < 60 ? `${s} s` : `${Math.floor(s / 60)} min`;
}

const STATUS_TEXT: Record<AgentClient["status"], string> = {
  ready: "ready",
  allowed: "allowed (this build)",
  check: "first run checks this build",
  blocked: "blocked",
  missing: "not found",
};

/** The user's agents (G.02): found by path (never a shell alias), version, whether a
 *  run may start on this build, which one is the default (an Owner setting). */
export function AgentPicker(props: { onChange?: () => void }) {
  const [list, { refetch }] = createResource(
    () => agentTick(),
    async () => (await attempt(() => actions.agentClients(false))) ?? null,
  );
  const [editing, setEditing] = createSignal<AgentClient["harness"] | null>(null);
  const [path, setPath] = createSignal("");
  const changed = () => {
    void refetch();
    props.onChange?.();
  };
  const lookAgain = async () => {
    await attempt(() => actions.agentClients(true));
    changed();
  };
  const makeDefault = async (h: AgentClient["harness"]) => {
    if (await attempt(() => actions.agentDefaultSet(h))) say("Default agent set");
    changed();
  };
  const setClientPath = async (h: AgentClient["harness"]) => {
    if (await attempt(() => actions.agentClientSet(path().trim(), h))) {
      setEditing(null);
      setPath("");
    }
    changed();
  };
  return (
    <div class="agent-picker" data-testid="agent-picker">
      <div class="row">
        <h3>Your agents</h3>
        <span class="spacer" />
        <button type="button" class="ghost" onClick={() => void lookAgain()}>Look again</button>
      </div>
      <Show when={list()} fallback={<p class="muted small">Looking for your agents…</p>}>
        {(l) => (
          <For each={l().clients}>
            {(c) => (
              <div class={`agent-client ${c.status}`} data-testid="agent-client" data-harness={c.harness} data-status={c.status}>
                <p class="row small">
                  <b>{c.label}</b>
                  <Show when={c.version}><span class="muted">{c.version}</span></Show>
                  <span class="tag">{STATUS_TEXT[c.status]}</span>
                  <Show when={l().default === c.harness}><span class="tag" data-testid="agent-default">default</span></Show>
                  <span class="spacer" />
                  <Show when={l().default !== c.harness && c.installed}>
                    <button type="button" class="ghost" onClick={() => void makeDefault(c.harness)}>Make default…</button>
                  </Show>
                  <button type="button" class="ghost" onClick={() => setEditing(editing() === c.harness ? null : c.harness)}>Set path…</button>
                </p>
                <Show when={c.found}><p class="muted small file">{c.found}{c.set_by_owner ? " (set by you)" : ""}</p></Show>
                <Show when={c.reason}><p class="muted small">{c.reason}</p></Show>
                <Show when={c.installed}>
                  <p class="muted small">Runs on {c.login}; {c.how}.</p>
                </Show>
                <Show when={editing() === c.harness}>
                  <form class="row" onSubmit={(e) => { e.preventDefault(); void setClientPath(c.harness); }}>
                    <input type="text" aria-label={`${c.label} path`} placeholder={`/absolute/path/to/${c.harness === "claude-code" ? "claude" : c.harness}`} value={path()} onInput={(e) => setPath(e.currentTarget.value)} />
                    <button type="submit" disabled={!path().trim()}>Use it…</button>
                  </form>
                </Show>
              </div>
            )}
          </For>
        )}
      </Show>
    </div>
  );
}

export function AgentPanel() {
  const [status, { refetch }] = createResource(
    () => ({ p: cutView()?.project, t: agentTick() }),
    async (k) => (k.p ? ((await attempt(() => actions.agentStatus(k.p))) ?? null) : null),
  );
  const [pass, setPass] = createSignal<AgentRun["pass"]>("edit_pass");
  const [note, setNote] = createSignal("");
  const [media, setMedia] = createSignal(false);
  const [clientPath, setClientPath] = createSignal("");
  const [runWith, setRunWith] = createSignal<AgentClient["harness"] | "">("");
  const [continuing, setContinuing] = createSignal<string | null>(null);
  const [contNote, setContNote] = createSignal("");
  const runs = () =>
    [...(status()?.runs ?? [])].filter((r) => r.cut === cutView()?.cut || MOMENT_PASSES.includes(r.pass)).sort((a, b) => b.started_ms - a.started_ms);
  const busy = () => runs().some((r) => r.state === "starting" || r.state === "checking" || r.state === "running");
  const harness = () => runWith() || status()?.default || "claude-code";
  const usable = () => (status()?.clients ?? []).filter((c) => c.installed);
  const chosen = () => (status()?.clients ?? []).find((c) => c.harness === harness());
  const runAnyway = async (r: AgentRun) => {
    if (!r.blocked) return;
    if (await attempt(() => actions.agentClientAllow(r.blocked!.sha256))) await run(r.pass, note(), r.harness as AgentClient["harness"]);
  };
  const run = async (p: AgentRun["pass"], brief: string, h: AgentClient["harness"] = harness()) => {
    const v = cutView();
    if (!v) return;
    const r = await attempt(() => actions.agentRun(v.project, v.cut, p, brief, media(), h));
    if (r) say("Your agent is on it — it edits this cut live; ⌘Z undoes its run");
    void refetch();
  };
  const cont = async (r: AgentRun) => {
    const text = contNote().trim();
    if (!text) return;
    const n = await attempt(() => actions.agentContinue(r.id, text));
    if (n) {
      say(r.resumes ? "Your agent picks up where it left off" : "A new run starts with your note and the last summary");
      setContinuing(null);
      setContNote("");
    }
    void refetch();
  };
  const passName = (p: string) => PASSES.find((x) => x.id === p)?.name ?? p;
  const found = () => chosen()?.installed ?? status()?.client.found ?? false;

  return (
    <section class="agent-panel" aria-label="Agent">
      <Show when={status() && !status()!.holder}>
        <p class="note" role="status">{status()!.note ?? "No agent server in this app."}</p>
      </Show>
      <Show
        when={status()?.client.found}
        fallback={
          <Show when={status()}>
            <div class="note" role="alert" data-testid="client-missing">
              <p>{status()!.client.message}</p>
              <form class="row" onSubmit={(e) => { e.preventDefault(); void attempt(() => actions.agentClientSet(clientPath().trim(), status()!.default)).then(() => refetch()); }}>
                <input type="text" aria-label="Agent path" placeholder="/Users/you/.nvm/versions/node/v24.1.0/bin/claude" value={clientPath()} onInput={(e) => setClientPath(e.currentTarget.value)} />
                <button type="submit" disabled={!clientPath().trim()}>Set path…</button>
              </form>
            </div>
          </Show>
        }
      >
        {/* the client the next run uses: the one picked under "Run with", else the default */}
        <p class="muted small" data-testid="client-path">
          {chosen()?.found
            ? `${chosen()!.label}: ${chosen()!.found}${chosen()!.set_by_owner ? " (set by you)" : ""}`
            : `${status()!.client.label ?? "Claude Code"}: ${status()!.client.path}${status()!.client.set_by_owner ? " (set by you)" : ""}`}
        </p>
      </Show>
      <div>
        <h3>Run with my agent</h3>
        <div class="col">
          <For each={PASSES}>
            {(p) => (
              <label class="radio">
                <input type="radio" name="pass" checked={pass() === p.id} onChange={() => setPass(p.id)} /> <b>{p.name}</b>
                <span class="muted small"> — {p.what}</span>
              </label>
            )}
          </For>
          <textarea rows="3" aria-label="Note for the agent" placeholder="Anything to add (optional): the angle, what to keep, what to cut." value={note()} onInput={(e) => setNote(e.currentTarget.value)} />
          <label class="check">
            <input type="checkbox" checked={media()} onChange={(e) => setMedia(e.currentTarget.checked)} /> Let the agent see and hear the footage (asks you first)
          </label>
          <div class="row">
            <label class="small">
              Run with:{" "}
              <select aria-label="Run with" data-testid="run-with" value={harness()} onChange={(e) => setRunWith(e.currentTarget.value as AgentClient["harness"])}>
                <For each={usable().length ? usable() : (status()?.clients ?? [])}>
                  {(c) => <option value={c.harness}>{c.label}{status()?.default === c.harness ? " (default)" : ""}</option>}
                </For>
              </select>
            </label>
            <button type="button" class="primary" data-testid="agent-run-button" disabled={!status()?.holder || !found() || busy()} onClick={() => void run(pass(), note())}>
              {busy() ? "Your agent is working…" : `✨ Run ${passName(pass())}`}
            </button>
          </div>
          <Show when={chosen()?.reason && chosen()!.status !== "ready"}>
            <p class="muted small" data-testid="run-with-note">{chosen()!.label}: {chosen()!.reason}</p>
          </Show>
        </div>
      </div>
      <details class="agent-picker-wrap">
        <summary class="small">Your agents</summary>
        <AgentPicker onChange={() => void refetch()} />
      </details>
      <div>
        <h3>Runs</h3>
        <For each={runs()} fallback={<p class="muted small">No runs yet. Nothing runs until you press a button.</p>}>
          {(r) => (
            <div class={`agent-run-row ${r.state}`} data-testid="agent-run-row" data-state={r.state}>
              <div class="row">
                <b>{passName(r.pass)}</b>
                <span class="tag">{r.state}</span>
                <span class="muted small">{r.harness_label ?? r.harness} · {ago(r.started_ms)} ago{r.tool_calls !== null ? ` · ${r.tool_calls} tool calls` : ""}{r.continued_from ? (r.resumed ? " · continued (same session)" : " · continued (fresh, with the last summary)") : ""}</span>
                <Show when={r.state === "done" || r.state === "failed"}>
                  <button type="button" class="ghost" disabled={busy() || !status()?.holder} onClick={() => void run(r.pass, note(), r.harness as AgentClient["harness"])}>Run again</button>
                  <button type="button" class="ghost" data-testid="continue-open" disabled={busy() || !status()?.holder} onClick={() => { setContinuing(continuing() === r.id ? null : r.id); setContNote(""); }}>Continue with a note…</button>
                </Show>
              </div>
              <Show when={(r.state === "running" || r.state === "starting") && r.looking_at}>
                <p class="small" role="status" data-testid="looking-at">Looking at: {r.looking_at!.label}</p>
              </Show>
              <Show when={r.found !== null && MOMENT_PASSES.includes(r.pass)}>
                <p class="small">Found {r.found} moment{r.found === 1 ? "" : "s"} by your agent — recall not measured.</p>
              </Show>
              <Show when={r.error}>
                <p class="small err">{r.error}</p>
              </Show>
              <Show when={r.blocked}>
                <ul class="small" data-testid="failed-checks">
                  <For each={r.blocked!.failed}>{(c) => <li><b>{c.id}</b> {c.what}</li>}</For>
                </ul>
                <button type="button" data-testid="run-anyway" disabled={busy()} onClick={() => void runAnyway(r)}>Run anyway…</button>
              </Show>
              <Show when={r.state === "checking" && r.summary}>
                <p class="small" role="status" data-testid="client-checking">{r.summary}</p>
              </Show>
              <Show when={r.state !== "checking" && r.summary}>
                <div class="from-agent" aria-label="From your agent (untrusted)">
                  <span class="muted small">From your agent:</span>
                  <p>{r.summary}</p>
                </div>
              </Show>
              <Show when={continuing() === r.id}>
                <form class="col" data-testid="continue-form" onSubmit={(e) => { e.preventDefault(); void cont(r); }}>
                  <textarea rows="2" aria-label="Note to continue with" placeholder="What next? e.g. now make it 45 s, keep the reaction." value={contNote()} onInput={(e) => setContNote(e.currentTarget.value)} />
                  <p class="muted small">
                    {r.resumes
                      ? `${r.harness_label ?? r.harness} picks up its own session where it stopped. A new run, with its own undo.`
                      : `${r.harness_label ?? r.harness} starts a new run with your note and the last run's summary. A new run, with its own undo.`}
                  </p>
                  <div class="row">
                    <button type="submit" class="primary" data-testid="continue-run" disabled={!contNote().trim() || busy()}>Continue</button>
                  </div>
                </form>
              </Show>
            </div>
          )}
        </For>
      </div>
    </section>
  );
}

/** The MCP connect lines (U.16): what to paste into Claude Code or Codex so it can reach
 *  this app. Connecting an outside client is the user's choice in that client. */
export function ConnectAgent(props: { onClose: () => void }) {
  const [status] = createResource(async () => (await attempt(() => actions.agentStatus())) ?? null);
  const copy = (t: string) =>
    void navigator.clipboard.writeText(t).then(
      () => say("Copied"),
      () => say("Select the line and copy it", "error"),
    );
  return (
    <div class="keys-backdrop" onPointerDown={(e) => { if (e.target === e.currentTarget) props.onClose(); }}>
      <section class="keys-dialog connect" role="dialog" aria-modal="true" aria-label="Connect an agent">
        <header>
          <h2>Connect an agent</h2>
          <button type="button" class="ghost" onClick={() => props.onClose()} aria-label="Close">✕</button>
        </header>
        <p>Paste one of these in a terminal. Your agent then reaches the projects open in this app through gmclip's tools; it runs with that agent's own permissions and other tools.</p>
        <Show when={status()?.connect} fallback={<p class="muted">Loading…</p>}>
          {(c) => (
            <dl class="connect-lines">
              <dt>Claude Code</dt>
              <dd><code data-testid="connect-claude">{c().claude_code}</code> <button type="button" class="ghost" onClick={() => copy(c().claude_code)}>Copy</button></dd>
              <dt>Codex</dt>
              <dd><code data-testid="connect-codex">{c().codex}</code> <button type="button" class="ghost" onClick={() => copy(c().codex)}>Copy</button></dd>
            </dl>
          )}
        </Show>
        <Show when={status() && !status()!.holder}>
          <p class="note">{status()!.note ?? "No agent server in this app."}</p>
        </Show>
      </section>
    </div>
  );
}

/** A typed folder with `~` or `~/…` (`~\…` on Windows) expanded to the home folder;
 *  anything else as typed. */
export function expandHome(typed: string, home: string | null | undefined): string {
  const p = typed.trim();
  if (!home || !(p === "~" || p.startsWith("~/") || p.startsWith("~\\"))) return p;
  const sep = home.includes("\\") && !home.includes("/") ? "\\" : "/";
  const base = home.replace(/[\\/]+$/, "");
  return p === "~" ? base : `${base}${sep}${p.slice(2).replace(/^[\\/]+/, "")}`;
}

/** Agent access (Owner, U.11): the folders agents may use and blocked clients. Every
 *  change asks on the native sheet, which says in plain words what it will do. Agents
 *  read the home folder by default (decision 0058); removing it narrows them to the
 *  folders listed. */
export function AgentAccess(props: { onClose: () => void }) {
  const [st, { refetch }] = createResource(async () => (await attempt(actions.ownerSettings)) ?? null);
  const [path, setPath] = createSignal("");
  const resolved = () => expandHome(path(), st()?.home);
  // a write root is read too (decision 0058): one list, write roots tagged
  const roots = () => [...new Set([...(st()?.read_roots ?? []), ...(st()?.write_roots ?? [])])];
  const [write, setWrite] = createSignal(false);
  const [ident, setIdent] = createSignal("");
  const ask = async (command: string, args: Record<string, unknown>) => {
    const r = await attempt(() => actions.ownerRequest(command, args));
    if (r !== undefined) say("Done");
    void refetch();
  };
  return (
    <div class="keys-backdrop" onPointerDown={(e) => { if (e.target === e.currentTarget) props.onClose(); }}>
      <section class="keys-dialog connect" role="dialog" aria-modal="true" aria-label="Agent access">
        <header>
          <h2>Agent access</h2>
          <button type="button" class="ghost" onClick={() => props.onClose()} aria-label="Close">✕</button>
        </header>
        <p class="muted small">Each change asks you in a system dialog first.</p>
        <h3>Folders agents may read</h3>
        <Show when={st()?.home && st()?.home_readable}>
          <div class="row" data-testid="home-root">
            <code>{st()!.home}</code>
            <span class="tag">your home folder</span>
            <button type="button" class="ghost" onClick={() => void ask("roots_remove", { path: st()!.home })}>Remove</button>
          </div>
        </Show>
        <Show when={st()?.home && st()?.home_readable === false}>
          <p class="row small" data-testid="home-off">
            <span class="muted">Agents read only the folders listed here, not your whole home folder.</span>
            <button type="button" class="ghost" onClick={() => void ask("roots_add", { path: st()!.home })}>Let agents read {st()!.home}…</button>
          </p>
        </Show>
        <For each={roots()} fallback={<Show when={!st()?.home_readable}><p class="muted small">None besides your projects.</p></Show>}>
          {(r) => (
            <div class="row" data-testid="agent-root">
              <code>{r}</code>
              <Show when={(st()?.write_roots ?? []).includes(r)}><span class="tag">read and write</span></Show>
              <button type="button" class="ghost" onClick={() => void ask("roots_remove", { path: r })}>Remove</button>
            </div>
          )}
        </For>
        <form class="row" onSubmit={(e) => { e.preventDefault(); void ask("roots_add", { path: resolved(), write: write() }); }}>
          <input type="text" aria-label="Folder for agents" placeholder="~/Movies/Recordings" value={path()} onInput={(e) => setPath(e.currentTarget.value)} />
          <label class="check"><input type="checkbox" checked={write()} onChange={(e) => setWrite(e.currentTarget.checked)} /> and write</label>
          <button type="submit" disabled={!resolved()}>Add folder…</button>
        </form>
        <Show when={resolved()}>
          <p class="muted small" data-testid="folder-resolved">Folder: <code>{resolved()}</code></p>
        </Show>
        <h3>Blocked clients</h3>
        <For each={st()?.revoked ?? []} fallback={<p class="muted small">None.</p>}>{(r) => <p><code>{r}</code></p>}</For>
        <form class="row" onSubmit={(e) => { e.preventDefault(); void ask("revoke", { identity: ident().trim() }); }}>
          <input type="text" aria-label="Client to block" placeholder="local:claude-code" value={ident()} onInput={(e) => setIdent(e.currentTarget.value)} />
          <button type="submit" disabled={!ident().trim()}>Block…</button>
        </form>
      </section>
    </div>
  );
}
