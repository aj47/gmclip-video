// "Send an agent to these N cuts" (U.16 / E.01; SPEC §10.7.1): pick cuts, a pass, which
// agent (decision 0012), the budget and the wall clocks; the native sheet asks once (and
// names a budget over $5); the batch runs one cut at a time, skips a cut you edit
// meanwhile, and shows each cut's state here. AI passes within the budget are approved at
// launch, for this batch only.
import { createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { actions, type AgentClient, type ProjectInfo } from "../actions";
import { attempt, say } from "../store";
import { Button, IconButton } from "../design/components";

export function BatchSheet(props: { project: ProjectInfo; onClose: () => void }) {
  const cuts = () => props.project.cuts.filter((c) => c.framed);
  const [picked, setPicked] = createSignal<string[]>([]);
  const [pass, setPass] = createSignal<"edit_pass" | "grow_story">("edit_pass");
  const [budget, setBudget] = createSignal("0");
  const [hours, setHours] = createSignal("8");
  const [perCut, setPerCut] = createSignal("90");
  const [agent, setAgent] = createSignal<AgentClient["harness"] | "">("");
  const [clients] = createResource(async () => (await attempt(() => actions.agentClients())) ?? null);
  const usable = () => (clients()?.clients ?? []).filter((c) => c.installed && c.status !== "missing");
  const [tick, setTick] = createSignal(0);
  const timer = setInterval(() => setTick(tick() + 1), 2000);
  onCleanup(() => clearInterval(timer));
  const [list, { refetch }] = createResource(tick, async () => (await attempt(() => actions.batchList(props.project.id))) ?? null);
  const toggle = (id: string, on: boolean) => setPicked(on ? [...picked(), id] : picked().filter((x) => x !== id));
  const num = (s: string) => Number(s.replace(",", "."));
  const valid = () => {
    const b = num(budget());
    const h = num(hours());
    const m = num(perCut());
    return Number.isFinite(b) && b >= 0 && b <= 100 && h >= 1 / 6 && h <= 24 && m >= 5 && m <= 90 && m <= h * 60;
  };
  const start = async () => {
    const opts = { budget_usd: num(budget()), wall_clock_h: num(hours()), per_cut_min: num(perCut()), ...(agent() ? { harness: agent() as AgentClient["harness"] } : {}) };
    const r = await attempt(() => actions.batchStart(props.project.id, picked(), pass(), opts));
    if (r) {
      say(`Your agent is on ${picked().length} cut${picked().length === 1 ? "" : "s"}, one at a time`);
      setPicked([]);
      void refetch();
    }
  };
  const mins = (s: number) => (s >= 3600 ? `${Math.round((s / 3600) * 10) / 10} h` : `${Math.round(s / 60)} min`);
  return (
    <div class="keys-backdrop" onPointerDown={(e) => { if (e.target === e.currentTarget) props.onClose(); }}>
      <section class="keys-dialog batch" role="dialog" aria-modal="true" aria-label="Send an agent to cuts">
        <header>
          <h2>Send your agent to these cuts</h2>
          <IconButton icon="x" label="Close" onClick={() => props.onClose()} />
        </header>
        <ul class="batch-cuts">
          <For each={cuts()} fallback={<li class="muted small">No framed cuts in this project yet.</li>}>
            {(c) => (
              <li>
                <label class="check">
                  <input type="checkbox" data-testid="batch-cut" checked={picked().includes(c.id)} onChange={(e) => toggle(c.id, e.currentTarget.checked)} />
                  {c.title} <span class="muted small">{c.parts} parts</span>
                </label>
              </li>
            )}
          </For>
        </ul>
        <div class="batch-opts">
          <label class="field">
            Pass
            <select aria-label="Pass" value={pass()} onChange={(e) => setPass(e.currentTarget.value as "edit_pass" | "grow_story")}>
              <option value="edit_pass">Editing pass</option>
              <option value="grow_story">Grow the story</option>
            </select>
          </label>
          <label class="field" data-tip="Each cut runs with this agent, on its own login (decision 0012)">
            Agent
            <select aria-label="Agent" data-testid="batch-agent" value={agent()} onChange={(e) => setAgent(e.currentTarget.value as AgentClient["harness"] | "")}>
              <option value="">Default{clients()?.default ? ` (${clients()!.default})` : ""}</option>
              <For each={usable()}>{(c) => <option value={c.harness}>{c.label}</option>}</For>
            </select>
          </label>
          <label class="field" data-tip="Metered AI the batch may spend; $0 on your own plan. Over $5 the start sheet names it">
            Budget $
            <input class="num" type="number" min="0" max="100" step="0.5" aria-label="Budget in dollars" data-testid="batch-budget" value={budget()} onInput={(e) => setBudget(e.currentTarget.value)} />
          </label>
          <label class="field" data-tip="The whole batch stops after this">
            Wall clock h
            <input class="num" type="number" min="0.25" max="24" step="0.5" aria-label="Wall clock in hours" data-testid="batch-hours" value={hours()} onInput={(e) => setHours(e.currentTarget.value)} />
          </label>
          <label class="field" data-tip="One cut's run at most (90 minutes or less)">
            Per cut min
            <input class="num" type="number" min="5" max="90" step="5" aria-label="Minutes per cut" data-testid="batch-per-cut" value={perCut()} onInput={(e) => setPerCut(e.currentTarget.value)} />
          </label>
        </div>
        <p class="muted small" data-testid="batch-preapproval">
          Pre-approved at launch: AI passes within ${(num(budget()) || 0).toFixed(2)} for this batch. A cut you edit meanwhile is skipped; every run starts with a restore point.
        </p>
        <div class="row">
          <Button variant="primary" data-testid="batch-start" disabled={!picked().length || !valid()} onClick={() => void start()}>
            Send to {picked().length || "…"} cut{picked().length === 1 ? "" : "s"}
          </Button>
          <Show when={!valid()}>
            <span class="small warn" role="status">Budget $0–100, wall clock ¼–24 h, 5–90 min a cut</span>
          </Show>
        </div>
        <Show when={list()?.batches.length}>
          <h3>Batches</h3>
          <For each={list()!.batches}>
            {(b) => (
              <div class="batch-row" data-testid="batch-row">
                <p class="small">
                  <b>{b.pass === "edit_pass" ? "Editing pass" : "Grow the story"}</b> · {b.state === "stopped" ? "stopped: its wall clock ran out" : b.state}
                </p>
                <p class="muted small" data-testid="batch-limits">
                  {b.harness ?? "default agent"} · ${b.budget_usd.toFixed(2)} · {mins(b.wall_clock_s)} in all, {mins(b.per_cut_s)} a cut
                </p>
                <ul>
                  <For each={b.cuts}>
                    {(c) => (
                      <li class="small" data-testid="batch-cut" data-state={c.state}>
                        {c.title}: {c.state.replace("_", " ")}{c.reason ? ` — ${c.reason}` : ""}
                      </li>
                    )}
                  </For>
                </ul>
              </div>
            )}
          </For>
        </Show>
      </section>
    </div>
  );
}
