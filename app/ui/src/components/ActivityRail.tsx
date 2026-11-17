// The activity rail (backlog U.10, G.04): which agent clients have connected, which runs
// are live and what each launched agent is looking at right now (its latest see / listen
// / transcript_read, read from the run's own tool-call log), Revoke a client and Pause /
// Resume every agent. An agent's ask (R-PROTO-6) is approved and exported as you, or
// dismissed; a run that kept asking is let go on (decision 0050). Revoke and Pause are Owner changes: each asks in a native system
// dialog.
import { baseName } from "../platform";
import { createResource, For, Show } from "solid-js";
import { actions } from "../actions";
import { agentTick, attempt, say, setRoute } from "../store";

import { ago } from "../eta";

/** The server-computed effects as lines of text (never the agent's own words). */
function effectLines(e: Record<string, unknown>): string[] {
  const out: string[] = [];
  const blocked = e.blocked_by;
  if (Array.isArray(blocked)) {
    for (const b of blocked) out.push(`blocked by ${(b as { rule?: string }).rule ?? "a check"}: ${(b as { message?: string }).message ?? ""}`);
  }
  const ex = e.export as { command?: string; args?: Record<string, unknown> } | undefined;
  if (ex?.command) out.push(`would run: ${ex.command} ${JSON.stringify(ex.args ?? {})}`.slice(0, 300));
  for (const [k, v] of Object.entries(e)) {
    if (k === "blocked_by" || k === "export" || k === "cut_rev") continue;
    out.push(`${k}: ${typeof v === "string" ? v : JSON.stringify(v)}`.slice(0, 300));
  }
  return out;
}

export function ActivityRail() {
  const [a, { refetch }] = createResource(
    () => agentTick(),
    async () => (await attempt(actions.activity)) ?? null,
  );
  // the app's launched runs (every project), for "looking at"
  const [mine] = createResource(
    () => agentTick(),
    async () => (await attempt(() => actions.agentStatus())) ?? null,
  );
  const live = () => (mine()?.runs ?? []).filter((r) => r.state === "running" || r.state === "starting" || r.state === "checking");
  const pause = async (on: boolean) => {
    if (await attempt(() => actions.agentsPause(on))) say(on ? "Agents paused" : "Agents resumed");
    void refetch();
  };
  const revoke = async (identity: string) => {
    if (await attempt(() => actions.ownerRequest("revoke", { identity }))) say("Blocked");
    void refetch();
  };
  const resolve = async (id: string | null, run: string | null) => {
    if (await attempt(() => actions.approvalResolve(id, run))) say(run ? "The run may continue" : "Dismissed");
    void refetch();
  };
  // decision 0050: the human exports what the agent was refused (a human export is allowed)
  const approve = async (id: string) => {
    if (await attempt(() => actions.approvalExport(id))) say("Exporting — the agent can wait on it");
    void refetch();
  };
  const ack = async (ts: number) => {
    await attempt(() => actions.firstSeenAck(ts));
    void refetch();
  };
  const stop = async (run: string) => {
    if (await attempt(() => actions.agentRunStop(run))) say("Run stopped");
    void refetch();
  };
  return (
    <aside class="activity-rail" aria-label="Agent activity">
      <header class="row">
        <h3>Agents</h3>
        <span class="spacer" />
        <button type="button" class="ghost" aria-label="Refresh activity" onClick={() => void refetch()}>↻</button>
      </header>
      <Show when={a()?.holder} fallback={<p class="muted small">No agent server in this app.</p>}>
        <Show
          when={a()!.paused}
          fallback={
            <button type="button" data-testid="agents-pause" onClick={() => void pause(true)}>Pause agents…</button>
          }
        >
          <p class="note" role="status" data-testid="agents-paused">Agents are paused: their gmclip tools are refused.</p>
          <button type="button" data-testid="agents-resume" onClick={() => void pause(false)}>Resume agents…</button>
        </Show>
        <For each={a()!.first_seen ?? []}>
          {(n) => (
            <div class="approval first-seen" role="status" data-testid="first-seen" data-identity={n.identity}>
              <p class="small"><b>A new program connected to gmclip</b></p>
              <p class="small"><code>{n.identity}</code></p>
              <Show when={!n.exe.startsWith("identity:")}>
                <p class="muted small file">{n.exe}</p>
              </Show>
              <p class="muted small">Any program running as you can use gmclip the way your agent can. If you don’t know this one, revoke it.</p>
              <div class="row">
                <Show when={n.identity.startsWith("local:") && !n.revoked}>
                  <button type="button" class="ghost" onClick={() => void revoke(n.identity)}>Revoke…</button>
                </Show>
                <button type="button" class="ghost" data-testid="first-seen-ok" onClick={() => void ack(n.ts)}>OK</button>
              </div>
            </div>
          )}
        </For>
        <Show when={(a()!.approvals ?? []).length}>
          <h4>Needs your approval</h4>
          <For each={a()!.approvals ?? []}>
            {(ap) => (
              <div class="approval" data-testid="approval">
                <Show when={ap.run_paused}>
                  <p class="note" role="status">This agent keeps asking — review run</p>
                </Show>
                <p class="small"><code>{ap.identity}</code> asked to <b>{ap.tool}</b></p>
                <p class="small">What it would do (worked out by gmclip):</p>
                <ul class="small effects">
                  <For each={effectLines(ap.effects)}>{(l) => <li>{l}</li>}</For>
                </ul>
                <Show when={ap.prose}>
                  <p class="small untrusted">The agent says (unchecked): “{ap.prose}”</p>
                </Show>
                <div class="row">
                  <Show when={ap.exportable}>
                    <button type="button" data-testid="approval-export" onClick={() => void approve(ap.id)}>Approve and export</button>
                  </Show>
                  <button type="button" class="ghost" data-testid="approval-dismiss" onClick={() => void resolve(ap.id, null)}>Dismiss</button>
                  <Show when={ap.run_paused}>
                    <button type="button" class="ghost" onClick={() => void resolve(null, ap.run)}>Let it continue</button>
                  </Show>
                </div>
              </div>
            )}
          </For>
        </Show>
        <Show when={live().length}>
          <h4>Your agent</h4>
          <For each={live()}>
            {(r) => (
              <div class="small" data-testid="rail-looking" data-run={r.id}>
                <p class="row"><b>{r.harness_label ?? r.harness}</b><span class="muted">{r.pass.replace("_", " ")} · {r.state}</span></p>
                <p class="muted">{r.looking_at ? `Looking at: ${r.looking_at.label}` : "Starting…"}</p>
              </div>
            )}
          </For>
        </Show>
        <h4>Running now</h4>
        <For each={a()!.runs ?? []} fallback={<p class="muted small">Nothing running.</p>}>
          {(r) => (
            <p class="row small">
              <span>Run on a cut</span>
              <span class="spacer" />
              <button type="button" class="ghost" onClick={() => void stop(r.run)}>Stop</button>
            </p>
          )}
        </For>
        <h4>Connected clients</h4>
        <For each={a()!.clients ?? []} fallback={<p class="muted small" data-testid="no-clients">No agent has connected yet. <button type="button" class="ghost small" onClick={() => setRoute({ view: "settings" })}>Your agents…</button></p>}>
          {(c) => (
            <div class="client-row" data-identity={c.identity}>
              <p class="small">
                <code>{c.identity}</code> <span class="muted">{ago(c.last_ms)}</span>
              </p>
              <Show when={c.client}>
                <p class="muted small file">{baseName(c.client!)}</p>
              </Show>
              <Show
                when={!c.revoked}
                fallback={<p class="muted small">Blocked</p>}
              >
                <Show when={c.identity.startsWith("local:")}>
                  <button type="button" class="ghost" onClick={() => void revoke(c.identity)}>Revoke…</button>
                </Show>
              </Show>
            </div>
          )}
        </For>
      </Show>
    </aside>
  );
}
