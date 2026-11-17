// "Draw your layout once" (SPEC §5.7.2; Ideas 2026-10-16, K.01): one frame of a video,
// drag boxes and name them — screen, camera, chat, private (alerts too) — and save them
// as the video's stream layout. New cuts from the video take their face from the camera
// box and their panel from the screen box; chat, alerts and private boxes are covered by
// the private-region lint and its fix. The same layout can go on the project's other
// videos of the same size, and be kept as a preset for every project.
import { createEffect, createResource, createSignal, For, Show } from "solid-js";
import { ideaActions, type LayoutBox, type RgbFrame } from "../actions";
import { Button, IconButton, SegmentedControl, Sheet, Switch } from "../design/components";
import { attempt, cutView, refreshCut, say } from "../store";
import { refreshMap } from "../streamState";
import "./ideas.css";

const UNIQUE = new Set(["screen", "cam"]);

function pct(n: number): string {
  return `${(n * 100).toFixed(3)}%`;
}

function clock(s: number): string {
  const t = Math.floor(s);
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, "0")}`;
}

export function DrawLayoutSheet(props: { project: string; source: string; onClose: () => void }) {
  const [view] = createResource(() => attempt(() => ideaActions.layoutDrawView(props.project, props.source)));
  const [frame, setFrame] = createSignal<RgbFrame | null>(null);
  const [boxes, setBoxes] = createSignal<LayoutBox[]>([]);
  const [role, setRole] = createSignal("screen");
  const [name, setName] = createSignal("Main scene");
  const [also, setAlso] = createSignal<string[]>([]);
  const [preset, setPreset] = createSignal(true);
  const [busy, setBusy] = createSignal(false);
  const [band, setBand] = createSignal<LayoutBox | null>(null);
  let canvas: HTMLCanvasElement | undefined;
  let stage: HTMLDivElement | undefined;

  createEffect(() => {
    const v = view();
    if (!v) return;
    setName(v.name);
    setBoxes(v.regions);
    // the same-size videos with no layout of their own take it too, by default
    setAlso(v.siblings.filter((s) => !s.layout).map((s) => s.source));
    void loadFrame();
  });
  const loadFrame = async (at?: number) => {
    const f = await attempt(() => ideaActions.layoutDrawFrame(props.project, props.source, at));
    if (f) setFrame(f);
  };
  const another = () => {
    const v = view();
    const f = frame();
    if (!v) return;
    const step = Math.max(1, v.duration_s / 7);
    const next = ((f?.at_s ?? 0) + step) % Math.max(1, v.duration_s);
    void loadFrame(next);
  };
  createEffect(() => {
    const f = frame();
    if (!f || !canvas) return;
    const bin = atob(f.rgb);
    const rgba = new Uint8ClampedArray(f.w * f.h * 4);
    for (let k = 0, j = 0; k < f.w * f.h; k++, j += 3) {
      rgba[k * 4] = bin.charCodeAt(j);
      rgba[k * 4 + 1] = bin.charCodeAt(j + 1);
      rgba[k * 4 + 2] = bin.charCodeAt(j + 2);
      rgba[k * 4 + 3] = 255;
    }
    canvas.width = f.w;
    canvas.height = f.h;
    canvas.getContext("2d")?.putImageData(new ImageData(rgba, f.w, f.h), 0, 0);
  });

  // drawing: a press on the picture starts a box, the release keeps it (≥ 1% a side)
  let from: { x: number; y: number } | null = null;
  const at = (e: PointerEvent) => {
    const r = stage!.getBoundingClientRect();
    return { x: Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)), y: Math.min(1, Math.max(0, (e.clientY - r.top) / r.height)) };
  };
  const rectOf = (a: { x: number; y: number }, b: { x: number; y: number }) => ({
    x: Math.min(a.x, b.x),
    y: Math.min(a.y, b.y),
    w: Math.abs(a.x - b.x),
    h: Math.abs(a.y - b.y),
  });
  const down = (e: PointerEvent) => {
    if (e.button !== 0 || !stage) return;
    e.preventDefault();
    stage.setPointerCapture?.(e.pointerId);
    from = at(e);
    setBand({ role: role(), rect: { ...from, w: 0, h: 0 } });
  };
  const move = (e: PointerEvent) => {
    if (!from) return;
    setBand({ role: role(), rect: rectOf(from, at(e)) });
  };
  const up = (e: PointerEvent) => {
    if (!from) return;
    const rect = rectOf(from, at(e));
    from = null;
    setBand(null);
    if (rect.w < 0.01 || rect.h < 0.01) return;
    const r = role();
    setBoxes((bs) => {
      const kept = UNIQUE.has(r) ? bs.filter((b) => b.role !== r) : bs;
      return [...kept, { role: r, rect }].slice(-8);
    });
  };
  const remove = (i: number) => setBoxes((bs) => bs.filter((_, k) => k !== i));
  const label = (r: string) => view()?.roles.find((x) => x.role === r)?.label ?? r;

  const save = async () => {
    if (!boxes().length || busy()) return;
    // read before closing: the sheet's props go stale once it is closed
    const project = props.project;
    setBusy(true);
    const r = await attempt(() => ideaActions.layoutDrawSave(project, props.source, name(), boxes(), also(), preset()));
    setBusy(false);
    if (!r) return;
    props.onClose();
    const where = r.videos > 1 ? `${r.videos} videos` : "this video";
    const priv = r.private ? ` · ${r.private} private region${r.private === 1 ? "" : "s"} covered` : "";
    say(`Layout “${r.name}” on ${where}${r.preset ? ", and kept as a preset" : ""}${priv} — ⌘Z undoes it`);
    void refreshMap(project);
    if (cutView()?.project === project) void refreshCut();
  };

  return (
    <Sheet
      label="Draw your layout"
      title="Draw your layout once"
      testid="draw-layout"
      class="dl-sheet"
      size="lg"
      onClose={props.onClose}
      sub="Box the screen, camera, chat and private parts of this video once: new cuts from it take the face from the camera box and the panel from the screen box; chat and private boxes stay covered."
    >
        <Show when={view()} fallback={<p class="muted small" role="status">{view.loading ? "Reading the video…" : "This video cannot be read here."}</p>}>
          {(v) => (
            <>
              <p class="muted small" data-testid="dl-frame-at">
                {v().file} · {v().canvas.w}×{v().canvas.h}
                <Show when={frame()?.at_s !== undefined}> · frame at {clock(frame()!.at_s!)}</Show>
              </p>
              <div class="row dl-tools">
                <SegmentedControl
                  label="Box to draw"
                  size="sm"
                  testid="dl-role"
                  value={role()}
                  onChange={setRole}
                  options={v().roles.map((r) => ({ value: r.role, label: r.label, hint: r.private ? `${r.label}: kept out of every cut (private)` : `Draw the ${r.label.toLowerCase()} box` }))}
                />
                <span class="spacer" />
                <Button size="sm" variant="ghost" icon="repeat" data-testid="dl-frame" tooltip="Show another frame of the video" onClick={another}>
                  Different frame
                </Button>
              </div>
              <div
                class="dl-stage"
                ref={stage}
                style={{ "aspect-ratio": `${v().canvas.w} / ${v().canvas.h}`, "--dl-ar": String(v().canvas.w / v().canvas.h) }}
                data-testid="dl-stage"
                onPointerDown={down}
                onPointerMove={move}
                onPointerUp={up}
              >
                <canvas ref={canvas} class="dl-frame" aria-label="A frame of the video" />
                <For each={boxes()}>
                  {(b) => (
                    <div class={`dl-box dl-${b.role}`} data-testid="dl-box" data-role={b.role} style={{ left: pct(b.rect.x), top: pct(b.rect.y), width: pct(b.rect.w), height: pct(b.rect.h) }}>
                      <span class="dl-tag">{label(b.role)}</span>
                    </div>
                  )}
                </For>
                <Show when={band()}>
                  {(b) => <div class={`dl-box dl-band dl-${b().role}`} style={{ left: pct(b().rect.x), top: pct(b().rect.y), width: pct(b().rect.w), height: pct(b().rect.h) }} />}
                </Show>
              </div>
              <ul class="dl-list" aria-label="Boxes">
                <For each={boxes()} fallback={<li class="muted small">Pick a box above, then drag it on the picture.</li>}>
                  {(b, i) => (
                    <li>
                      <span class={`dl-swatch dl-${b.role}`} />
                      <span>{label(b.role)}</span>
                      <span class="muted small">
                        {Math.round(b.rect.w * v().canvas.w)}×{Math.round(b.rect.h * v().canvas.h)} at {Math.round(b.rect.x * v().canvas.w)},{Math.round(b.rect.y * v().canvas.h)}
                      </span>
                      <IconButton icon="x" size="sm" label={`Remove the ${label(b.role)} box`} onClick={() => remove(i())} />
                    </li>
                  )}
                </For>
              </ul>
              <Show when={v().presets.length}>
                <div class="row">
                  <span class="muted small">Start from</span>
                  <For each={v().presets}>
                    {(p) => (
                      <Button size="sm" variant="ghost" data-testid="dl-preset" tooltip="Use this preset's boxes" onClick={() => { setBoxes(p.regions); setName(p.name); }}>
                        {p.name}
                      </Button>
                    )}
                  </For>
                </div>
              </Show>
              <label class="row dl-name">
                <span class="small">Layout name</span>
                <input type="text" data-testid="dl-name" value={name()} onInput={(e) => setName(e.currentTarget.value)} />
              </label>
              <Show when={v().siblings.length}>
                <Switch
                  testid="dl-also"
                  on={also().length > 0}
                  onChange={(on) => setAlso(on ? v().siblings.map((s) => s.source) : [])}
                  label={<>Also on the {v().siblings.length} other video{v().siblings.length === 1 ? "" : "s"} of this size in the project</>}
                  hint={v().siblings.map((s) => s.name + (s.layout ? ` (has “${s.layout}”)` : "")).join(", ")}
                />
              </Show>
              <Switch testid="dl-preset-save" on={preset()} onChange={setPreset} label={<>Keep it as a preset for all my projects</>} hint="Offered on any video of this size; your 9:16 layouts list it too" />
              <div class="row dl-foot">
                <span class="muted small">{boxes().length} box{boxes().length === 1 ? "" : "es"}{boxes().some((b) => b.role === "cam") ? "" : " · no camera box: new cuts will have no face"}</span>
                <span class="spacer" />
                <Button variant="ghost" onClick={props.onClose}>Skip</Button>
                <Button variant="primary" data-testid="dl-save" disabled={!boxes().length || busy()} onClick={() => void save()}>
                  Save layout
                </Button>
              </div>
            </>
          )}
        </Show>
    </Sheet>
  );
}
