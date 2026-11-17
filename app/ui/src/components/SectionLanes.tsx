// Zoom and layout sections as lanes above the parts (decision 0046: "drag sections like
// screen studio on top of parts"). Each section is a bar on the cut's clock, free of part
// edges: drag its body to move it, an edge to resize it (snapping to part edges, the
// playhead and other sections' edges; ⌥ off), click to select it (its controls open in
// the inspector), ⌫ removes it, a double-click on an empty stretch adds a 2 s zoom or a
// layout section there. Automatic zooms (`zoom_auto`) are dashed until edited. Every
// release is one `section_set` (one undo step). They follow the timeline's mapping
// (`tlView`), as the layer rows do.
import { createEffect, createSignal, For, on, Show } from "solid-js";
import { rate, studioActions, type SectionView } from "../actions";
import { attempt, cutView, editing, say, selectedPart, setSelectedPart, snapping, tlView, viewer } from "../store";
import { showTab } from "../layout";
import { Icon } from "../design/Icon";
import "./sections.css";
import { cutSections, hasRecording, refreshSections, sectionLanesShown, selectedSection, setSelectedSection } from "../sectionsState";

const SNAP_PX = 6;
const EDGE_PX = 6;
const MIN_MS = 100;
const NEW_ZOOM_MS = 2000;

type Lane = "zoom" | "layout";
const laneOf = (s: SectionView): Lane => ("zoom" in s.kind ? "zoom" : "layout");

/** A bar being dragged: which edge (or the body), where the press began, its span then. */
interface BarDrag {
  id: string;
  side: "in" | "out" | "body";
  x0: number;
  span: [number, number];
  now: [number, number];
  moved: boolean;
}

export function SectionLanes() {
  // read again whenever the cut (or its rev) moves: undo, redo and agents' edits included
  createEffect(on(() => [cutView()?.cut, cutView()?.rev], () => void refreshSections()));
  // a part picked elsewhere takes the inspector (and ⌫) back
  createEffect(on(selectedPart, (p) => p && setSelectedSection(null), { defer: true }));

  const fps = () => rate(cutView()?.fps);
  const msToX = (ms: number) => {
    const t = tlView();
    return t ? 12 + (ms / 1000) * fps() * t.ppf - t.scroll : 0;
  };
  const xToMs = (x: number) => {
    const t = tlView();
    return t && t.ppf > 0 ? (((x - 12 + t.scroll) / t.ppf) / fps()) * 1000 : 0;
  };
  /** CSS px per ms at the timeline's zoom. */
  const pxPerMs = () => ((tlView()?.ppf ?? 0) * fps()) / 1000;
  const cutMs = () => cutSections()?.cutMs ?? ((cutView()?.frames ?? 0) / fps()) * 1000;

  const [drag, setDrag] = createSignal<BarDrag | null>(null);
  const spanOf = (s: SectionView): [number, number] | null => {
    const d = drag();
    if (d && d.id === s.id) return d.now;
    return s.start_ms == null || s.end_ms == null ? null : [s.start_ms, s.end_ms];
  };
  const placed = (lane: Lane) => (cutSections()?.list ?? []).filter((s) => laneOf(s) === lane && s.start_ms != null && s.end_ms != null);

  /** Part edges, the playhead and the other sections' edges (ms). */
  const snapTargets = (except: string): number[] => {
    const v = cutView();
    if (!v) return [];
    const out: number[] = [0, cutMs()];
    for (const p of v.parts) out.push((p.out[0] / fps()) * 1000, (p.out[1] / fps()) * 1000);
    const f = viewer()?.frame;
    if (f !== undefined) out.push((f / fps()) * 1000);
    for (const s of cutSections()?.list ?? []) if (s.id !== except && s.start_ms != null && s.end_ms != null) out.push(s.start_ms, s.end_ms);
    return out;
  };
  /** How far `ms` moves to its nearest target within a few px (0 when none, or ⌥). */
  const snapBy = (ms: number, targets: number[], alt: boolean): number | null => {
    if (alt || !snapping() || pxPerMs() <= 0) return null;
    let best: number | null = null;
    for (const t of targets) {
      const d = t - ms;
      if (Math.abs(d) * pxPerMs() <= SNAP_PX && (best === null || Math.abs(d) < Math.abs(best))) best = d;
    }
    return best;
  };

  const onBarDown = (e: PointerEvent, s: SectionView) => {
    if (e.button !== 0) return;
    const span = spanOf(s);
    if (!span) return;
    e.stopPropagation();
    const el = e.currentTarget as HTMLElement;
    const r = el.getBoundingClientRect();
    const side = e.clientX - r.left < EDGE_PX ? "in" : r.right - e.clientX < EDGE_PX ? "out" : "body";
    el.setPointerCapture(e.pointerId);
    setDrag({ id: s.id, side, x0: e.clientX, span, now: span, moved: false });
  };
  const onBarMove = (e: PointerEvent) => {
    const d = drag();
    if (!d || pxPerMs() <= 0) return;
    if (!d.moved && Math.abs(e.clientX - d.x0) <= 3) return;
    const dms = (e.clientX - d.x0) / pxPerMs();
    const total = cutMs();
    const t = snapTargets(d.id);
    let [a, b] = d.span;
    if (d.side === "body") {
      const len = b - a;
      let na = Math.max(0, Math.min(total - len, a + dms));
      const sa = snapBy(na, t, e.altKey);
      const sb = snapBy(na + len, t, e.altKey);
      const by = sa === null ? sb : sb === null ? sa : Math.abs(sa) <= Math.abs(sb) ? sa : sb;
      if (by !== null) na = Math.max(0, Math.min(total - len, na + by));
      a = na;
      b = na + len;
    } else if (d.side === "in") {
      a = Math.max(0, Math.min(b - MIN_MS, a + dms));
      a += snapBy(a, t, e.altKey) ?? 0;
      a = Math.max(0, Math.min(b - MIN_MS, a));
    } else {
      b = Math.min(total, Math.max(a + MIN_MS, b + dms));
      b += snapBy(b, t, e.altKey) ?? 0;
      b = Math.min(total, Math.max(a + MIN_MS, b));
    }
    setDrag({ ...d, now: [Math.round(a), Math.round(b)], moved: true });
  };
  const onBarUp = (s: SectionView) => {
    const d = drag();
    const v = cutView();
    if (!d || !v) return setDrag(null);
    if (!d.moved) {
      setDrag(null);
      select(s);
      return;
    }
    const [a, b] = d.now;
    const args = { section: s.id, ...(d.side !== "out" ? { start_ms: a } : {}), ...(d.side !== "in" ? { end_ms: b } : {}) };
    setSelectedSection(s.id);
    void editing(attempt(() => studioActions.sectionSet(v.project, args)))
      .then(() => refreshSections())
      .finally(() => setDrag(null));
  };
  /** Selected: its controls in the inspector, the playhead inside it. */
  const select = (s: SectionView) => {
    setSelectedPart(null);
    setSelectedSection(s.id);
    showTab("look", "section-controls");
  };

  const [adding, setAdding] = createSignal(false);
  /** A double-click on an empty stretch of a lane: a 2 s zoom, or a layout section with the
   * framing's first layout (else the first built-in one the cut can make, made now). */
  const addAt = async (lane: Lane, e: MouseEvent) => {
    const v = cutView();
    if (!v || adding() || e.target !== e.currentTarget) return;
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const total = cutMs();
    const start = Math.max(0, Math.min(Math.max(0, total - MIN_MS), Math.round(xToMs(e.clientX - r.left))));
    const end = Math.min(total, start + NEW_ZOOM_MS);
    setAdding(true);
    try {
      if (lane === "zoom") {
        const made = await editing(attempt(() => studioActions.sectionAdd(v.project, { cut: v.cut, start_ms: start, end_ms: end, kind: { zoom: {} } })));
        if (made) {
          setSelectedPart(null);
          setSelectedSection(made.id);
          showTab("look", "section-controls");
          say("Zoom added · drag its edges, ⌘Z undoes");
        }
      } else {
        const framing = v.framing?.active;
        if (!framing) return say("This cut has no framing yet", "error");
        let layout = v.framing?.layouts[0]?.id;
        if (!layout) {
          const list = await attempt(() => studioActions.layoutBuiltinList(v.project, v.cut));
          const pick = list?.layouts.find((l) => l.available !== false);
          if (!pick) return say("No layout this cut can use", "error");
          const made = await editing(attempt(() => studioActions.layoutBuiltinAdd(v.project, { cut: v.cut, framing, name: pick.name })));
          layout = made?.id;
        }
        if (!layout) return;
        const id = layout;
        const made = await editing(attempt(() => studioActions.sectionAdd(v.project, { cut: v.cut, framing, start_ms: start, end_ms: end, kind: { layout: { layout: id } } })));
        if (made) {
          setSelectedPart(null);
          setSelectedSection(made.id);
          showTab("look", "section-controls");
          say("Layout section added · ⌘Z undoes");
        }
      }
    } finally {
      setAdding(false);
      void refreshSections();
    }
  };

  const [autoBusy, setAutoBusy] = createSignal(false);
  const autoZoom = async () => {
    const v = cutView();
    if (!v || autoBusy()) return;
    setAutoBusy(true);
    const r = await editing(attempt(() => studioActions.zoomAuto(v.project, v.cut)));
    setAutoBusy(false);
    void refreshSections();
    if (r) say(r.sections ? `Auto zoom: ${r.sections} zoom section${r.sections === 1 ? "" : "s"} from the input log (hand-made ones kept) · ⌘Z undoes` : "Auto zoom: nothing in the input log to zoom on");
  };

  const layoutName = (id: string) => cutView()?.framing?.layouts.find((l) => l.id === id)?.name ?? "Layout";
  const label = (s: SectionView) => {
    if ("zoom" in s.kind) {
      const z = s.kind.zoom;
      const t = z.target ?? "cursor";
      const on = t === "cursor" ? "cursor" : t === "focus" ? "focus" : "point";
      return `${z.depth ?? 2}× · ${on}`;
    }
    return layoutName(s.kind.layout.layout);
  };
  const isAuto = (s: SectionView) => "zoom" in s.kind && !!s.kind.zoom.auto;
  const lanes: { lane: Lane; name: string; icon: string }[] = [
    { lane: "zoom", name: "Zoom", icon: "zoom-in" },
    { lane: "layout", name: "Layout", icon: "layout" },
  ];

  return (
    <Show when={cutView() && sectionLanesShown()}>
      <section class="layer-tracks section-lanes" aria-label="Sections" data-testid="section-lanes" data-count={cutSections()?.list.length ?? 0}>
        <For each={lanes}>
          {(l, i) => (
            <div class={`lt-row sl-row sl-${l.lane}`} style={{ top: `${i() * 26}px` }} data-testid={`section-lane-${l.lane}`}>
              <div class="lt-head">
                <Icon name={l.icon} size={12} />
                <span class="lt-name">{l.name}</span>
                <Show when={l.lane === "zoom" && hasRecording()}>
                  <button type="button" class="lt-eye" aria-label="Auto zoom from the input log" data-tip="Auto zoom: zoom sections where you typed or clicked (from the recording's input log); earlier automatic ones are replaced, yours kept"
                    data-testid="zoom-auto" disabled={autoBusy()} aria-busy={autoBusy()} onClick={() => void autoZoom()}>
                    <Icon name={autoBusy() ? "loader" : "sparkles"} size={12} />
                  </button>
                </Show>
              </div>
              <div class="lt-body sl-body" data-tip={l.lane === "zoom" ? "Double-click to add a zoom here" : "Double-click to add a layout section here"} onDblClick={(e) => void addAt(l.lane, e)}>
                <For each={placed(l.lane)}>
                  {(s) => (
                    <div
                      role="button"
                      tabindex="0"
                      class={`sl-bar sl-${l.lane}${isAuto(s) ? " auto" : ""}${selectedSection() === s.id ? " sel" : ""}`}
                      data-testid="section-bar"
                      data-id={s.id}
                      data-kind={l.lane}
                      data-auto={isAuto(s) ? "1" : "0"}
                      data-start-ms={spanOf(s)?.[0]}
                      data-end-ms={spanOf(s)?.[1]}
                      aria-label={`${l.name} section ${label(s)}, ${((spanOf(s)?.[0] ?? 0) / 1000).toFixed(1)}–${((spanOf(s)?.[1] ?? 0) / 1000).toFixed(1)} s${isAuto(s) ? " (automatic)" : ""}`}
                      aria-pressed={selectedSection() === s.id}
                      style={{ left: `${msToX(spanOf(s)![0])}px`, width: `${Math.max(4, (spanOf(s)![1] - spanOf(s)![0]) * pxPerMs())}px` }}
                      onPointerDown={(e) => onBarDown(e, s)}
                      onPointerMove={onBarMove}
                      onPointerUp={() => onBarUp(s)}
                      onPointerCancel={() => setDrag(null)}
                      onDblClick={(e) => e.stopPropagation()}
                      onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); select(s); } }}
                    >
                      <span>{label(s)}</span>
                    </div>
                  )}
                </For>
              </div>
            </div>
          )}
        </For>
        <Show when={viewer()}>
          {(vw) => <span class="lt-playhead" style={{ left: `calc(var(--lane-header-w) + ${msToX((vw().frame / fps()) * 1000)}px)` }} />}
        </Show>
      </section>
    </Show>
  );
}
