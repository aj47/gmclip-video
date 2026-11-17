// A pane's drag handle (the side panel's left edge, the map's right edge, the timeline's
// top edge). Drag to resize, double-click for the layout's own size again, or focus it
// and use the arrow keys (⇧ for bigger steps). The size shows while dragging and is saved
// on release (prefs: side_w, map_w, tl_h).
import { createSignal } from "solid-js";
import { prefs, previewPref, setPref } from "../prefs";

type SizeKey = "side_w" | "map_w" | "tl_h";

export function Splitter(props: {
  /** Which pref holds the size. */
  pref: SizeKey;
  /** What it resizes, for its name ("side panel"). */
  what: string;
  class: string;
  /** x: drags sideways; y: drags up and down. */
  axis: "x" | "y";
  /** +1 when dragging right/down grows the pane, −1 when it shrinks it. */
  dir: 1 | -1;
  /** The pane's size now, in px (read when a drag starts). */
  measure: () => number;
  /** The allowed range now, in px. */
  range: () => [number, number];
  testid?: string;
}) {
  const [live, setLive] = createSignal<number | null>(null);
  const clamp = (n: number) => {
    const [lo, hi] = props.range();
    return Math.round(Math.max(lo, Math.min(Math.max(lo, hi), n)));
  };
  let start: { at: number; size: number } | null = null;
  const down = (e: PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    start = { at: props.axis === "x" ? e.clientX : e.clientY, size: props.measure() };
    document.body.classList.add(props.axis === "x" ? "resizing-x" : "resizing-y");
  };
  const move = (e: PointerEvent) => {
    if (!start) return;
    const d = (props.axis === "x" ? e.clientX : e.clientY) - start.at;
    const n = clamp(start.size + props.dir * d);
    setLive(n);
    previewPref(props.pref, n);
  };
  const up = () => {
    if (!start) return;
    start = null;
    document.body.classList.remove("resizing-x", "resizing-y");
    const n = live();
    setLive(null);
    if (n !== null) setPref(props.pref, n);
  };
  const key = (e: KeyboardEvent) => {
    const step = e.shiftKey ? 64 : 16;
    const grow = props.axis === "x" ? (e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0) * props.dir : (e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : 0) * props.dir;
    if (e.key === "Home" || e.key === "Enter") {
      e.preventDefault();
      e.stopPropagation();
      setPref(props.pref, 0);
      return;
    }
    if (!grow) return;
    e.preventDefault();
    e.stopPropagation();
    setPref(props.pref, clamp(props.measure() + grow * step));
  };
  return (
    <div
      class={`splitter splitter-${props.axis} ${props.class}`}
      role="separator"
      tabindex="0"
      aria-orientation={props.axis === "x" ? "vertical" : "horizontal"}
      aria-label={`Resize the ${props.what}`}
      aria-valuenow={prefs[props.pref] || undefined}
      data-tip={`Drag to resize the ${props.what} · double-click resets`}
      data-testid={props.testid}
      data-live={live() !== null ? "1" : undefined}
      onPointerDown={down}
      onPointerMove={move}
      onPointerUp={up}
      onPointerCancel={up}
      onDblClick={() => setPref(props.pref, 0)}
      onKeyDown={key}
    >
      <span class="splitter-grip" />
    </div>
  );
}
