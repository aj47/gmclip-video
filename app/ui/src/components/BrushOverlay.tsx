// The edge brush (R.02, "fix edges with a brush"): over the viewer, drag on the frame to
// add to or take away from the selected part's cut-out. The stroke is mapped from canvas
// pixels to the source through the matted layer's place and crop and committed as one
// matte_brush on release (⌘Z undoes it); it applies to every frame of this part.
import { createSignal, Show } from "solid-js";
import { actions, type LayerView, type PartView } from "../actions";
import { brushFeather, brushMode, brushSize } from "../roto";
import { attempt, cutView, viewer } from "../store";
import type { CanvasRect } from "./Viewer";

/** The matted layer the brush paints on: the selected one if matted, else the first. */
export function brushLayer(part: PartView | undefined): LayerView | undefined {
  const sel = viewer()?.selected;
  const ls = (part?.layers ?? []).filter((l) => l.matte && l.model && l.crop_n);
  return ls.find((l) => sel?.[0] === part?.plan_id && sel?.[1] === l.id) ?? ls[0];
}

export function BrushOverlay(props: { rect: CanvasRect; part: PartView | undefined }) {
  const [pts, setPts] = createSignal<[number, number][]>([]);
  const [hover, setHover] = createSignal<[number, number] | null>(null);
  const layer = () => brushLayer(props.part);
  const scale = () => props.rect.w / (cutView()?.canvas.w ?? 1080);
  // canvas px → normalised source through the layer's place and crop
  const toSource = (cx: number, cy: number): [number, number] | null => {
    const l = layer();
    if (!l?.crop_n) return null;
    const [px, py, pw, ph] = l.place;
    const [nx, ny, nw, nh] = l.crop_n;
    return [nx + ((cx - px) / pw) * nw, ny + ((cy - py) / ph) * nh];
  };
  // the brush radius on screen: radius × source height, source px → canvas px → display
  const radiusPx = () => {
    const l = layer();
    if (!l) return 0;
    return brushSize() * l.src_size[1] * (l.place[2] / Math.max(1, l.crop[2])) * scale();
  };
  const local = (e: PointerEvent): [number, number] => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    return [e.clientX - r.left, e.clientY - r.top];
  };
  const down = (e: PointerEvent) => {
    if (e.button !== 0 || !layer()) return;
    e.preventDefault();
    e.stopPropagation();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    setPts([local(e)]);
  };
  const move = (e: PointerEvent) => {
    const p = local(e);
    setHover(p);
    if (pts().length && e.buttons) setPts([...pts(), p]);
  };
  const up = () => {
    const v = cutView();
    const l = layer();
    const p = props.part;
    const framing = v?.framing?.active;
    const s = scale();
    const src = pts()
      .map(([x, y]) => toSource(x / s, y / s))
      .filter((q): q is [number, number] => q !== null)
      // ≤ 4096 points: keep every nth
      .filter((_, i, a) => a.length <= 4096 || i % Math.ceil(a.length / 4096) === 0);
    setPts([]);
    if (!v || !l?.model || !p || !framing || src.length === 0) return;
    void attempt(() =>
      actions.matteBrush(v.project, l.model!, p.id, framing, [
        { mode: brushMode(), radius: brushSize(), feather: brushFeather(), points: src },
      ]),
    );
  };
  return (
    <div
      class="brush-overlay"
      data-testid="brush-overlay"
      style={{ left: `${props.rect.x}px`, top: `${props.rect.y}px`, width: `${props.rect.w}px`, height: `${props.rect.h}px` }}
      title={layer() ? `Brush: ${brushMode() === "add" ? "add to" : "take away from"} the cut-out on this part` : "No cut-out on this part to fix"}
      onPointerDown={down}
      onPointerMove={move}
      onPointerUp={up}
      onPointerLeave={() => setHover(null)}
    >
      <svg width={props.rect.w} height={props.rect.h}>
        <Show when={pts().length > 0}>
          <polyline
            points={pts().map((p) => p.join(",")).join(" ")}
            fill="none"
            stroke={brushMode() === "add" ? "var(--brush-add, #4ade80)" : "var(--brush-sub, #f87171)"}
            stroke-opacity="0.6"
            stroke-width={2 * radiusPx()}
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </Show>
        <Show when={hover()}>
          {(h) => <circle cx={h()[0]} cy={h()[1]} r={radiusPx()} fill="none" stroke="var(--fg, #fff)" stroke-dasharray="3 3" />}
        </Show>
      </svg>
    </div>
  );
}
