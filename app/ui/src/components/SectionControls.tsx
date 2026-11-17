// A selected section's controls (decision 0046), in the inspector. A zoom: how deep
// (1–4×), what it follows (the recorded cursor, the focused field, or a fixed point
// picked by a click on the preview), its eases, the camera shrinking while zoomed, motion
// blur (off by default: it renders sub-frames) and which layers zoom. A layout section:
// which layout (the framing's own, or a built-in one made on demand) and its ease. Each
// change is one `section_set` with the whole kind (an automatic zoom becomes hand-made).
import { createResource, For, Show } from "solid-js";
import { studioActions, type SectionKind, type SectionView, type ZoomSpec } from "../actions";
import { Button, SegmentedControl, Switch } from "../design/components";
import { Icon } from "../design/Icon";
import { attempt, cutView, editing, say, viewer } from "../store";
import type { CanvasRect } from "./Viewer";
import { pointPick, refreshSections, removeSection, sectionById, selectedSection, setPointPick } from "../sectionsState";
import "./sections.css";

const SHRINK = [0.5, 0.6, 0.7, 0.8, 0.9];
type TargetKind = "cursor" | "focus" | "point";

export function SectionControls() {
  const sec = () => sectionById(selectedSection());
  const write = async (s: SectionView, kind: SectionKind) => {
    const v = cutView();
    if (!v) return;
    const r = await editing(attempt(() => studioActions.sectionSet(v.project, { section: s.id, kind })));
    void refreshSections();
    if (r) say("Section changed · ⌘Z undoes");
  };
  const secs = (ms: number | null | undefined) => ((ms ?? 0) / 1000).toFixed(1);
  return (
    <Show when={sec()}>
      {(s) => (
        <section class="studio-block section-controls" aria-label="Section" data-testid="section-controls" data-kind={"zoom" in s().kind ? "zoom" : "layout"}>
          <h3>
            <Icon name={"zoom" in s().kind ? "zoom-in" : "layout"} size={14} />
            {"zoom" in s().kind ? "Zoom" : "Layout"} section
            <span class="muted small">{secs(s().start_ms)}–{secs(s().end_ms)} s</span>
          </h3>
          <Show when={"zoom" in s().kind ? (s().kind as { zoom: ZoomSpec }).zoom : null}>
            {(z) => <ZoomControls s={s()} z={z()} write={(k) => void write(s(), k)} />}
          </Show>
          <Show when={"layout" in s().kind ? (s().kind as { layout: { layout: string; ease_ms?: number } }).layout : null}>
            {(l) => <LayoutControls l={l()} write={(k) => void write(s(), k)} />}
          </Show>
          <div class="row">
            <Button size="sm" variant="ghost" icon="x" data-testid="section-remove" tooltip="Remove this section (⌫)" onClick={() => void removeSection(s().id)}>Remove</Button>
          </div>
        </section>
      )}
    </Show>
  );
}

function ZoomControls(props: { s: SectionView; z: ZoomSpec; write: (k: SectionKind) => void }) {
  /** The whole zoom with `patch` on it, hand-made from now on. */
  const set = (patch: Partial<ZoomSpec>) => {
    const next: ZoomSpec = { ...props.z, ...patch, auto: false };
    if (next.camera_scale == null) delete next.camera_scale;
    props.write({ zoom: next });
  };
  const depth = () => props.z.depth ?? 2;
  const target = (): TargetKind => {
    const t = props.z.target ?? "cursor";
    return t === "cursor" || t === "focus" ? t : "point";
  };
  const point = () => {
    const t = props.z.target;
    return t && typeof t === "object" ? t.point : null;
  };
  const pickTarget = (k: TargetKind) => {
    if (k === "point") {
      setPointPick(props.s.id);
      say("Click the preview where the zoom should hold (Esc cancels)");
    } else set({ target: k });
  };
  return (
    <>
      <Show when={props.z.auto}>
        <p class="note" data-testid="zoom-auto-note">Automatic (from the input log): editing it makes it yours, and Auto zoom leaves it alone.</p>
      </Show>
      <label class="appearance-range small">
        <span>Depth</span>
        <input type="range" min="1" max="4" step="0.25" data-testid="zoom-depth" value={Math.min(4, depth())} onChange={(e) => set({ depth: Number(e.currentTarget.value) })} />
        <span class="tabular" data-testid="zoom-depth-value">{depth()}×</span>
      </label>
      <div class="row">
        <SegmentedControl<TargetKind>
          label="Zoom target"
          size="sm"
          testid="zoom-target"
          value={pointPick() === props.s.id ? "point" : target()}
          onChange={pickTarget}
          options={[
            { value: "cursor", label: "Follow cursor", hint: "Glide after the recorded cursor" },
            { value: "focus", label: "Focus field", hint: "Aim at the caret or the focused field" },
            { value: "point", label: "Fixed point", hint: "Hold on a point: click it on the preview" },
          ]}
        />
        <Show when={point()}>
          {(p) => <span class="muted small" data-testid="zoom-point">at {Math.round(p().x * 100)}%, {Math.round(p().y * 100)}%</span>}
        </Show>
        <Show when={pointPick() === props.s.id}>
          <span class="muted small" data-testid="zoom-picking">Click the preview…</span>
        </Show>
      </div>
      <div class="row small">
        <label class="row">Ease in
          <input type="number" min="0" max="5000" step="50" aria-label="Ease in (ms)" data-testid="zoom-ease-in" value={props.z.ease_in_ms ?? 700} onChange={(e) => set({ ease_in_ms: Math.max(0, Math.round(Number(e.currentTarget.value) || 0)) })} /> ms
        </label>
        <label class="row">out
          <input type="number" min="0" max="5000" step="50" aria-label="Ease out (ms)" data-testid="zoom-ease-out" value={props.z.ease_out_ms ?? 700} onChange={(e) => set({ ease_out_ms: Math.max(0, Math.round(Number(e.currentTarget.value) || 0)) })} /> ms
        </label>
      </div>
      <div class="row small">
        <label class="row" data-tip="The camera shrinks toward its corner while zoomed in">Camera
          <select aria-label="Camera shrink while zoomed" data-testid="zoom-camera" value={props.z.camera_scale == null ? "" : String(props.z.camera_scale)} onChange={(e) => set({ camera_scale: e.currentTarget.value ? Number(e.currentTarget.value) : null })}>
            <option value="">stays</option>
            <For each={SHRINK}>{(x) => <option value={String(x)}>shrinks to {Math.round(x * 100)}%</option>}</For>
          </select>
        </label>
        <label class="row">Zooms
          <select aria-label="Which layers zoom" data-testid="zoom-layer" value={props.z.layer ?? "screen"} onChange={(e) => set({ layer: e.currentTarget.value as ZoomSpec["layer"] })}>
            <option value="screen">the screen</option>
            <option value="cursor">the cursor's layer</option>
            <option value="all">every layer</option>
          </select>
        </label>
      </div>
      <Switch label={<>Motion blur <span class="note">slower export</span></>} testid="zoom-blur" hint="Blur while the zoom moves (5 sub-frames each frame it moves: slower export)" on={!!props.z.motion_blur} onChange={(x) => set({ motion_blur: x })} />
    </>
  );
}

function LayoutControls(props: { l: { layout: string; ease_ms?: number }; write: (k: SectionKind) => void }) {
  const [builtins] = createResource(
    () => { const v = cutView(); return v ? `${v.project}\n${v.cut}` : null; },
    async (k) => (await attempt(() => studioActions.layoutBuiltinList(k.split("\n")[0]!, k.split("\n")[1]!))) ?? null,
  );
  const mine = () => cutView()?.framing?.layouts ?? [];
  const set = (layout: string, ease = props.l.ease_ms ?? 400) => props.write({ layout: { layout, ease_ms: ease } });
  const pick = async (value: string) => {
    const v = cutView();
    const framing = v?.framing?.active;
    if (!v || !value) return;
    if (!value.startsWith("builtin:")) return set(value);
    if (!framing) return say("This cut has no framing yet", "error");
    const made = await editing(attempt(() => studioActions.layoutBuiltinAdd(v.project, { cut: v.cut, framing, name: value.slice(8) })));
    if (made) set(made.id);
  };
  return (
    <>
      <label class="row small">Layout
        <select aria-label="Section layout" data-testid="section-layout" value={props.l.layout} onChange={(e) => void pick(e.currentTarget.value)}>
          <Show when={!mine().some((m) => m.id === props.l.layout)}>
            <option value={props.l.layout}>(a layout of another framing)</option>
          </Show>
          <For each={mine()}>{(m) => <option value={m.id}>{m.name}</option>}</For>
          <optgroup label="New from a built-in">
            <For each={(builtins()?.layouts ?? []).filter((b) => b.available !== false)}>{(b) => <option value={`builtin:${b.name}`}>{b.name}</option>}</For>
          </optgroup>
        </select>
      </label>
      <label class="row small">Ease
        <input type="number" min="0" max="5000" step="50" aria-label="Layout ease (ms)" data-testid="section-layout-ease" value={props.l.ease_ms ?? 400} onChange={(e) => set(props.l.layout, Math.max(0, Math.round(Number(e.currentTarget.value) || 0)))} /> ms
      </label>
    </>
  );
}

/** The preview while a zoom's fixed point is picked: a crosshair follows the pointer and a
 * click sets the point, normalised in the zoomed layer's crop (the screen's, else the
 * bottom layer's, of the part under the playhead). Esc or a right-click cancels. */
export function PointPicker(props: { rect: CanvasRect }) {
  let el: HTMLDivElement | undefined;
  const at = { x: -100, y: -100 };
  const cross = (e: PointerEvent) => {
    const c = el?.querySelector<HTMLElement>(".point-pick-cross");
    const r = el?.getBoundingClientRect();
    if (!c || !r) return;
    at.x = e.clientX - r.left;
    at.y = e.clientY - r.top;
    c.style.left = `${at.x}px`;
    c.style.top = `${at.y}px`;
  };
  const pick = async (e: MouseEvent) => {
    const id = pointPick();
    const v = cutView();
    const s = sectionById(id);
    if (!id || !v || !s || !("zoom" in s.kind) || !el) return setPointPick(null);
    const r = el.getBoundingClientRect();
    const cx = ((e.clientX - r.left) / r.width) * v.canvas.w;
    const cy = ((e.clientY - r.top) / r.height) * v.canvas.h;
    const f = viewer()?.frame ?? 0;
    const part = v.parts.find((p) => f >= p.out[0] && f < p.out[1]) ?? v.parts[0];
    const layers = [...(part?.layers ?? [])].sort((a, b) => a.z - b.z);
    const l = layers.find((x) => x.role === "screen") ?? layers[0];
    const clamp = (n: number) => Math.round(Math.max(0, Math.min(1, n)) * 1000) / 1000;
    const p = l ? { x: clamp((cx - l.place[0]) / Math.max(1, l.place[2])), y: clamp((cy - l.place[1]) / Math.max(1, l.place[3])) } : { x: clamp(cx / v.canvas.w), y: clamp(cy / v.canvas.h) };
    setPointPick(null);
    const r2 = await editing(attempt(() => studioActions.sectionSet(v.project, { section: id, kind: { zoom: { ...(s.kind as { zoom: ZoomSpec }).zoom, target: { point: p }, auto: false } } })));
    void refreshSections();
    if (r2) say(`The zoom holds at ${Math.round(p.x * 100)}%, ${Math.round(p.y * 100)}% · ⌘Z undoes`);
  };
  return (
    <div
      ref={(e) => {
        el = e;
        queueMicrotask(() => e.focus({ preventScroll: true }));
      }}
      class="point-pick"
      data-testid="point-pick"
      tabindex="-1"
      role="button"
      aria-label="Click where the zoom should hold; Esc cancels"
      style={{ left: `${props.rect.x}px`, top: `${props.rect.y}px`, width: `${props.rect.w}px`, height: `${props.rect.h}px` }}
      onPointerMove={cross}
      onClick={(e) => void pick(e)}
      onContextMenu={(e) => { e.preventDefault(); setPointPick(null); }}
      onKeyDown={(e) => { if (e.key === "Escape") { e.stopPropagation(); setPointPick(null); } }}
    >
      <span class="point-pick-hint">Click where the zoom should hold · Esc cancels</span>
      <span class="point-pick-cross" />
    </div>
  );
}
