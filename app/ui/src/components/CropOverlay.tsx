// The crop editor (AJ 2026-10-14: "make crops for vertical"): over the full source picture,
// each layer of the current part in the 9:16 output is a box — its crop. Drag a box to
// move it, drag a corner to resize it (its shape is kept, so the panel in the 9:16 frame
// keeps its size). A drag is a human gesture (U.12, R-GEST-1): pointer down holds the
// layer and its part in the core (an agent's write meanwhile gets Conflict{by: human}),
// releasing commits one crop_set for this part (⌘Z undoes it), Esc puts it back.
//
// AJ 2026-10-19 ("improve the cropping ui"): one layer is in focus (click a box or its
// chip); everything outside the crops is dimmed so what each layer keeps reads at a
// glance; each box says its size in source pixels; a toolbar centres, enlarges or resets
// the focused crop (its shape kept), and arrow keys nudge it (⇧ ×10); a thirds grid shows
// while dragging.
//
// AJ 2026-10-20 ("when I go to crop the camera, it shows me a preview of the screen"): the
// picture is the focused layer's own video (the cut view loads it), so a recording's camera
// layer is cropped over the camera file; only the boxes of layers cut from that video are
// drawn over it, the chips switch to the others. A layer grouped on the timeline takes its
// group along: the same crop on every layer of the group, one undo.
import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
import { actions, type LayerView, type PartView } from "../actions";
import { attempt, cutView, editing, groupFor, refreshCut, say } from "../store";
import { escCancels, hold, type Held } from "../gesture";
import type { CanvasRect } from "./Viewer";

type R = [number, number, number, number];

const clampRect = (r: R): R => {
  const w = Math.min(1, Math.max(0.02, r[2]));
  const h = Math.min(1, Math.max(0.02, r[3]));
  return [Math.min(1 - w, Math.max(0, r[0])), Math.min(1 - h, Math.max(0, r[1])), w, h];
};

export function CropOverlay(props: { rect: CanvasRect; part: PartView | undefined; focus: LayerView | undefined; setFocus: (layer: string | null) => void }) {
  const [live, setLive] = createSignal<{ layer: string; r: R } | null>(null);
  /** move, a corner (0–3: keeps the shape) or an edge (n e s w: changes it) */
  type Mode = "move" | 0 | 1 | 2 | 3 | "n" | "e" | "s" | "w";
  let drag: { layer: string; mode: Mode; x: number; y: number; r0: R; aspect: number; held: Held | null } | null = null;
  const cancel = () => {
    drag?.held?.cancel();
    drag = null;
    setLive(null);
  };
  escCancels(() => drag !== null, cancel);

  /** Every layer with a crop (the chips), and those over the picture shown (the boxes). */
  const all = () => (props.part?.layers ?? []).filter((l) => l.crop_n && l.model && l.visible);
  // the layer in focus: the toolbar and the arrow keys act on it
  const focused = () => props.focus;
  const setFocus = (layer: string | null) => props.setFocus(layer);
  const layers = () => {
    const f = focused();
    return all().filter((l) => !f || (l.source ?? props.part?.source) === (f.source ?? props.part?.source));
  };
  /** The layers grouped with the focused one on the timeline (it among them), else none. */
  const group = () => {
    const p = props.part;
    const l = focused();
    return p && l?.model ? groupFor(p.id, l.model) : undefined;
  };
  const [dragging, setDragging] = createSignal(false);
  const ROLE: Record<string, string> = { cam: "Camera", face: "Camera", screen: "Screen", detail: "Detail" };
  const roleName = (r: string) => ROLE[r.split("-")[0] ?? r] ?? r;
  const px = (l: { src_size: [number, number] }, r: R) => `${Math.round(r[2] * l.src_size[0])}×${Math.round(r[3] * l.src_size[1])}`;
  /** Writes a crop for the focused layer (one undo step), holding it until read back. */
  const commit = (layer: string, r: R) => {
    const v = cutView();
    const p = props.part;
    if (!v || !p) return;
    const lv = { layer, r: clampRect(r) };
    setLive(lv);
    void editing(attempt(() => actions.cropNorm(v.project, v.cut, p.id, layer, lv.r, null, groupFor(p.id, layer)).then(() => true)))
      .then((ok) => (ok ? refreshCut() : undefined))
      .finally(() => {
        if (live() === lv) setLive(null);
      });
  };
  /** The focused crop's shape in source pixels (w/h). */
  const shapeOf = (l: { src_size: [number, number] }, r: R) => (r[2] * l.src_size[0]) / Math.max(1e-6, r[3] * l.src_size[1]);
  const centre = () => {
    const l = focused();
    if (!l) return;
    const r = rectOf(l.model!, l.crop_n!);
    commit(l.model!, [(1 - r[2]) / 2, (1 - r[3]) / 2, r[2], r[3]]);
  };
  /** The focused crop at shape `a` (source px w/h), its width kept where it fits, around
   * its centre. */
  const toRatio = (a: number) => {
    const l = focused();
    if (!l) return;
    const r = rectOf(l.model!, l.crop_n!);
    const [sw, sh] = l.src_size;
    let w = r[2];
    let h = (w * sw) / a / sh;
    if (h > 1) {
      h = 1;
      w = (h * sh * a) / sw;
    }
    commit(l.model!, [r[0] + r[2] / 2 - w / 2, r[1] + r[3] / 2 - h / 2, w, h]);
  };
  const RATIOS: [string, number][] = [["16:9", 16 / 9], ["4:3", 4 / 3], ["1:1", 1], ["4:5", 4 / 5], ["9:16", 9 / 16]];
  const ratioNow = () => {
    const l = focused();
    if (!l) return "";
    const a = shapeOf(l, rectOf(l.model!, l.crop_n!));
    return RATIOS.find(([, v]) => Math.abs(v / a - 1) < 0.01)?.[0] ?? "";
  };
  /** The other parts cut from the same video as this one. */
  const sameSource = () => {
    const v = cutView();
    const p = props.part;
    if (!v || !p) return 0;
    return new Set(v.parts.filter((q) => q.source === p.source && q.id !== p.id).map((q) => q.id)).size;
  };
  const toSameSource = () => {
    const v = cutView();
    const p = props.part;
    if (!v || !p) return;
    void editing(attempt(() => actions.layoutCopySameSource(v.project, v.cut, p.id))).then((r) => {
      if (r) say(`This part's crops and layout are on ${r.parts} more part${r.parts === 1 ? "" : "s"} from the same video — ⌘Z undoes it`);
    });
  };
  /** As large as the picture allows at the same shape, around its centre. */
  const biggest = (keepCentre: boolean) => {
    const l = focused();
    if (!l) return;
    const r = rectOf(l.model!, l.crop_n!);
    const a = shapeOf(l, r);
    const [sw, sh] = l.src_size;
    let w = 1;
    let h = (sw / a) / sh;
    if (h > 1) {
      h = 1;
      w = (sh * a) / sw;
    }
    const cx = keepCentre ? r[0] + r[2] / 2 : 0.5;
    const cy = keepCentre ? r[1] + r[3] / 2 : 0.5;
    commit(l.model!, [cx - w / 2, cy - h / 2, w, h]);
  };
  // arrow keys nudge the focused crop by a source pixel (⇧: ten), one write when they stop
  let nudge: { layer: string; r: R; t: number } | null = null;
  const onKey = (e: KeyboardEvent) => {
    const l = focused();
    const d = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[e.key];
    const t = e.target as HTMLElement | null;
    if (!d || !l || e.metaKey || e.ctrlKey || e.altKey || drag || t?.closest("input, textarea, select, [contenteditable]")) return;
    e.preventDefault();
    e.stopPropagation();
    const k = e.shiftKey ? 10 : 1;
    const base = nudge?.layer === l.model ? nudge.r : rectOf(l.model!, l.crop_n!);
    const r = clampRect([base[0] + (d[0]! * k) / l.src_size[0], base[1] + (d[1]! * k) / l.src_size[1], base[2], base[3]]);
    if (nudge) clearTimeout(nudge.t);
    setLive({ layer: l.model!, r });
    nudge = { layer: l.model!, r, t: window.setTimeout(() => { nudge = null; commit(l.model!, r); }, 350) };
  };
  createEffect(() => {
    addEventListener("keydown", onKey, true);
    onCleanup(() => removeEventListener("keydown", onKey, true));
  });
  /** The dim outside every crop: the whole picture with a hole per box. */
  const dimPath = () => {
    const W = props.rect.w;
    const H = props.rect.h;
    let d = `M0 0H${W}V${H}H0Z`;
    for (const l of layers()) {
      const r = rectOf(l.model!, l.crop_n!);
      const [x, y, w, h] = [r[0] * W, r[1] * H, r[2] * W, r[3] * H];
      d += `M${x} ${y}V${y + h}H${x + w}V${y}Z`;
    }
    return d;
  };
  const rectOf = (layer: string, r: R): R => {
    const lv = live();
    return lv && lv.layer === layer ? lv.r : r;
  };

  const down = (e: PointerEvent, layer: string, r: R, mode: Mode) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    // Shape in display pixels (= source pixels up to one scale).
    const v = cutView();
    const held = v && props.part ? hold(v.project, "layer", layer, props.part.id) : null;
    drag = { layer, mode, x: e.clientX, y: e.clientY, r0: r, aspect: (r[2] * props.rect.w) / Math.max(1e-6, r[3] * props.rect.h), held };
    setFocus(layer);
    setDragging(true);
    setLive({ layer, r });
  };
  const move = (e: PointerEvent) => {
    if (!drag) return;
    const dx = (e.clientX - drag.x) / props.rect.w;
    const dy = (e.clientY - drag.y) / props.rect.h;
    const [x, y, w, h] = drag.r0;
    let r: R;
    if (drag.mode === "move") r = [x + dx, y + dy, w, h];
    else if (typeof drag.mode === "string") {
      // an edge: only that side moves, so the shape changes with it
      const m = drag.mode;
      const min = 0.03;
      if (m === "w") { const nx = Math.min(x + w - min, Math.max(0, x + dx)); r = [nx, y, x + w - nx, h]; }
      else if (m === "e") r = [x, y, Math.max(min, Math.min(1 - x, w + dx)), h];
      else if (m === "n") { const ny = Math.min(y + h - min, Math.max(0, y + dy)); r = [x, ny, w, y + h - ny]; }
      else r = [x, y, w, Math.max(min, Math.min(1 - y, h + dy))];
    } else {
      // The opposite corner stays put; the width follows the pointer, the height the shape.
      const left = drag.mode === 0 || drag.mode === 2;
      const top = drag.mode === 0 || drag.mode === 1;
      let nw = Math.max(0.03, left ? w - dx : w + dx);
      let nh = (nw * props.rect.w) / drag.aspect / props.rect.h;
      if (nh > 1) {
        nh = 1;
        nw = (nh * props.rect.h * drag.aspect) / props.rect.w;
      }
      if (nw > 1) {
        nw = 1;
        nh = (nw * props.rect.w) / drag.aspect / props.rect.h;
      }
      r = [left ? x + w - nw : x, top ? y + h - nh : y, nw, nh];
    }
    setLive({ layer: drag.layer, r: clampRect(r) });
  };
  const up = () => {
    setDragging(false);
    const d = drag;
    const lv = live();
    drag = null;
    const v = cutView();
    const p = props.part;
    if (!d || !lv || !v || !p) {
      d?.held?.cancel();
      setLive(null);
      return;
    }
    const same = lv.r.every((n, i) => Math.abs(n - d.r0[i]!) < 1e-4);
    if (same || !d.held) {
      d.held?.cancel();
      setLive(null);
      return;
    }
    // the preview holds until the cut is read back with the new crop (letting go at the
    // reply drew the old crop for a moment), and only this drag's: a drag begun since
    // keeps its own
    void editing(attempt(() => d.held!.commit((g) => actions.cropNorm(v.project, v.cut, p.id, d.layer, lv.r, g, groupFor(p.id, d.layer))).then(() => true)))
      .then((ok) => (ok ? refreshCut() : undefined))
      .finally(() => {
        if (live() === lv) setLive(null);
      });
  };

  return (
    <div
      class="crop-overlay"
      data-testid="crop-overlay"
      style={{ left: `${props.rect.x}px`, top: `${props.rect.y}px`, width: `${props.rect.w}px`, height: `${props.rect.h}px` }}
      onPointerMove={move}
      onPointerUp={up}
      onPointerCancel={cancel}
    >
      <Show when={!layers().length}>
        <p class="crop-empty">Pick a part with a layout to crop.</p>
      </Show>
      <svg class="crop-dim" width={props.rect.w} height={props.rect.h} aria-hidden="true">
        <path d={dimPath()} fill-rule="evenodd" />
      </svg>
      <Show when={layers().length}>
        <div class="crop-tools" role="toolbar" aria-label="Crop tools" data-testid="crop-tools" data-tip="Drag to move · corners resize · edges change the shape · arrows nudge (⇧ ×10) · Esc done" onPointerDown={(e) => e.stopPropagation()}>
          <For each={all()}>
            {(l) => (
              <button type="button" class={`crop-chip role-${l.role}`} aria-pressed={focused()?.model === l.model} data-testid="crop-chip" data-layer={l.model!} onClick={() => setFocus(l.model)}>
                {roleName(l.role)}
              </button>
            )}
          </For>
          <Show when={group()}>
            {(g) => (
              <span class="crop-group" data-testid="crop-group" data-tip="Grouped on the timeline: this crop is set on every layer of the group (one undo)">
                {roleName(focused()!.role)} on {g().length} parts
              </span>
            )}
          </Show>
          <span class="crop-sep" />
          <For each={RATIOS}>
            {([name, a]) => (
              <button type="button" class="crop-tool crop-ratio" aria-pressed={ratioNow() === name} data-testid={`crop-ratio-${name.replace(":", "x")}`} data-tip={`Make the crop ${name}`} onClick={() => toRatio(a)}>{name}</button>
            )}
          </For>
          <span class="crop-sep" />
          <button type="button" class="crop-tool" data-testid="crop-centre" data-tip="Centre it on the picture" onClick={centre}>Centre</button>
          <button type="button" class="crop-tool" data-testid="crop-biggest" data-tip="As large as the picture allows, same shape, around its centre" onClick={() => biggest(true)}>Biggest</button>
          <button type="button" class="crop-tool" data-testid="crop-reset" data-tip="The whole picture at its shape, centred" onClick={() => biggest(false)}>Reset</button>
          <Show when={sameSource()}>
            <span class="crop-sep" />
            <button type="button" class="crop-tool crop-all" data-testid="crop-same-source" data-tip="Give every part from this video these crops and this layout (one undo)" onClick={toSameSource}>
              Use on all {sameSource() + 1} parts of this video
            </button>
          </Show>
          <span class="crop-hint">Drag to move · corners resize · edges change the shape · arrows nudge (⇧ ×10) · Esc done</span>
        </div>
      </Show>
      <For each={layers()}>
        {(l) => {
          const r = () => rectOf(l.model!, l.crop_n!);
          return (
            <div
              class={`crop-box role-${l.role}${focused()?.model === l.model ? " focused" : ""}`}
              data-layer={l.model!}
              data-role={l.role}
              aria-label={`${l.role} crop`}
              style={{
                // smaller boxes above larger ones, so one inside another stays reachable
                "z-index": 2 + layers().filter((o) => o.crop_n![2] * o.crop_n![3] > l.crop_n![2] * l.crop_n![3]).length,
                left: `${r()[0] * 100}%`,
                top: `${r()[1] * 100}%`,
                width: `${r()[2] * 100}%`,
                height: `${r()[3] * 100}%`,
              }}
              onPointerDown={(e) => down(e, l.model!, r(), "move")}
            >
              <span class="crop-label">{roleName(l.role)} <small>{px(l, r())} px</small></span>
              <Show when={dragging() && live()?.layer === l.model}>
                <span class="crop-thirds" aria-hidden="true" />
              </Show>
              <For each={[0, 1, 2, 3] as const}>
                {(k) => <span class={`crop-grip g${k}`} data-grip={k} onPointerDown={(e) => down(e, l.model!, r(), k)} />}
              </For>
              <For each={["n", "e", "s", "w"] as const}>
                {(k) => <span class={`crop-edge e-${k}`} data-edge={k} onPointerDown={(e) => down(e, l.model!, r(), k)} />}
              </For>
            </div>
          );
        }}
      </For>
    </div>
  );
}
