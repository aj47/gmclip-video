// The screen's look (decision 0046), in the inspector: a layer's frame (rounded corners, a
// soft shadow, a border; `layer_frame_set` on the layer) and padding (its place inset
// evenly from the canvas edges, `place_set`), with the background plate beside them
// (decision 0029's plates, the Screen Studio gradients among them); the camera bubble (a
// built-in layout on all or the selected parts); and the one-click look on a recording
// (`screen_studio_apply`, with "Undo look" back to the restore point it took first).
// Each change is one undo step.
import { createMemo, createResource, createSignal, For, Show } from "solid-js";
import { actions, studioActions, type BubbleShape, type LayerFrame, type LayerView, type PartView, type PlannedFrame } from "../actions";
import { Button, SegmentedControl, Switch } from "../design/components";
import { Icon } from "../design/Icon";
import { attempt, cutView, editing, refreshCut, say, sayAct, viewer } from "../store";
import { selection } from "../partGroups";
import { hasRecording, refreshSections } from "../sectionsState";
import "./sections.css";

const SHADOW = { blur: 0.05, offset_y: 0.015, opacity: 0.45, colour: "#000000" };
const BORDER = { width: 0.005, colour: "#ffffff" };
const ROLE_NAME: Record<string, string> = { cam: "Camera", face: "Camera", screen: "Screen", detail: "Detail inset", extra: "Extra", clip: "Clip" };
const roleName = (r: string) => ROLE_NAME[r.split("-")[0] ?? r] ?? r;

/** The planned frame in the command's words (`colour` for `rgb`). */
function asCommand(f: PlannedFrame | null | undefined): LayerFrame {
  return {
    radius: f?.radius ?? 0,
    ...(f?.shadow ? { shadow: { blur: f.shadow.blur, offset_y: f.shadow.offset_y, opacity: f.shadow.opacity, colour: f.shadow.rgb } } : {}),
    ...(f?.border ? { border: { width: f.border.width, colour: f.border.rgb } } : {}),
  };
}

function Range(props: { label: string; testid: string; min: number; max: number; step: number; value: number; show: (v: number) => string; onCommit: (v: number) => void; disabled?: boolean }) {
  return (
    <label class="appearance-range small">
      <span>{props.label}</span>
      <input type="range" min={props.min} max={props.max} step={props.step} data-testid={props.testid} value={props.value} disabled={props.disabled} onChange={(e) => props.onCommit(Number(e.currentTarget.value))} />
      <span class="tabular">{props.show(props.value)}</span>
    </label>
  );
}

/** Corners, shadow, border and padding of one of the part's layers (the one selected on
 * the preview, else the screen, else the bottom one), and the plate behind. */
export function LayerLook(props: { part: PartView }) {
  const own = () => props.part.layers.filter((l) => l.model && l.role !== "cover");
  const [pick, setPick] = createSignal<string | null>(null);
  const layer = createMemo<LayerView | undefined>(() => {
    const ls = own();
    const sel = viewer()?.selected;
    const bySel = sel && sel[0] === props.part.plan_id ? ls.find((l) => l.id === sel[1]) : undefined;
    return ls.find((l) => l.model === pick()) ?? bySel ?? ls.find((l) => l.role === "screen") ?? [...ls].sort((a, b) => a.z - b.z)[0];
  });
  const [busy, setBusy] = createSignal(false);
  const run = async (f: () => Promise<unknown>, what: string) => {
    if (busy()) return;
    setBusy(true);
    const r = await editing(attempt(f));
    setBusy(false);
    if (r) say(`${what} · ⌘Z undoes`);
  };
  const frame = () => asCommand(layer()?.frame);
  const setFrame = (patch: Partial<LayerFrame>) => {
    const v = cutView();
    const l = layer();
    if (!v || !l?.model) return;
    const next: LayerFrame = { ...frame(), ...patch };
    if (!next.shadow) delete next.shadow;
    if (!next.border) delete next.border;
    const empty = next.radius === 0 && !next.shadow && !next.border;
    void run(() => studioActions.layerFrameSet(v.project, { layer: l.model!, frame: empty ? null : next }), "Look changed");
  };
  // padding: the gap to the nearest canvas edge, a share of the canvas's short side
  const short = () => Math.max(1, Math.min(cutView()?.canvas.w ?? 1, cutView()?.canvas.h ?? 1));
  const padding = () => {
    const v = cutView();
    const l = layer();
    if (!v || !l) return 0;
    const [x, y, w, h] = l.place;
    return Math.max(0, Math.min(x, y, v.canvas.w - x - w, v.canvas.h - y - h)) / short();
  };
  const setPadding = (p: number) => {
    const v = cutView();
    const l = layer();
    const f = v?.framing?.active;
    if (!v || !l?.model || l.place[2] <= 0 || l.place[3] <= 0) return;
    const { w: cw, h: ch } = v.canvas;
    const pad = p * short();
    const aspect = l.place[2] / l.place[3];
    const w = Math.max(16, Math.min(cw - 2 * pad, (ch - 2 * pad) * aspect));
    const h = w / aspect;
    const place = { x: (cw - w) / 2 / cw, y: (ch - h) / 2 / ch, w: w / cw };
    // a part's own place stays the part's (an override), else the layer's for every part
    const part = l.from?.place === "part" && f ? { part: props.part.id, framing: f } : {};
    void run(() => studioActions.placeSet(v.project, { layer: l.model!, place, ...part }), "Padding changed");
  };
  const pct = (x: number) => `${Math.round(x * 100)}%`;
  return (
    <Show when={layer()}>
      {(l) => (
        <section class="studio-block layer-look" aria-label="Look" data-testid="layer-look" data-layer={l().role} aria-busy={busy()}>
          <h3>
            <Icon name="square-dashed" size={14} />
            Look
            <Show when={own().length > 1} fallback={<span class="muted small">{roleName(l().role)}</span>}>
              <select aria-label="Layer to style" data-testid="look-layer" value={l().model ?? ""} onChange={(e) => setPick(e.currentTarget.value)}>
                <For each={own()}>{(x) => <option value={x.model!}>{roleName(x.role)}</option>}</For>
              </select>
            </Show>
          </h3>
          <div class="row">
            <Range label="Corners" testid="look-radius" min={0} max={0.5} step={0.01} value={frame().radius} show={(v) => (v >= 0.5 ? "circle" : pct(v))} onCommit={(v) => setFrame({ radius: v })} disabled={busy()} />
            <Button size="sm" variant="ghost" data-testid="look-circle" tooltip="Round it fully (a circle on a square layer)" onClick={() => setFrame({ radius: 0.5 })}>Circle</Button>
          </div>
          <Switch label="Shadow" testid="look-shadow" hint="A soft shadow under the layer" on={!!frame().shadow} disabled={busy()} onChange={(x) => setFrame({ shadow: x ? SHADOW : null })} />
          <Show when={frame().shadow}>
            {(s) => (
              <div class="row">
                <Range label="Softness" testid="look-shadow-blur" min={0} max={0.2} step={0.005} value={s().blur} show={pct} onCommit={(v) => setFrame({ shadow: { ...s(), blur: v } })} disabled={busy()} />
                <Range label="Offset" testid="look-shadow-offset" min={-0.1} max={0.1} step={0.005} value={s().offset_y} show={pct} onCommit={(v) => setFrame({ shadow: { ...s(), offset_y: v } })} disabled={busy()} />
                <Range label="Opacity" testid="look-shadow-opacity" min={0} max={1} step={0.05} value={s().opacity} show={pct} onCommit={(v) => setFrame({ shadow: { ...s(), opacity: v } })} disabled={busy()} />
              </div>
            )}
          </Show>
          <Switch label="Border" testid="look-border" hint="A line around the layer" on={!!frame().border} disabled={busy()} onChange={(x) => setFrame({ border: x ? BORDER : null })} />
          <Show when={frame().border}>
            {(b) => (
              <div class="row">
                <Range label="Width" testid="look-border-width" min={0} max={0.03} step={0.001} value={b().width} show={(v) => `${(v * 100).toFixed(1)}%`} onCommit={(v) => setFrame({ border: { ...b(), width: v } })} disabled={busy()} />
                <input type="color" aria-label="Border colour" data-testid="look-border-colour" value={b().colour} disabled={busy()} onChange={(e) => setFrame({ border: { ...b(), colour: e.currentTarget.value } })} />
              </div>
            )}
          </Show>
          <Range label="Padding" testid="look-padding" min={0} max={0.2} step={0.01} value={Math.min(0.2, Math.round(padding() * 100) / 100)} show={pct} onCommit={setPadding} disabled={busy()} />
          <PlatePick />
        </section>
      )}
    </Show>
  );
}

/** The background plate (decision 0029's picker, the Screen Studio gradients among the
 * built-ins). */
function PlatePick() {
  const [plates, { refetch }] = createResource(
    () => { const v = cutView(); return v ? { p: v.project, c: v.cut, rev: v.rev } : null; },
    async (k) => (await attempt(() => actions.platesView(k.p, k.c))) ?? null,
  );
  const rows = () => {
    const p = plates();
    if (!p) return [];
    const out = p.project.map((r) => ({ name: r.name, mine: false }));
    for (const r of p.mine) if (!out.some((x) => x.name === r.name)) out.push({ name: r.name, mine: true });
    return out;
  };
  const apply = async (name: string) => {
    const v = cutView();
    const f = v?.framing?.active;
    if (!v || !f || !name) return;
    const row = rows().find((r) => r.name === name);
    if (await editing(attempt(() => actions.plateApply(v.project, f, name, row?.mine ?? false)))) say(`Plate “${name}” behind the cut · ⌘Z undoes`);
    void refetch();
  };
  return (
    <label class="row small">Background
      <select aria-label="Background plate" data-testid="look-plate" value={plates()?.current ?? ""} onChange={(e) => void apply(e.currentTarget.value)}>
        <option value="">(none)</option>
        <For each={rows()}>{(r) => <option value={r.name}>{r.name}{r.mine ? " (mine)" : ""}</option>}</For>
      </select>
    </label>
  );
}

const CORNERS = ["bottom right", "bottom left", "top right", "top left"] as const;

/** The camera as a bubble over the screen: a built-in layout on all or the selected parts
 * (decision 0046; the paired face file of a recording, else a camera region). */
export function CameraBubble(props: { part: PartView }) {
  const [list] = createResource(
    () => { const v = cutView(); return v ? `${v.project}\n${v.cut}` : null; },
    async (k) => (await attempt(() => studioActions.layoutBuiltinList(k.split("\n")[0]!, k.split("\n")[1]!))) ?? null,
  );
  const [corner, setCorner] = createSignal<(typeof CORNERS)[number]>("bottom right");
  const [size, setSize] = createSignal(0.22);
  const [shape, setShape] = createSignal<BubbleShape>("circle");
  const [scope, setScope] = createSignal<"all" | "selected">("all");
  const [busy, setBusy] = createSignal(false);
  const name = () => `Screen + camera bubble (${corner()})`;
  const available = () => list()?.layouts.find((l) => l.name === name())?.available !== false;
  const apply = async () => {
    const v = cutView();
    const framing = v?.framing?.active;
    if (!v || !framing || busy()) return;
    const parts = [...new Set(scope() === "all" ? v.parts.map((p) => p.id) : selection().length ? selection() : [props.part.id])];
    setBusy(true);
    const r = await editing(attempt(() => studioActions.layoutBuiltinAdd(v.project, { cut: v.cut, framing, name: name(), parts, size: size(), shape: shape() })));
    setBusy(false);
    if (r) say(`Camera bubble (${corner()}) on ${parts.length} part${parts.length === 1 ? "" : "s"} · ⌘Z undoes`);
  };
  return (
    <Show when={list()?.layouts.some((l) => l.needs_camera && l.available !== false)}>
      <section class="studio-block camera-bubble" aria-label="Camera bubble" data-testid="camera-bubble" aria-busy={busy()}>
        <h3><Icon name="smile" size={14} />Camera bubble</h3>
        <div class="row small">
          <select aria-label="Bubble corner" data-testid="bubble-corner" value={corner()} onChange={(e) => setCorner(e.currentTarget.value as (typeof CORNERS)[number])}>
            <For each={CORNERS}>{(c) => <option value={c}>{c[0]!.toUpperCase() + c.slice(1)}</option>}</For>
          </select>
          <SegmentedControl<BubbleShape> label="Bubble shape" size="sm" testid="bubble-shape" value={shape()} onChange={setShape}
            options={[{ value: "circle", label: "Circle" }, { value: "rounded", label: "Rounded" }]} />
        </div>
        <Range label="Size" testid="bubble-size" min={0.1} max={0.4} step={0.01} value={size()} show={(v) => `${Math.round(v * 100)}%`} onCommit={setSize} />
        <div class="row small">
          <SegmentedControl<"all" | "selected"> label="Parts" size="sm" testid="bubble-scope" value={scope()} onChange={setScope}
            options={[{ value: "all", label: "All parts" }, { value: "selected", label: "Selected parts" }]} />
          <Button size="sm" icon="smile" data-testid="bubble-apply" disabled={busy() || !available()} tooltip={available() ? "Screen padded with the camera as a bubble in that corner (one undo)" : "This cut has no camera to put in a bubble"} onClick={() => void apply()}>
            Apply bubble
          </Button>
        </div>
      </section>
    </Show>
  );
}

/** One click on a recording: the Screen Studio look (decision 0046). */
export function ScreenStudioLook() {
  const [busy, setBusy] = createSignal(false);
  const apply = async () => {
    const v = cutView();
    if (!v || busy()) return;
    setBusy(true);
    const r = await editing(attempt(() => studioActions.screenStudioApply(v.project, v.cut)));
    setBusy(false);
    if (!r) return;
    void refreshSections();
    const notes = r.notes.length ? ` (${r.notes.join("; ")})` : "";
    sayAct(`Screen Studio look applied${notes}`, "Undo look", () => {
      void editing(attempt(() => studioActions.restoreTo(v.project, r.before))).then((x) => {
        if (!x) return;
        say("Look undone: back to before it");
        void refreshCut();
        void refreshSections();
      });
    });
  };
  return (
    <Show when={hasRecording()}>
      <div class="studio-block studio-look" data-testid="studio-look">
        <Button variant="primary" size="sm" icon="sparkles" data-testid="studio-look-apply" disabled={busy()} aria-busy={busy()}
          tooltip="Plate, padding, rounded corners and shadow, the camera as a bubble, a smooth cursor with ripples, and auto zoom from the input log: one click"
          onClick={() => void apply()}>
          {busy() ? "Applying the look…" : "Screen Studio look"}
        </Button>
      </div>
    </Show>
  );
}
