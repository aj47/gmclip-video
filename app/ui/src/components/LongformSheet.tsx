// "Make long-form from topics…" (decision 0011): tick topics, order them, and a 16:9
// long-form is made from what the stream edit keeps inside them, one chapter per topic.
// A project of several videos lists every video's topics (a long-form may mix them).
import { createSignal, For, Show } from "solid-js";
import { actions, type MediaRef } from "../actions";
import { Button, Sheet } from "../design/components";
import { attempt, cutView, openCut, say } from "../store";
import { projectMapData, refreshMap, setLongformOpen } from "../streamState";

function clock(ms: number): string {
  const t = Math.round(ms / 1000);
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, "0")}`;
}

export function LongformSheet() {
  const streams = () => projectMapData()?.streams ?? (projectMapData()?.stream ? [projectMapData()!.stream!] : []);
  // each topic's video by name: the main cut holds every video (decision 0036)
  const videoName = (m: MediaRef) => {
    const key = "group" in m ? m.group : m.source;
    return (projectMapData()?.sources ?? []).find((x) => x.id === key || x.group === key)?.name ?? "";
  };
  const accepted = () => streams().flatMap((s) => s.topics.filter((t) => t.state === "accepted").map((t) => ({ ...t, video: videoName(t.media) })));
  const several = () => new Set(accepted().map((t) => t.video)).size > 1;
  const [picked, setPicked] = createSignal<string[]>([]);
  const [order, setOrder] = createSignal<"source" | "given">("source");
  const [title, setTitle] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const toggle = (id: string) => setPicked((p) => (p.includes(id) ? p.filter((x) => x !== id) : [...p, id]));
  const move = (id: string, d: -1 | 1) =>
    setPicked((p) => {
      const i = p.indexOf(id);
      const j = i + d;
      if (i < 0 || j < 0 || j >= p.length) return p;
      const n = [...p];
      [n[i], n[j]] = [n[j]!, n[i]!];
      return n;
    });
  const chosen = () => (order() === "source" ? accepted().filter((t) => picked().includes(t.id)).map((t) => t.id) : picked());
  const total = () => accepted().filter((t) => picked().includes(t.id)).reduce((s, t) => s + t.kept_ms, 0);
  const make = async () => {
    const v = cutView();
    if (!v || !chosen().length || busy()) return;
    setBusy(true);
    const r = await attempt(() => actions.longformFromTopics(v.project, chosen(), order(), title()));
    setBusy(false);
    if (!r) return;
    setLongformOpen(false);
    say(`Long-form made: ${r.parts} parts, ${clock(r.duration_ms)}`);
    await refreshMap(v.project);
    await openCut(v.project, r.cut);
  };
  return (
    <Sheet
      label="Make long-form from topics"
      testid="longform-sheet"
      class="longform-sheet"
      onClose={() => setLongformOpen(false)}
      sub="A 16:9 cut of what the stream edit keeps inside the topics you tick, one chapter per topic. Later stream-edit changes are offered, never pulled by themselves."
    >
        <ul class="lf-topics">
          <For each={accepted()} fallback={<li class="muted">No accepted topics yet.</li>}>
            {(t) => (
              <li>
                <label class="check">
                  <input type="checkbox" checked={picked().includes(t.id)} onChange={() => toggle(t.id)} />
                  <span class="dot" style={{ background: t.colour, "border-color": t.colour }} />
                  {t.title}
                  <Show when={several()}><span class="muted small">· {t.video}</span></Show>
                  <span class="muted small">{clock(t.kept_ms)}</span>
                </label>
                <Show when={order() === "given" && picked().includes(t.id)}>
                  <span class="lf-pos small">#{picked().indexOf(t.id) + 1}</span>
                  <button type="button" class="ghost small" aria-label={`Move ${t.title} up`} onClick={() => move(t.id, -1)}>↑</button>
                  <button type="button" class="ghost small" aria-label={`Move ${t.title} down`} onClick={() => move(t.id, 1)}>↓</button>
                </Show>
              </li>
            )}
          </For>
        </ul>
        <div class="row">
          <label class="check"><input type="radio" name="lf-order" checked={order() === "source"} onChange={() => setOrder("source")} /> Stream order</label>
          <label class="check"><input type="radio" name="lf-order" checked={order() === "given"} onChange={() => setOrder("given")} /> The order I tick them</label>
        </div>
        <div class="row">
          <input type="text" class="grow" aria-label="Long-form title" placeholder="Title (default: the first topic's)" value={title()} onInput={(e) => setTitle(e.currentTarget.value)} />
        </div>
        <div class="row">
          <span class="muted small">{chosen().length} topic{chosen().length === 1 ? "" : "s"} · {clock(total())}</span>
          <span class="spacer" />
          <Button variant="ghost" onClick={() => setLongformOpen(false)}>Cancel</Button>
          <Button variant="primary" data-testid="longform-make" disabled={!chosen().length || busy()} onClick={() => void make()}>Make long-form</Button>
        </div>
    </Sheet>
  );
}
