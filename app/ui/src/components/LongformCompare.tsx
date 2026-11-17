// Compare two long-forms (Ideas 2026-10-16, K.03): side by side, each one's chapters
// (topics) in order with their lengths and a frame strip, what moved and what only one
// plays. Pick one (the other is removed, one ⌘Z brings it back) or keep both.
import { createEffect, createResource, createSignal, For, Show } from "solid-js";
import { ideaActions, type ComparedLong, type RgbFrame } from "../actions";
import { Button, Chip, Sheet } from "../design/components";
import { attempt, cutView, openCut, sayUndo } from "../store";
import { projectMapData, refreshMap } from "../streamState";
import "./ideas.css";

function clock(ms: number): string {
  const t = Math.round(ms / 1000);
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const s = String(t % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

function Thumb(props: { f: RgbFrame | null; colour: string }) {
  let c: HTMLCanvasElement | undefined;
  createEffect(() => {
    const f = props.f;
    if (!f || !c) return;
    const bin = atob(f.rgb);
    const rgba = new Uint8ClampedArray(f.w * f.h * 4);
    for (let k = 0, j = 0; k < f.w * f.h; k++, j += 3) {
      rgba[k * 4] = bin.charCodeAt(j);
      rgba[k * 4 + 1] = bin.charCodeAt(j + 1);
      rgba[k * 4 + 2] = bin.charCodeAt(j + 2);
      rgba[k * 4 + 3] = 255;
    }
    c.width = f.w;
    c.height = f.h;
    c.getContext("2d")?.putImageData(new ImageData(rgba, f.w, f.h), 0, 0);
  });
  return (
    <span class="lc-thumb" style={{ "border-color": props.colour }}>
      <Show when={props.f} fallback={<span class="lc-nothumb" style={{ background: props.colour }} />}>
        <canvas ref={c} aria-hidden="true" />
      </Show>
    </span>
  );
}

function Side(props: { l: ComparedLong; moved: string[]; only: string[]; other: ComparedLong; onPick: () => void; onOpen: () => void; testid: string }) {
  const total = () => Math.max(1, props.l.duration_ms);
  return (
    <section class="lc-side" aria-label={props.l.title} data-testid={props.testid}>
      <header class="lc-head">
        <b class="lc-title">{props.l.title}</b>
        <span class="muted small">
          {clock(props.l.duration_ms)} · {props.l.chapters.length} chapter{props.l.chapters.length === 1 ? "" : "s"} · {props.l.parts} part{props.l.parts === 1 ? "" : "s"}
          {props.l.duration_ms !== props.other.duration_ms ? ` · ${props.l.duration_ms > props.other.duration_ms ? "+" : "−"}${clock(Math.abs(props.l.duration_ms - props.other.duration_ms))}` : ""}
        </span>
      </header>
      <div class="lc-strip" role="img" aria-label={`Chapters of ${props.l.title}`}>
        <For each={props.l.chapters}>
          {(c) => <span class="lc-seg" style={{ width: `${(c.dur_ms / total()) * 100}%`, background: c.colour }} title={`${c.title} · ${clock(c.dur_ms)}`} />}
        </For>
      </div>
      <ol class="lc-chapters">
        <For each={props.l.chapters}>
          {(c, i) => (
            <li data-testid="lc-chapter" classList={{ moved: props.moved.includes(c.topic), only: props.only.includes(c.topic) }}>
              <Thumb f={c.frame} colour={c.colour} />
              <span class="lc-n">{i() + 1}</span>
              <span class="lc-ch">{c.title}</span>
              <span class="muted small">{clock(c.at_ms)} · {clock(c.dur_ms)}</span>
              <Show when={props.moved.includes(c.topic)}><Chip tone="accent" tooltip="At another place in the other long-form">moved</Chip></Show>
              <Show when={props.only.includes(c.topic)}><Chip tone="warning" tooltip="The other long-form does not play this topic">only here</Chip></Show>
            </li>
          )}
        </For>
      </ol>
      <div class="row">
        <Button size="sm" variant="ghost" icon="folder-open" onClick={props.onOpen}>Open</Button>
        <span class="spacer" />
        <Button size="sm" variant="primary" data-testid={`${props.testid}-pick`} tooltip="Keep this one; the other is removed (⌘Z brings it back)" onClick={props.onPick}>
          Pick this one
        </Button>
      </div>
    </section>
  );
}

export function LongformCompare(props: { project: string; a: string; b: string; onClose: () => void }) {
  const longs = () => projectMapData()?.longs ?? [];
  const [a, setA] = createSignal(props.a);
  const [b, setB] = createSignal(props.b);
  const [cmp] = createResource(() => ({ a: a(), b: b() }), (k) => attempt(() => ideaActions.longformCompare(props.project, k.a, k.b)));
  const pick = async (keep: string, drop: string) => {
    // read before closing: the sheet's props go stale once it is closed
    const project = props.project;
    const r = await attempt(() => ideaActions.longformPick(project, keep, drop));
    if (!r) return;
    props.onClose();
    await refreshMap(project);
    if (cutView()?.cut === drop) await openCut(project, keep);
    sayUndo(`Kept one long-form; removed “${r.dropped_title}”`, project);
  };
  const open = async (cut: string) => {
    const project = props.project;
    props.onClose();
    await openCut(project, cut);
  };
  const Picker = (p: { value: string; set: (v: string) => void; label: string }) => (
    <select aria-label={p.label} value={p.value} onChange={(e) => p.set(e.currentTarget.value)}>
      <For each={longs()}>{(l) => <option value={l.cut}>{l.title}</option>}</For>
    </select>
  );
  return (
    <Sheet label="Compare long-forms" testid="longform-compare" class="lc-sheet" size="lg" onClose={props.onClose} sub="Each one's chapters in order, what moved and what only one plays. Pick one to keep (⌘Z brings the other back), or keep both.">
        <div class="row lc-pickers">
          <Picker value={a()} set={setA} label="First long-form" />
          <span class="muted">and</span>
          <Picker value={b()} set={setB} label="Second long-form" />
        </div>
        <Show when={cmp()} fallback={<p class="muted small" role="status">{cmp.loading ? "Comparing…" : "These two cannot be compared."}</p>}>
          {(c) => (
            <>
              <p class="muted small" role="status" data-testid="lc-summary">
                {c().same_topics ? (c().same_order ? "The same topics in the same order." : `The same topics; ${c().moved.length} in another place.`) : `${c().only_a.length + c().only_b.length} topic${c().only_a.length + c().only_b.length === 1 ? "" : "s"} in only one of them.`}
              </p>
              <div class="lc-sides">
                <Side l={c().a} other={c().b} moved={c().moved} only={c().only_a} testid="lc-a" onPick={() => void pick(c().a.cut, c().b.cut)} onOpen={() => void open(c().a.cut)} />
                <Side l={c().b} other={c().a} moved={c().moved} only={c().only_b} testid="lc-b" onPick={() => void pick(c().b.cut, c().a.cut)} onOpen={() => void open(c().b.cut)} />
              </div>
            </>
          )}
        </Show>
        <div class="row">
          <span class="spacer" />
          <Button variant="ghost" data-testid="lc-keep-both" onClick={props.onClose}>Keep both</Button>
        </div>
    </Sheet>
  );
}
