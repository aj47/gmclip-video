// History (U.14 / A2.13; SPEC §5.12): the project's versions — restore points (taken by
// themselves every 20 edits or 5 minutes), named snapshots (⌘⇧S) and the version before
// each agent run — newest first, with what this cut was at each and who edited after it
// (you, agent:<client>, a review link). Compare draws the cut then and now as two strips
// (parts added, removed, trimmed, moved) with pictures of the changed parts; Restore puts
// this cut back as one undoable step and says how long it took (R-HIST-5). Opened from the
// cut head ("↶ N earlier versions"), ⌘⇧S, or a ⌘Z with nothing left to undo.
//
// "Undo this agent" (decision 0050): a version taken before an agent's run undoes that run
// (`run_undo`: every field it wrote, back as it was, one undo step); when later edits
// touched the same things it offers the core's choices — undo only what nobody changed
// since, put it all back as it was before the agent, or leave it.
//
// Scrub through versions (lane K5): a slider across the cut's versions, oldest on the left
// and now on the right. Each step draws that version's strip (the compare strip, coloured
// against now) with its parts and length; Restore restores the one shown.
import { createEffect, createResource, createSignal, For, on, onMount, Show } from "solid-js";
import { actions, type HistoryDiff, type HistoryDiffPart, type HistoryVersion } from "../actions";
import { Button, Chip, IconButton, Sheet } from "../design/components";
import { attempt, CallError, cutView, say, undoAsk, setUndoAsk } from "../store";

const when = (ms: number) => new Date(ms).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
const secs = (x: number | null | undefined) => (x === null || x === undefined ? "?" : `${x.toFixed(1)} s`);

function title(v: HistoryVersion): string {
  if (v.kind === "named") return v.name ?? "named";
  if (v.kind === "run") return `before ${v.run_by ?? "an agent"}${v.run_label && v.run_label !== "implicit" ? ` (${v.run_label})` : ""}`;
  return "restore point";
}

/** A thumbnail sent as base64 RGB, drawn on a canvas (no markup from data). */
function Thumb(props: { t?: { w: number; h: number; rgb: string } }) {
  let c: HTMLCanvasElement | undefined;
  onMount(() => {
    const t = props.t;
    if (!c || !t) return;
    const bin = atob(t.rgb);
    const rgba = new Uint8ClampedArray(t.w * t.h * 4);
    for (let k = 0, j = 0; k < t.w * t.h && j + 2 < bin.length; k++, j += 3) {
      rgba[k * 4] = bin.charCodeAt(j);
      rgba[k * 4 + 1] = bin.charCodeAt(j + 1);
      rgba[k * 4 + 2] = bin.charCodeAt(j + 2);
      rgba[k * 4 + 3] = 255;
    }
    c.width = t.w;
    c.height = t.h;
    c.getContext("2d")?.putImageData(new ImageData(rgba, t.w, t.h), 0, 0);
  });
  return <Show when={props.t} fallback={<span class="hist-thumb empty" aria-hidden="true" />}><canvas ref={c} class="hist-thumb" aria-hidden="true" /></Show>;
}

/** One version of the cut as a strip of parts, sized by length and coloured by change. */
function Strip(props: { parts: HistoryDiffPart[]; total: number; mark: (p: HistoryDiffPart) => string; label: string }) {
  return (
    <div class="hist-strip" role="img" aria-label={props.label}>
      <For each={props.parts}>
        {(p) => <span class={`seg ${props.mark(p)}`} style={{ width: `${Math.max(0.4, (100 * p.dur_s) / Math.max(props.total, 0.001))}%` }} data-tip={`${secs(p.src_in_s)}–${secs(p.src_out_s)} of the source`} />}
      </For>
    </div>
  );
}

function Diff(props: { d: HistoryDiff }) {
  const d = () => props.d;
  const has = (list: HistoryDiffPart[], p: HistoryDiffPart) => list.some((q) => q.part === p.part);
  const trimmedTo = () => d().trimmed.map((t) => t.to);
  const total = () => Math.max(d().from_seconds ?? 0, d().to_seconds ?? 0);
  const markFrom = (p: HistoryDiffPart) => (has(d().removed, p) ? "removed" : d().trimmed.some((t) => t.from.part === p.part) ? "trimmed" : d().moved.includes(p.part) ? "moved" : "same");
  const markTo = (p: HistoryDiffPart) => (has(d().added, p) ? "added" : has(trimmedTo(), p) ? "trimmed" : d().moved.includes(p.part) ? "moved" : "same");
  const delta = () => (d().to_seconds ?? 0) - (d().from_seconds ?? 0);
  return (
    <div class="hist-diff" data-testid="history-diff">
      <div class="hist-sum small">
        <span>then {d().from_parts ?? 0} parts, {secs(d().from_seconds)}</span>
        <span>now {d().to_parts ?? 0} parts, {secs(d().to_seconds)} ({delta() >= 0 ? "+" : ""}{delta().toFixed(1)} s)</span>
      </div>
      <Strip parts={d().from} total={total()} mark={markFrom} label="The cut then" />
      <Strip parts={d().to} total={total()} mark={markTo} label="The cut now" />
      <div class="row hist-legend small">
        <Chip tone="success" dot>{d().added.length} added</Chip>
        <Chip tone="danger" dot>{d().removed.length} removed</Chip>
        <Chip tone="warning" dot>{d().trimmed.length} trimmed</Chip>
        <Chip tone="accent" dot>{d().moved.length} moved</Chip>
        <Show when={d().by.length}>
          <span class="muted">edited by {d().by.join(", ")}</span>
        </Show>
      </div>
      <ul class="hist-changes small">
        <For each={d().removed}>{(p) => <li class="removed"><Thumb t={p.thumb} />removed part {p.index + 1} · {secs(p.dur_s)}</li>}</For>
        <For each={d().added}>{(p) => <li class="added"><Thumb t={p.thumb} />added part {p.index + 1} · {secs(p.dur_s)}</li>}</For>
        <For each={d().trimmed}>
          {(t) => (
            <li class="trimmed">
              <Thumb t={t.to.thumb} />
              part {t.to.index + 1} trimmed {secs(t.from.dur_s)} → {secs(t.to.dur_s)}
            </li>
          )}
        </For>
      </ul>
    </div>
  );
}

/** The slider across the versions: oldest left, now right (the arrow keys step it). */
function VersionScrub(props: { versions: HistoryVersion[]; nowParts: number | null; nowSeconds: number | null; onRestore: (id: string) => void }) {
  // oldest first; versions from before the cut existed have nothing to show
  const steps = () => [...props.versions].filter((v) => v.parts !== null).reverse();
  const nowAt = () => steps().length;
  const [at, setAt] = createSignal(0);
  createEffect(on(nowAt, (n) => setAt(n), { defer: false }));
  const diffs = new Map<string, HistoryDiff>();
  const [diff, setDiff] = createSignal<HistoryDiff | null>(null);
  const [loading, setLoading] = createSignal(false);
  const shown = () => steps()[at()];
  // the diff of the version shown (cached): its own strip is the "then" side
  createEffect(on([at, steps], async () => {
    const v = cutView();
    const s = shown() ?? steps()[steps().length - 1];
    if (!v || !s) {
      setDiff(null);
      return;
    }
    const hit = diffs.get(s.id);
    if (hit) return void setDiff(hit);
    setLoading(true);
    const d = await attempt(() => actions.historyDiff(v.project, v.cut, s.id));
    setLoading(false);
    if (d) diffs.set(s.id, d);
    if ((shown() ?? steps()[steps().length - 1])?.id === s.id) setDiff(d ?? null);
  }));
  const isNow = () => at() >= nowAt();
  const parts = () => {
    const d = diff();
    if (!d) return [];
    return isNow() ? d.to : d.from;
  };
  // one scale for every step (the longest version), so a shorter cut draws shorter
  const total = () => Math.max(props.nowSeconds ?? 0, ...steps().map((v) => v.seconds ?? 0));
  const has = (list: HistoryDiffPart[], p: HistoryDiffPart) => list.some((q) => q.part === p.part);
  const mark = (p: HistoryDiffPart) => {
    const d = diff();
    if (!d || isNow()) return "same";
    return has(d.removed, p) ? "removed" : d.trimmed.some((t) => t.from.part === p.part) ? "trimmed" : d.moved.includes(p.part) ? "moved" : "same";
  };
  const nParts = () => (isNow() ? props.nowParts : shown()?.parts) ?? 0;
  const secsShown = () => (isNow() ? props.nowSeconds : shown()?.seconds) ?? null;
  const delta = () => (secsShown() ?? 0) - (props.nowSeconds ?? 0);
  const label = () => (isNow() ? "Now" : `${title(shown()!)} · ${when(shown()!.ts)}`);
  return (
    <section class="hist-scrub" aria-label="Scrub through versions" data-testid="history-scrub" data-at={at()} data-now={isNow() ? "1" : "0"}>
      <div class="hist-scrub-head small">
        <b data-testid="history-scrub-label">{label()}</b>
        <span class="muted" data-testid="history-scrub-length">
          {nParts()} part{nParts() === 1 ? "" : "s"} · {secs(secsShown())}
          <Show when={!isNow()}> ({delta() >= 0 ? "+" : ""}{delta().toFixed(1)} s against now)</Show>
        </span>
      </div>
      <input
        type="range"
        class="hist-slider"
        min="0"
        max={nowAt()}
        step="1"
        value={at()}
        aria-label="Version"
        aria-valuetext={label()}
        data-testid="history-slider"
        onInput={(e) => setAt(Number(e.currentTarget.value))}
      />
      <div class="hist-scrub-ends small muted" aria-hidden="true">
        <span>{steps()[0] ? when(steps()[0]!.ts) : ""}</span>
        <span>now</span>
      </div>
      <div class="hist-scrub-strip" data-loading={loading() ? "1" : "0"}>
        <Show when={diff()} fallback={<div class="hist-strip placeholder" aria-hidden="true" />}>
          <Strip parts={parts()} total={total()} mark={mark} label={`The cut at ${label()}`} />
        </Show>
      </div>
      <div class="row hist-scrub-actions">
        <Show when={!isNow()} fallback={<span class="muted small">Drag the slider (or use ← →) to see an earlier version.</span>}>
          <span class="muted small">Coloured against now: <span class="hist-key removed">gone since</span> <span class="hist-key trimmed">trimmed since</span> <span class="hist-key moved">moved since</span></span>
          <span class="spacer" />
          <Button size="sm" variant="primary" data-testid="history-scrub-restore" aria-label={`Restore this cut to ${label()}`} onClick={() => props.onRestore(shown()!.id)}>
            Restore this version
          </Button>
        </Show>
      </div>
    </section>
  );
}

export function History(props: { onClose: () => void; reason?: string; nameFirst?: boolean }) {
  const key = () => {
    const v = cutView();
    return v ? { p: v.project, c: v.cut, rev: v.rev } : undefined;
  };
  const [list, { refetch }] = createResource(key, async (k) => (await attempt(() => actions.historyList(k.p, k.c))) ?? null);
  const [name, setName] = createSignal("");
  const [open, setOpen] = createSignal<string | null>(null);
  const [diff, setDiff] = createSignal<HistoryDiff | null>(null);
  let nameInput: HTMLInputElement | undefined;
  onMount(() => {
    if (props.nameFirst) nameInput?.focus();
  });
  const what = (v: HistoryVersion) => {
    const l = list();
    if (v.parts === null) return "this cut did not exist yet";
    const now = l?.now_parts ?? null;
    const same = now === v.parts && Math.abs((l?.now_seconds ?? 0) - (v.seconds ?? 0)) < 0.05;
    return same ? `${v.parts} parts, ${secs(v.seconds)} (as now)` : `${v.parts} parts, ${secs(v.seconds)} → now ${now ?? "?"} parts, ${secs(l?.now_seconds ?? null)}`;
  };
  const compare = async (id: string) => {
    const v = cutView();
    if (!v) return;
    if (open() === id) {
      setOpen(null);
      return;
    }
    setOpen(id);
    setDiff(null);
    const d = await attempt(() => actions.historyDiff(v.project, v.cut, id));
    if (open() === id) setDiff(d ?? null);
  };
  const restore = async (id: string) => {
    const v = cutView();
    if (!v) return;
    const r = await attempt(() => actions.historyRestore(v.project, v.cut, id));
    if (r) {
      say(`This cut is back to that version in ${r.ms ?? "?"} ms — ⌘Z undoes it`);
      props.onClose();
    }
  };
  // a run's undo that met later edits: its choices, shown on that run's row
  const [undoChoice, setUndoChoice] = createSignal<{ run: string; later: string[]; options: string[] } | null>(null);
  const undoAgent = async (run: string, option?: "revert_untouched" | "restore_to_point") => {
    const v = cutView();
    if (!v) return;
    try {
      await actions.runUndo(v.project, run, option);
      setUndoChoice(null);
      say("That agent's edits are undone — ⌘Z puts them back");
      void refetch();
    } catch (e) {
      const d = e instanceof CallError && e.error.code === "Conflict" ? (e.error.detail as { diff?: { later_edits?: string[] }; options?: string[] } | undefined) : undefined;
      if (d) setUndoChoice({ run, later: d.diff?.later_edits ?? [], options: d.options ?? [] });
      else say(e instanceof CallError ? e.error.message : String(e), "error");
    }
  };
  const save = async () => {
    const v = cutView();
    const n = name().trim();
    if (!v || !n) return;
    if (await attempt(() => actions.snapshotNamed(v.project, n))) {
      say(`Saved “${n}”`);
      setName("");
      void refetch();
    }
  };
  return (
    <Sheet label="History" class="history" testid="history" onClose={props.onClose} sub="Every version of this cut: drag the slider to see one, restore it, or name this one.">
        <Show when={props.reason}>
          <p class="muted small" data-testid="history-reason">{props.reason}</p>
        </Show>
        <form class="row" onSubmit={(e) => { e.preventDefault(); void save(); }}>
          <input ref={nameInput} type="text" aria-label="Name this version" data-testid="snapshot-name" placeholder="name this version (⌘⇧S)" value={name()} onInput={(e) => setName(e.currentTarget.value)} />
          <Button type="submit" disabled={!name().trim()}>Save</Button>
        </form>
        <Show when={(list()?.versions ?? []).some((v) => v.parts !== null)}>
          <VersionScrub versions={list()?.versions ?? []} nowParts={list()?.now_parts ?? null} nowSeconds={list()?.now_seconds ?? null} onRestore={(id) => void restore(id)} />
        </Show>
        <ul class="history-list" aria-label="Versions">
          <For each={list()?.versions ?? []} fallback={<li class="muted small">No earlier versions yet.</li>}>
            {(v) => (
              <li class="small" data-testid="history-row" data-kind={v.kind}>
                <div class="row">
                  <span class="when">{when(v.ts)}</span>
                  <span class="what">
                    <b>{title(v)}</b> · {what(v)}
                    <Show when={v.then_by.length}>
                      <span class="muted" data-testid="history-by"> · then {v.then_by.join(", ")}</span>
                    </Show>
                  </span>
                  <Button variant="ghost" size="sm" aria-expanded={open() === v.id} aria-label={`Compare ${when(v.ts)} with now`} onClick={() => void compare(v.id)}>
                    Compare
                  </Button>
                  <Button variant="ghost" size="sm" aria-label={`Restore this cut to ${when(v.ts)}`} onClick={() => void restore(v.id)}>
                    Restore
                  </Button>
                  <Show when={v.kind === "run" && v.run}>
                    {(run) => (
                      <Button variant="ghost" size="sm" data-testid="history-undo-agent" aria-label={`Undo what ${v.run_by ?? "the agent"} did after ${when(v.ts)}`} onClick={() => void undoAgent(run())}>
                        Undo this agent
                      </Button>
                    )}
                  </Show>
                </div>
                <Show when={undoChoice()?.run === v.run && undoChoice()}>
                  {(c) => (
                    <div class="hist-undo-choice small" role="group" aria-label="Undo this agent" data-testid="history-undo-choice">
                      <p>Edits made after this agent changed some of the same things:</p>
                      <ul class="muted">
                        <For each={c().later}>{(l) => <li>{l}</li>}</For>
                      </ul>
                      <div class="row">
                        <Show when={c().options.includes("revert_untouched")}>
                          <Button size="sm" variant="primary" data-testid="history-undo-untouched" onClick={() => void undoAgent(c().run, "revert_untouched")}>Undo only what nobody changed since</Button>
                        </Show>
                        <Show when={c().options.includes("restore_to_point")}>
                          <Button size="sm" variant="ghost" data-testid="history-undo-all" onClick={() => void undoAgent(c().run, "restore_to_point")}>Put it all back as before the agent</Button>
                        </Show>
                        <Button size="sm" variant="ghost" onClick={() => setUndoChoice(null)}>Cancel</Button>
                      </div>
                    </div>
                  )}
                </Show>
                <Show when={open() === v.id}>
                  <Show when={diff()} fallback={<p class="muted small">Comparing…</p>}>{(d) => <Diff d={d()} />}</Show>
                </Show>
              </li>
            )}
          </For>
        </ul>
    </Sheet>
  );
}

/** R-HIST-1: the first ⌘Z after reopening a project asks once before undoing an edit
 * made in an earlier session. */
export function UndoAsk() {
  const go = async () => {
    const a = undoAsk();
    setUndoAsk(null);
    if (!a) return;
    await attempt(() => (a.redo ? actions.redo(a.project, true) : actions.undo(a.project, true)));
  };
  return (
    <Show when={undoAsk()}>
      {(a) => (
        <div class="keys-backdrop" onPointerDown={(e) => { if (e.target === e.currentTarget) setUndoAsk(null); }}>
          <section class="keys-dialog undo-ask" role="alertdialog" aria-modal="true" aria-label="Undo from an earlier session">
            <h2>{a().redo ? "Redo" : "Undo"} an edit from an earlier session?</h2>
            <p class="small">{a().command.replace(/_/g, " ")} · {when(a().ts)}</p>
            <div class="row">
              <Button variant="primary" data-testid="undo-ask-yes" onClick={() => void go()}>{a().redo ? "Redo it" : "Undo it"}</Button>
              <Button variant="ghost" onClick={() => setUndoAsk(null)}>Cancel</Button>
            </div>
          </section>
        </div>
      )}
    </Show>
  );
}
