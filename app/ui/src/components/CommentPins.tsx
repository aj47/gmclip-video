// Over the viewer (decision 0015): comment boxes near the playhead (±1.5 s), the box being
// drawn for a new comment, and a thumbnail title previewed on the frame. Drawing is a drag
// on the frame while "Draw a box" is on; the box is kept as fractions of the canvas.
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { rate } from "../actions";
import { cutView, viewer } from "../store";
import { draftBox, drawing, pickedThread, setDraftBox, threads, titlePreview } from "../collab";
import type { CanvasRect } from "./Viewer";

export function CommentPins(props: { rect: CanvasRect }) {
  const [start, setStart] = createSignal<[number, number] | null>(null);
  // The viewer hands its frame rect over once, at mount; this overlay stays mounted, so it
  // follows the canvas itself (resize, framing switch).
  const [rect, setRect] = createSignal<CanvasRect>(props.rect);
  let el: HTMLDivElement | undefined;
  onMount(() => {
    const canvas = el?.parentElement?.querySelector<HTMLCanvasElement>(".viewer-canvas");
    if (!canvas) return;
    const read = () => setRect({ x: canvas.offsetLeft, y: canvas.offsetTop, w: canvas.offsetWidth, h: canvas.offsetHeight });
    const ro = new ResizeObserver(read);
    ro.observe(canvas);
    if (canvas.parentElement) ro.observe(canvas.parentElement);
    read();
    onCleanup(() => ro.disconnect());
  });
  const near = () => {
    const f = viewer()?.frame ?? 0;
    const fps = rate(cutView()?.fps);
    return threads().filter((t) => t.box && t.frame !== null && (Math.abs(t.frame - f) <= fps * 1.5 || pickedThread() === t.id));
  };
  const frac = (e: PointerEvent): [number, number] => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    return [Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)), Math.min(1, Math.max(0, (e.clientY - r.top) / r.height))];
  };
  const down = (e: PointerEvent) => {
    if (!drawing() || e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    setStart(frac(e));
    setDraftBox(null);
  };
  const move = (e: PointerEvent) => {
    const s = start();
    if (!s || !e.buttons) return;
    const [x, y] = frac(e);
    setDraftBox({ x: Math.min(s[0], x), y: Math.min(s[1], y), w: Math.abs(x - s[0]), h: Math.abs(y - s[1]) });
  };
  const up = () => {
    setStart(null);
    const b = draftBox();
    if (b && (b.w < 0.01 || b.h < 0.01)) setDraftBox(null);
  };
  const pos = (b: { x: number; y: number; w: number; h: number }) => ({
    left: `${b.x * 100}%`,
    top: `${b.y * 100}%`,
    width: `${b.w * 100}%`,
    height: `${b.h * 100}%`,
  });
  return (
    <div
      class="comment-pins"
      classList={{ drawing: drawing() }}
      data-testid="comment-pins"
      ref={el}
      style={{ left: `${rect().x}px`, top: `${rect().y}px`, width: `${rect().w}px`, height: `${rect().h}px` }}
      onPointerDown={down}
      onPointerMove={move}
      onPointerUp={up}
      onPointerCancel={up}
    >
      <For each={near()}>
        {(t) => <div class="comment-pin" classList={{ resolved: t.resolved, picked: pickedThread() === t.id }} style={pos(t.box!)} data-thread={t.id} />}
      </For>
      <Show when={draftBox()}>{(b) => <div class="comment-pin draft" data-testid="draft-box" style={pos(b())} />}</Show>
      <Show when={titlePreview()}>
        {(p) => (
          <div
            class={`thumb-title-preview align-${p().align}`}
            data-testid="title-preview"
            style={{
              ...pos(p().box),
              color: p().style.fill,
              "font-size": `${p().style.size * rect().h}px`,
              "-webkit-text-stroke": `${Math.max(1, p().style.size * rect().h * 0.08)}px ${p().style.outline}`,
              "text-transform": p().style.uppercase ? "uppercase" : "none",
            }}
          >
            <For each={p().title.split(/(\*[^*]+\*)/)}>
              {(seg) => (seg.startsWith("*") && seg.endsWith("*") && seg.length > 2 ? <span style={{ color: p().style.accent }}>{seg.slice(1, -1)}</span> : <>{seg}</>)}
            </For>
          </div>
        )}
      </Show>
    </div>
  );
}
