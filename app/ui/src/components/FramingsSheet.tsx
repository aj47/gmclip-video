// 9:16 and 16:9 together (lane K5): one action exports both framings of a cut, the vertical
// and the horizontal, as two exports with their own records. The export popover does it for
// the cut on screen (`BothFramings`); this sheet does it for several cuts of a project at
// once, like the agent batch sheet. A cut with only one framing is offered the other from the
// layout defaults first: it is made when the export starts, and the cut keeps showing its own.
import { createEffect, createResource, createSignal, For, on, Show } from "solid-js";
import { actions, type FramingExport, type FramingsView } from "../actions";
import { attempt, cutView, jobs, say } from "../store";
import { Icon } from "../design/Icon";
import { Button, Chip, Sheet } from "../design/components";

type Aspect = "9:16" | "16:9";
/** Which export job is which cut's which framing (the jobs list does not say). */
type Started = Record<string, { cut: string; aspect: string }>;

const stateWord = (f: FramingExport) => (f.export.state === "fresh" ? "exported" : f.export.state === "stale" ? "out of date" : "not exported");
const tone = (f: FramingExport): "success" | "warning" | "neutral" => (f.export.state === "fresh" ? "success" : f.export.state === "stale" ? "warning" : "neutral");

/** One cut's two framings as chips; a missing one says it will be made. */
export function FramingChips(props: { v: FramingsView; progress?: (aspect: string) => string | null }) {
  const all = (): ({ aspect: Aspect; f?: FramingExport })[] =>
    (["9:16", "16:9"] as Aspect[]).map((a) => ({ aspect: a, f: props.v.framings.find((x) => x.aspect === a) }));
  return (
    <span class="framing-chips">
      <For each={all()}>
        {(x) => (
          <Show
            when={x.f}
            fallback={
              <Chip tone="accent" icon={x.aspect === "9:16" ? "portrait" : "landscape"} testid="framing-missing" tooltip="Made from the layout defaults when the export starts; adjust it after in the Look tab">
                {x.aspect} · not made yet
              </Chip>
            }
          >
            {(f) => (
              <Chip tone={tone(f())} icon={x.aspect === "9:16" ? "portrait" : "landscape"} testid="framing-state" tooltip={`${f().name} (${f().w}×${f().h})`}>
                {x.aspect} · {props.progress?.(x.aspect) ?? stateWord(f())}
              </Chip>
            )}
          </Show>
        )}
      </For>
    </span>
  );
}

/** The progress line of a started export (null once it is done or gone). */
function progressOf(started: Started, cut: string, aspect: string): string | null {
  const id = Object.keys(started).find((k) => started[k]!.cut === cut && started[k]!.aspect === aspect);
  const j = id ? jobs().find((x) => x.id === id) : undefined;
  if (!j) return null;
  if (j.state === "queued") return "queued";
  if (j.state === "running") return `exporting ${Math.round((100 * j.done) / Math.max(1, j.frames))}%`;
  if (j.state === "failed") return "failed";
  if (j.state === "cancelled") return "cancelled";
  return null;
}

export function FramingsSheet(props: { project: { id: string; name: string; cuts: { id: string; title: string; framed: boolean }[] }; only?: string; onClose: () => void }) {
  const cuts = () => props.project.cuts.filter((c) => c.framed);
  const [picked, setPicked] = createSignal<string[]>(props.only ? [props.only] : []);
  const [started, setStarted] = createSignal<Started>({});
  const [busy, setBusy] = createSignal(false);
  // each cut's framings, read again as its exports finish
  const done = () => jobs().filter((j) => started()[j.id] && j.state === "done").length;
  const [views, { refetch }] = createResource(
    () => ({ n: cuts().length, d: done() }),
    async () => {
      const out: Record<string, FramingsView> = {};
      for (const c of cuts()) {
        const v = await actions.exportFramings(props.project.id, c.id).catch(() => null);
        if (v) out[c.id] = v;
      }
      return out;
    },
  );
  const usable = (id: string) => views()?.[id]?.supported !== false;
  const toggle = (id: string, onOff: boolean) => setPicked(onOff ? [...new Set([...picked(), id])] : picked().filter((x) => x !== id));
  const toMake = () => picked().reduce((n, id) => n + (views()?.[id]?.missing.length ?? 0), 0);
  const go = async () => {
    setBusy(true);
    let files = 0;
    for (const id of picked()) {
      if (!usable(id)) continue;
      const r = await attempt(() => actions.exportBoth(props.project.id, id, true));
      if (!r) continue;
      const add: Started = {};
      for (const j of r.jobs) add[j.job] = { cut: id, aspect: j.aspect };
      setStarted({ ...started(), ...add });
      files += r.jobs.length;
    }
    setBusy(false);
    if (files) say(`${files} exports queued: each cut in 9:16 and 16:9, one after another`);
    void refetch();
  };
  const n = () => picked().filter(usable).length;
  return (
    <Sheet
      label="Export 9:16 and 16:9"
      testid="framings-sheet"
      class="framings-sheet"
      onClose={props.onClose}
      sub="Each cut you tick is exported twice, vertical and horizontal, with a record for each. A cut with only one shape gets the other from the layout defaults first; it keeps showing its own."
    >
        <ul class="framings-cuts" aria-label="Cuts">
          <For each={cuts()} fallback={<li class="muted small">No framed cuts in {props.project.name} yet.</li>}>
            {(c) => (
              <li data-testid="framings-row" data-cut={c.id}>
                <label class="check">
                  <input type="checkbox" data-testid="framings-cut" disabled={!usable(c.id)} checked={picked().includes(c.id)} onChange={(e) => toggle(c.id, e.currentTarget.checked)} />
                  <span class="framings-title">{c.title}</span>
                </label>
                <Show when={views()?.[c.id]} fallback={<span class="muted small">{views.loading ? "Reading…" : "Cannot be read"}</span>}>
                  {(v) => (
                    <Show when={v().supported} fallback={<span class="muted small">{v().why_not}</span>}>
                      <FramingChips v={v()} progress={(a) => progressOf(started(), c.id, a)} />
                    </Show>
                  )}
                </Show>
              </li>
            )}
          </For>
        </ul>
        <div class="row framings-actions">
          <Button variant="primary" icon="share" data-testid="framings-start" disabled={!n() || busy()} onClick={() => void go()}>
            {n() ? `Export ${n()} cut${n() === 1 ? "" : "s"} · ${n() * 2} files` : "Tick the cuts to export"}
          </Button>
          <Show when={toMake()}>
            <span class="muted small" data-testid="framings-make-note">{toMake()} framing{toMake() === 1 ? "" : "s"} will be made from the layout defaults</span>
          </Show>
        </div>
    </Sheet>
  );
}

/** The export popover's row: this cut's two framings, and one button that exports both
 * (offering to make a missing one first). */
export function BothFramings(props: { project: string; cut: string; rev: number; open: boolean; onSeveral: () => void }) {
  const [v, { refetch }] = createResource(
    () => (props.open ? { p: props.project, c: props.cut, r: props.rev, n: jobs().filter((j) => j.cut === props.cut && j.state === "done").length } : undefined),
    async (k) => (await actions.exportFramings(k.p, k.c).catch(() => null)) ?? null,
  );
  const [asking, setAsking] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  createEffect(on(() => props.cut, () => setAsking(false)));
  const go = async (make: boolean) => {
    setBusy(true);
    const r = await attempt(() => actions.exportBoth(props.project, props.cut, make));
    setBusy(false);
    setAsking(false);
    if (r) {
      say(`${r.made.length ? `Made the ${r.made.join(" and ")} framing from the layout defaults; ` : ""}exporting 9:16, then 16:9`);
      void refetch();
    }
  };
  const other = () => (cutView()?.framing?.aspect === "16:9" ? "9:16" : "16:9");
  return (
    <div class="export-both" data-testid="export-both-box">
      <Show when={v()} fallback={<span class="hint">{v.loading ? "Reading this cut's shapes…" : "This cut's shapes cannot be read."}</span>}>
        {(fv) => (
          <Show when={fv().supported} fallback={<span class="hint">{fv().why_not}</span>}>
            <Show
              when={fv().missing.length && asking()}
              fallback={
                <button
                  type="button"
                  class="menu-item"
                  data-testid="export-both"
                  disabled={busy()}
                  data-tip={`This cut in both shapes, each export with its own record (the ${other()} made from the layout defaults if it has none)`}
                  onClick={() => (fv().missing.length ? setAsking(true) : void go(false))}
                >
                  <Icon name={other() === "16:9" ? "landscape" : "portrait"} size={14} />
                  Also export {other()}
                </button>
              }
            >
              <div class="export-both-ask" role="group" aria-label="Make the missing framing" data-testid="export-both-ask">
                <span class="hint">This cut has no {fv().missing.join(" or ")} yet: make it from the layout defaults (you can adjust it after) and export both?</span>
                <div class="row">
                  <Button size="sm" variant="primary" data-testid="export-both-make" disabled={busy()} onClick={() => void go(true)}>
                    Make {fv().missing[0]} and export both
                  </Button>
                  <Button size="sm" variant="ghost" onClick={() => setAsking(false)}>Not now</Button>
                </div>
              </div>
            </Show>
          </Show>
        )}
      </Show>
      <button type="button" class="menu-item" data-testid="framings-open" data-tip="Tick several cuts of this project and export each in both shapes" onClick={() => props.onSeveral()}>
        <Icon name="layers" size={14} />
        Several cuts…
      </button>
    </div>
  );
}
