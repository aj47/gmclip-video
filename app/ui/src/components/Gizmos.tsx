// Gizmos over the viewer besides the layer boxes the render thread draws (U.12, SPEC §7):
// each text overlay on the paused frame (a hook, a pill, a run clock) is a box you drag to
// move, and the caption seat is a handle you drag up or down (⇧ moves only this part's).
// A drag is a human gesture (R-GEST-1): pointer down holds the overlay (or the framing, or
// the part) in the core, release commits one command, Esc puts it back with nothing
// written. The boxes come from the core's own text layout, so they sit on the glyphs, and
// while one drags the frame itself shows the move: the render thread draws those text
// events moved through the shared compositor (`viewer_text_drag`, never logged), and keeps
// them there until the commit's re-planned cut replaces the preview with the plan's own.
import { createResource, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { actions, type TextRole, type ViewerGizmos } from "../actions";
import { attempt, cutView, editing, selectedOverlay, setSelectedOverlay, viewer } from "../store";
import { escCancels, hold, type Held } from "../gesture";
import type { CanvasRect } from "./Viewer";

const ROLE: Record<string, string> = { text_hook: "Hook", section_pill: "Pill", run_clock: "Run clock", note: "Text" };
/** Snap distance to the frame's centre line, in screen px (⌥ off). */
const SNAP_PX = 8;

type Drag =
  | { kind: "text"; overlay: string; x0: number; y0: number; dx: number; dy: number; held: Held }
  /** an added video or image: moved (body) or resized from its bottom-right corner */
  | { kind: "pic"; overlay: string; resize: boolean; x0: number; y0: number; dx: number; dy: number; held: Held }
  | { kind: "seat"; y0: number; dy: number; part: string | null; held: Held };

export function Gizmos(props: { rect: CanvasRect }) {
  // the frame rect, followed like the comment pins (resize, framing switch)
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
  // what is on the paused frame (nothing while playing: the frame moves under the pointer)
  const key = () => {
    const v = cutView();
    const vs = viewer();
    if (!v || !vs || vs.playing || vs.cut !== v.cut) return null;
    return { p: v.project, c: v.cut, f: vs.frame, rev: v.rev, fr: vs.framing };
  };
  // a read: a frame the core cannot measure just shows no gizmos (no toast)
  const [g, { refetch }] = createResource(key, (k) => actions.viewerGizmos(k.p, k.c, k.f).catch(() => null));
  const [drag, setDrag] = createSignal<Drag | null>(null);
  // the last result stays up while the next one loads (no flicker on a frame step)
  let last: ViewerGizmos | null = null;
  const gz = () => {
    const v = g.latest ?? null;
    if (v) last = v;
    return key() ? last : null;
  };
  const k = () => {
    const c = gz()?.canvas;
    return c ? rect().w / c.w : 0;
  };

  // the frame's preview of the move: at most one request per animation frame, the last
  let queued: { role: TextRole; at: [number, number] | null; dx: number; dy: number } | null = null;
  let raf = 0;
  let previewed = false;
  const preview = (role: TextRole, at: [number, number] | null, dx: number, dy: number) => {
    queued = { role, at, dx, dy };
    if (raf) return;
    raf = requestAnimationFrame(() => {
      raf = 0;
      const q = queued;
      queued = null;
      if (!q || !drag()) return;
      previewed = true;
      void actions.viewerTextDrag(q.role, q.at, q.dx, q.dy).catch(() => undefined);
    });
  };
  const stopPreview = () => {
    if (raf) cancelAnimationFrame(raf);
    raf = 0;
    queued = null;
  };
  onCleanup(stopPreview);
  /** The release: the preview stays drawn until the re-planned cut arrives (or goes back
   *  at once when nothing was committed). */
  const endPreview = (committed: boolean) => {
    stopPreview();
    if (!previewed) return Promise.resolve();
    if (!committed) previewed = false;
    return actions.viewerTextDragEnd(committed).catch(() => undefined);
  };

  const cancel = () => {
    drag()?.held.cancel();
    setDrag(null);
    void endPreview(false);
  };
  escCancels(() => drag() !== null, cancel);
  // Esc lets go of a picked video or image (its box stops taking the preview's clicks)
  onMount(() => {
    const esc = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !drag() && selectedOverlay()) setSelectedOverlay(null);
    };
    addEventListener("keydown", esc);
    onCleanup(() => removeEventListener("keydown", esc));
  });

  const downText = (e: PointerEvent, t: ViewerGizmos["texts"][number]) => {
    const v = cutView();
    if (e.button !== 0 || !v) return;
    e.preventDefault();
    e.stopPropagation();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    previewed = false;
    setDrag({ kind: "text", overlay: t.overlay, x0: e.clientX, y0: e.clientY, dx: 0, dy: 0, held: hold(v.project, "overlay", t.overlay) });
  };
  const downPic = (e: PointerEvent, p: NonNullable<ViewerGizmos["pictures"]>[number], resize: boolean) => {
    const v = cutView();
    if (e.button !== 0 || !v) return;
    e.preventDefault();
    e.stopPropagation();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    setDrag({ kind: "pic", overlay: p.overlay, resize, x0: e.clientX, y0: e.clientY, dx: 0, dy: 0, held: hold(v.project, "overlay", p.overlay) });
  };
  const downSeat = (e: PointerEvent) => {
    const v = cutView();
    const c = gz()?.caption;
    if (e.button !== 0 || !v || !c) return;
    e.preventDefault();
    e.stopPropagation();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    // ⇧: only the part on screen; else the framing's seat (every part without its own)
    const one = e.shiftKey && c.part ? c.part : null;
    const framing = viewer()?.framing ?? cutView()?.framing?.active ?? null;
    const held = one ? hold(v.project, "part", one) : framing ? hold(v.project, "framing", framing) : null;
    if (!held) return;
    previewed = false;
    setDrag({ kind: "seat", y0: e.clientY, dy: 0, part: one, held });
  };
  /** The seat's committed y for a drag of `dy` screen px (canvas px, whole). */
  const seatAt = (y: number, dy: number) => Math.round(Math.min((gz()?.canvas.h ?? 1920) - 1, Math.max(1, y + dy / k())));
  const move = (e: PointerEvent) => {
    const d = drag();
    if (!d || !e.buttons) return;
    if (d.kind === "seat") {
      const dy = e.clientY - d.y0;
      setDrag({ ...d, dy });
      const c = gz()?.caption;
      if (c) preview("caption", null, 0, seatAt(c.y, dy) - c.y);
      return;
    }
    if (d.kind === "pic") {
      setDrag({ ...d, dx: e.clientX - d.x0, dy: e.clientY - d.y0 });
      return;
    }
    let dx = e.clientX - d.x0;
    const t = gz()?.texts.find((x) => x.overlay === d.overlay);
    // the box's centre snaps to the frame's centre line (⌥ off)
    if (t && !e.altKey) {
      const cx = (t.box[0] + t.box[2] / 2) * k() + dx;
      if (Math.abs(cx - rect().w / 2) <= SNAP_PX) dx += rect().w / 2 - cx;
    }
    const dy = e.clientY - d.y0;
    setDrag({ ...d, dx, dy });
    if (t && k() > 0) preview(t.role, t.at, dx / k(), dy / k());
  };
  /** After a commit: the gizmos read back before the preview goes (never the old place
   * for a moment), and only this drag's preview (a drag begun since keeps its own). */
  const settle = (d: Drag) => async () => {
    await refetch();
    if (drag() === d) setDrag(null);
  };
  const up = () => {
    const d = drag();
    const v = cutView();
    const cur = gz();
    if (!d) return;
    if (!v || !cur || (Math.abs(d.kind === "seat" ? 0 : d.dx) < 1 && Math.abs(d.dy) < 1)) {
      cancel();
      return;
    }
    // the preview stays on the frame through the commit; a failed one puts it back; the
    // gizmos read back before this drag's state goes (never the old place for a moment)
    const done = (r: unknown) => {
      if (r === undefined) void endPreview(false);
      return settle(d)();
    };
    if (d.kind === "pic") {
      const p = (cur.pictures ?? []).find((x) => x.overlay === d.overlay);
      if (!p) return cancel();
      d.held.cancel();
      const place = d.resize
        ? { x: p.place.x, y: p.place.y, w: Math.max(0.02, Math.min(1.5, p.place.w + d.dx / rect().w)) }
        : { x: p.place.x + d.dx / rect().w, y: p.place.y + d.dy / rect().h, w: p.place.w };
      void editing(attempt(() => actions.overlayPlace(v.project, d.overlay, place))).then(() => settle(d)());
      return;
    }
    if (d.kind === "text") {
      const t = cur.texts.find((x) => x.overlay === d.overlay);
      if (!t) return cancel();
      const x = t.place.x + d.dx / rect().w;
      const y = t.place.y + d.dy / rect().h;
      void endPreview(true)
        .then(() => editing(attempt(() => d.held.commit((gesture) => actions.overlayDrag(v.project, v.cut, d.overlay, x, y, gesture)))))
        .then(done);
    } else {
      const c = cur.caption;
      if (!c) return cancel();
      const y = seatAt(c.y, d.dy);
      void endPreview(true)
        .then(() => editing(attempt(() => d.held.commit((gesture) => actions.captionSeat(v.project, v.cut, y, d.part, gesture)))))
        .then(done);
    }
  };

  const boxStyle = (b: [number, number, number, number], dx = 0, dy = 0) => ({
    left: `${b[0] * k() + dx}px`,
    top: `${b[1] * k() + dy}px`,
    width: `${Math.max(12, b[2] * k())}px`,
    height: `${Math.max(12, b[3] * k())}px`,
  });
  const seatY = () => {
    const c = gz()?.caption;
    if (!c) return 0;
    const d = drag();
    return c.y * k() + (d?.kind === "seat" ? d.dy : 0);
  };

  return (
    <div
      class="gizmos"
      ref={el}
      data-testid="gizmos"
      style={{ left: `${rect().x}px`, top: `${rect().y}px`, width: `${rect().w}px`, height: `${rect().h}px` }}
      onPointerMove={move}
      onPointerUp={up}
      onPointerCancel={cancel}
    >
      <For each={gz()?.pictures ?? []}>
        {(p) => {
          const d = () => {
            const x = drag();
            return x?.kind === "pic" && x.overlay === p.overlay ? x : null;
          };
          // a resize keeps the picture's shape: the height follows the width
          const style = () => {
            const x = d();
            const b = p.box;
            if (x?.resize && k() > 0) {
              const w = Math.max(12, b[2] * k() + x.dx);
              return { left: `${b[0] * k()}px`, top: `${b[1] * k()}px`, width: `${w}px`, height: `${(w * b[3]) / Math.max(1e-6, b[2])}px` };
            }
            return boxStyle(b, x?.dx ?? 0, x?.dy ?? 0);
          };
          return (
            <div class="gz-pic" classList={{ dragging: !!d(), sel: selectedOverlay() === p.overlay }}
              onPointerDown={(e) => { if (selectedOverlay() === p.overlay) downPic(e, p, false); }} data-testid="gizmo-picture" data-overlay={p.overlay} role="button"
              aria-label={`Added ${p.kind}. Drag to move it; drag its corner to resize.`}
              data-tip={`Added ${p.kind} · drag to move, corner resizes (Esc cancels)`}
              style={style()}>
              {/* the box lets clicks through to the layers under it; its tag moves it */}
              <span class="gz-tag gz-move" data-testid="gizmo-picture-move" onPointerDown={(e) => { setSelectedOverlay(p.overlay); downPic(e, p, false); }}>{p.kind === "video" ? "Video" : "Image"} ✥</span>
              <span class="gz-corner" data-testid="gizmo-picture-corner" onPointerDown={(e) => downPic(e, p, true)} />
            </div>
          );
        }}
      </For>
      <For each={gz()?.texts ?? []}>
        {(t) => {
          const d = () => {
            const x = drag();
            return x?.kind === "text" && x.overlay === t.overlay ? x : null;
          };
          return (
            <div
              class="gz-text"
              classList={{ dragging: !!d() }}
              data-testid="gizmo-text"
              data-overlay={t.overlay}
              data-role={t.role}
              role="button"
              aria-label={`${ROLE[t.role] ?? "Text"}: ${t.label}. Drag to move it; Esc puts it back.`}
              data-tip={`${ROLE[t.role] ?? "Text"} · drag to move (⌥ no snapping, Esc cancels)`}
              style={boxStyle(t.box, d()?.dx ?? 0, d()?.dy ?? 0)}
              onPointerDown={(e) => downText(e, t)}
            >
              <span class="gz-tag">{ROLE[t.role] ?? "Text"}</span>
            </div>
          );
        }}
      </For>
      <Show when={gz()?.caption?.editable ? gz()?.caption : null}>
        {(c) => (
          <>
            <Show when={c().box}>
              {(b) => <div class="gz-cue" classList={{ dragging: drag()?.kind === "seat" }} style={boxStyle(b(), 0, drag()?.kind === "seat" ? (drag() as { dy: number }).dy : 0)} />}
            </Show>
            <div
              class="gz-seat"
              classList={{ dragging: drag()?.kind === "seat" }}
              data-testid="gizmo-seat"
              data-y={Math.round(c().y)}
              role="slider"
              aria-orientation="vertical"
              aria-label="Caption height. Drag up or down; ⇧ moves only this part's captions."
              aria-valuenow={Math.round(c().y)}
              aria-valuemin={0}
              aria-valuemax={gz()?.canvas.h ?? 1920}
              data-tip="Captions · drag up or down (⇧ this part only, Esc cancels)"
              style={{ top: `${seatY()}px` }}
              onPointerDown={downSeat}
            >
              <span class="gz-grip" aria-hidden="true" />
              <span class="gz-tag">Captions{drag()?.kind === "seat" ? ` · y ${Math.round(seatY() / Math.max(1e-6, k()))}` : ""}</span>
            </div>
          </>
        )}
      </Show>
    </div>
  );
}
