// The timeline (SPEC §5.3): one canvas with its own hit-testing — ruler, parts lane
// (with the audio's energy and quiet regions under each part), caption lane, playhead —
// plus overlay lanes (the plan's text hooks, pills and clocks, greedy-packed), marks
// (⌘-click) and a range (⇧-click; ⌥L loops it), hover scrub on the ruler (scrub steps
// show the scrub proxy, then the exact frame once the pointer rests: H.05), filmstrip
// thumbnails from the scrub proxy under each part,
// plus an accessible list mirror. Trims drag a part edge, snap to word edges (⌥ off,
// frames only) and release as one `cut_trim`; dragging a part's body moves it (one
// `part_reorder`); a sweep's preview shows as red spans. Zoom: fit, `-`/`=` (×1.6,
// playhead held), ⌥-wheel. Moving through time without a sideways wheel: the wheel over
// the ruler and parts always scrolls time (below them it scrolls overflowing lanes first),
// ⇧-wheel scrolls time anywhere, a middle-button drag pans, and the scrollbar under the
// lanes drags (a click on its track jumps there).
//
// Editable lanes (U.08): a model cut's overlays drag (body moves, edges trim, snapping to
// part and word edges, ⌥ off; lanes re-pack live) and the gain-envelope lane (click to
// add a segment at that level, drag its body to move it and change its level, its edges
// to re-time it, double-click removes it). Each drag is a human gesture held in the core
// from pointer down and commits one command (one undo step) on release; Esc cancels.
// `m` / `⇧m` mark a measurement's start / end at the playhead; measurements show as
// brackets with their value above the parts.
import { createEffect, createMemo, createResource, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { commentMarks } from "../collab";
import { actions, rate, type CutColumns, type CutMarker, type CutView, type PartView, type TimelineLanes } from "../actions";
import { analysisTick, attempt, cutView, openCut, editing, markIn, markOut, markedRange, say, selectedEdge, setMarkIn, setMarkOut, setTimelineDropHook, setTlHover, setTlView, lanesTick, loopAsk, mattesTick, measureFrom, proxyTick, selectedPart, setSelectedPart, setSnapping, setTlTool, snapping, sweepRibbons, tlTool, viewer } from "../store";
import { Chip, IconButton, Toggle, Toolbar, ToolbarGroup, ToolbarSep } from "../design/components";
import { trData } from "../transcriptState";
import { pack } from "../lanes";
import { prefs, setPref, toggleHoverScrub } from "../prefs";
import { isAddClick } from "../platform";
import { canGroup, canUngroup, clearPicked, groupSelected, pickedParts, pickPart, selection, setPickedParts, ungroupSelected, withGroups } from "../partGroups";
import { Icon } from "../design/Icon";
import { LayerTracks } from "./LayerTracks";
import { SectionLanes } from "./SectionLanes";
import { cutMarkers, sectionLanesShown, setSectionLanesOn } from "../sectionsState";
import { timecode } from "./Viewer";

const RULER = 24;
/** Measurement brackets (a ribbon between the ruler and the parts). */
const MEAS_Y = 26;
const PARTS_Y = 42;
const PARTS_H = 70;
/** Inside a part: the filmstrip on top, the waveform in the bottom band. */
const WAVE_H = 24;
/** The gain-envelope lane: 0 dB in the middle, ±GAIN_RANGE dB at its edges. */
const GAIN_Y = PARTS_Y + PARTS_H + 6;
const GAIN_H = 30;
const GAIN_RANGE = 24;
const CAP_Y = GAIN_Y + GAIN_H + 6;
const CAP_H = 22;
const OV_Y = CAP_Y + CAP_H + 8;
const OV_H = 18;
const OV_GAP = 4;
const EDGE = 7;
/** Filmstrip thumbnail width (px, CSS) and the drawn height inside a part. */
const THUMB_W = 96;
/** Lane rows for the sticky header column (canvas y, height). */
const LANES: { name: string; icon: string; y: number; h: number }[] = [
  { name: "Video", icon: "film", y: PARTS_Y, h: PARTS_H },
  { name: "Gain", icon: "activity", y: GAIN_Y, h: GAIN_H },
  { name: "Captions", icon: "captions", y: CAP_Y, h: CAP_H },
  { name: "Text", icon: "type", y: OV_Y, h: OV_H },
];
const MONO = "10px ui-monospace, SFMono-Regular, Menlo, monospace";
const UI = (w: number, px: number) => `${w} ${px}px -apple-system, BlinkMacSystemFont, system-ui, sans-serif`;
/** Ruler steps (s) and their minor divisions; the first whose major spacing is ≥ 72 px. */
const STEPS: [number, number][] = [[0.5, 5], [1, 5], [2, 4], [5, 5], [10, 5], [15, 3], [30, 6], [60, 6], [120, 4], [300, 5], [600, 5], [1800, 6]];
/** A recording marker's output frame. */
const markerFrame = (ms: number, fps: number) => Math.round((ms / 1000) * fps);
/** A recording marker's tooltip: which one, and when on the cut's clock. */
const markerTip = (m: CutMarker) => {
  const s = m.cut_ms / 1000;
  return `Marker ${m.n} · ${Math.floor(s / 60)}:${(s % 60).toFixed(1).padStart(4, "0")}`;
};
function rr(g: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number) {
  g.beginPath();
  if (w < 2 * r + 1 || !("roundRect" in g)) g.rect(x, y, w, h);
  else g.roundRect(x, y, w, h, r);
}

type Lanes = TimelineLanes;
type Ov = { id: string; role: string | null; text: string; out: [number, number]; kind?: Lanes["overlays"][number]["kind"]; under?: number | null; matte?: boolean };
/** A picture overlay's bar label: what it is, and where it sits (decision 0042). */
const pictureLabel = (o: Ov) =>
  `${o.kind === "video" ? "Video" : "Image"}${o.matte ? " · cut out" : ""}${o.under != null ? " · behind" : ""}`;
type Gain = Lanes["gains"][number];
/** A drag on an overlay or a gain segment: which side (`body` moves), the pointer's
 * start, the change so far (frames; dB for a gain body) and the core gesture holding it. */
type LaneDrag = {
  kind: "overlay" | "gain";
  id: string;
  side: "in" | "out" | "body";
  out: [number, number];
  db: number;
  x0: number;
  y0: number;
  df: number;
  ddb: number;
  moved: boolean;
  gesture: Promise<number | null>;
};
type Drag =
  | { part: PartView; edge: "in" | "out"; x0: number; delta: number; snapped: "" | "word" | "onset" }
  | { scrub: true }
  | { pan: true; x0: number; s0: number }
  /** A band drawn across the parts (⇧ / ⌘ drag on them, or a drag in any empty space below
   * the ruler): selects every part it covers; ⌘ adds to what was selected. `on` is the
   * part it started on (a click without a drag picks it as before); `seek`: a click
   * without a drag seeks there, as the press used to. */
  | { band: { x0: number; x: number; add: boolean; run: boolean; base: string[]; on: string | null; moved: boolean; seek?: boolean } }
  | { move: PartView; ids: string[]; x0: number; x: number; active: boolean }
  | { lane: LaneDrag };
/** A part group's colour (decision 0037): its id hashed onto the colour wheel. */
function groupColour(id: string): string {
  let h = 0;
  for (const c of id) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return `hsl(${h % 360} 70% 58%)`;
}
/** How close (px) a dragged edge must come to a word edge to snap to it. */
const SNAP_PX = 6;

// Colour tokens, read once and again when the colour scheme changes.
let tokens: Record<string, string> = {};
function css(name: string): string {
  tokens[name] ??= getComputedStyle(document.documentElement).getPropertyValue(name).trim() || "#888";
  return tokens[name]!;
}

/** Gain limits the core takes (dB). */
const GAIN_MIN = -40;
const GAIN_MAX = 24;
const clampDb = (db: number) => Math.max(GAIN_MIN, Math.min(GAIN_MAX, Math.round(db * 2) / 2));

/** A measurement's value as the bracket shows it ("4.20 s", "1:07.4"). */
function fmtMeasure(value: number, unit: string): string {
  if (unit !== "s") return `${value} ${unit}`;
  if (value < 60) return `${value.toFixed(2)} s`;
  const m = Math.floor(value / 60);
  return `${m}:${(value - m * 60).toFixed(1).padStart(4, "0")}`;
}

/** The nearest value of sorted `xs` to `want`, if within `max`. */
function nearest(xs: number[], want: number, max: number): number | null {
  let lo = 0;
  let hi = xs.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (xs[mid]! < want) lo = mid + 1;
    else hi = mid;
  }
  let best: number | null = null;
  for (const k of [lo - 1, lo]) {
    const x = xs[k];
    if (x !== undefined && Math.abs(x - want) <= max && (best === null || Math.abs(x - want) < Math.abs(best - want))) best = x;
  }
  return best;
}

/** Frame-time instrumentation for the R-TL-1 perf spec: present only when the page set
 * `__GMCLIP_TL_PERF__` before load (never in normal use). */
interface TlPerf {
  draws: number[];
  /** D.02: per columns load, the fetch + decode (ms) and request → visible window drawn. */
  columns?: { load: number; window: number; frames: number }[];
}

/** Frames at least this wide (px) show their sample peaks over the level. */
const PEAKS_PPF = 4;
/** Below this the energy column says nothing plays (`columns::SILENT_DB`, −120). */
const SILENT_DB = -119.5;

/** The audio lane as drawn (D.02): the `gm://data` columns, per output frame, or where
 * those cannot be read the `timeline_audio` JSON, per part. Onsets are snap targets. */
type AudioLane = { points: number; t0: number; load: number } & (
  | { src: "columns"; energy: Float32Array; quiet: Uint32Array; words: CutColumns["words"] }
  | { src: "json"; parts: { out: [number, number]; db: number[]; quiet: [number, number][] }[] }
);
const perf = (): TlPerf | undefined => (globalThis as unknown as { __GMCLIP_TL_PERF__?: TlPerf }).__GMCLIP_TL_PERF__;

/** The lanes' least height: every fixed lane and the first overlay row. A shorter
 * timeline (a window ≤ 600 px tall) scrolls its lanes vertically instead of clipping. */
const MIN_LANES_H = OV_Y + OV_H + 8;

export function Timeline() {
  let wrap!: HTMLDivElement;
  let body!: HTMLDivElement;
  let canvas!: HTMLCanvasElement;
  /** The ruler, pinned above the picture layers: a copy of the canvas's ruler band,
   * which is itself scrolled out of view above the lanes. */
  let ruler!: HTMLCanvasElement;
  /** Where a press began (the ruler or the lanes): it captures the pointer. */
  let pressEl: HTMLCanvasElement | null = null;
  /** The lanes' width (CSS px), tracked so the fit zoom and `tlView` follow resizes. */
  const [laneW, setLaneW] = createSignal(0);
  const [pxPerFrame, setPxPerFrame] = createSignal<number | null>(null); // null = fit
  const [scroll, setScroll] = createSignal(0);
  /** Vertical scroll of the overlay lanes (px), when there are more than fit. */
  const [ovScroll, setOvScroll] = createSignal(0);
  const [drag, setDrag] = createSignal<Drag | null>(null);
  /** The dragged part's level on its own source past its edges (decision 0053): what
   * lengthening it reveals, at the timeline's scale. */
  const [edgeLevels, setEdgeLevels] = createSignal<{ part: string; first: number; db: number[] } | null>(null);
  const [hover, setHover] = createSignal<string>("");
  /** The pointer's x over the timeline (null when away): a hover line and its time. With
   * hover scrub on, the viewer also follows it over the ruler; off, only the line shows. */
  const [hoverX, setHoverX] = createSignal<number | null>(null);
  // the hover frame for the cut keys (s, q, w), kept a moment after the pointer leaves
  createEffect(() => {
    const x = hoverX();
    const v = cutView();
    if (x !== null && v) setTlHover({ frame: Math.max(0, Math.min(v.frames, Math.round(frameAt(x)))), left: null });
    else setTlHover((h) => h && { ...h, left: h.left ?? performance.now() });
  });
  /** Marks (⌘-click), output frames; the range (⇧-click, or I and O). Session-only. */
  const [marks, setMarks] = createSignal<number[]>([]);
  /** The recording marker under the pointer in the ruler: its tooltip. */
  const [markerHover, setMarkerHover] = createSignal("");
  const range = markedRange;
  const setRange = (r: [number, number] | null) => {
    setMarkIn(r ? r[0] : null);
    setMarkOut(r ? r[1] : null);
  };
  /** The cut's own overlays, gain segments and measurements (output frames). */
  const [lanes, { refetch: refetchLanes }] = createResource(
    () => {
      const v = cutView();
      return v ? { p: v.project, c: v.cut, rev: v.rev, t: lanesTick() } : undefined;
    },
    async (k) => {
      try {
        return await actions.timelineLanes(k.p, k.c);
      } catch {
        return null;
      }
    },
  );
  const editable = () => lanes()?.overlays_editable ?? false;
  const gainsEditable = () => lanes()?.gains_editable ?? false;
  /** Hooks, pills and clocks: a model cut's own overlays (draggable), else the plan's text
   * events (an imported RenderPlan; captions stay in the caption lane). */
  // images and videos are picture layers: their own rows above (LayerTracks), not here
  const baseOverlays = createMemo<Ov[]>(() =>
    editable() ? (lanes()?.overlays ?? []).filter((o) => o.kind !== "video" && o.kind !== "image") : (cutView()?.captions ?? []).filter((c) => c.role !== "caption"),
  );
  /** Where a lane drag would put its overlay or gain segment now. */
  const laneSpan = (l: LaneDrag): [number, number] => {
    const total = cutView()?.frames ?? 0;
    let [a, b] = l.out;
    if (l.side === "body") {
      const len = b - a;
      a = Math.max(0, Math.min(total - len, a + l.df));
      b = a + len;
    } else if (l.side === "in") a = Math.max(0, Math.min(b - 1, a + l.df));
    else b = Math.min(total, Math.max(a + 1, b + l.df));
    return [a, b];
  };
  const laneDrag = (kind: "overlay" | "gain"): LaneDrag | null => {
    const d = drag();
    return d && "lane" in d && d.lane.kind === kind ? d.lane : null;
  };
  /** The overlays as drawn: a dragged one at its previewed span (lanes re-pack live). */
  const overlays = createMemo<Ov[]>(() => {
    const base = baseOverlays();
    const l = laneDrag("overlay");
    if (!l) return base;
    const span = laneSpan(l);
    return base.map((o) => (o.id === l.id ? { ...o, out: span } : o));
  });
  const packed = createMemo(() => pack(overlays()));
  /** The gain segments as drawn: a dragged one at its previewed span and level. */
  const gains = createMemo<Gain[]>(() => {
    const base = lanes()?.gains ?? [];
    const l = laneDrag("gain");
    if (!l) return base;
    const span = laneSpan(l);
    return base.map((g) => (g.id === l.id ? { ...g, out: span, gain_db: clampDb(l.db + l.ddb) } : g));
  });
  const [audio] = createResource(
    () => {
      const v = cutView();
      return v ? { p: v.project, c: v.cut, rev: v.rev, t: analysisTick() } : undefined;
    },
    async (k): Promise<AudioLane | null> => {
      const t0 = performance.now();
      // the columns (gm://data): the level, the quiet regions, the words; the sample
      // peaks only once frames are wide enough to show them (below)
      const cols = await actions.cutColumns(k.p, k.c, ["energy", "quiet", "words"]);
      if (cols?.energy) {
        const load = performance.now() - t0;
        let points = 0;
        for (let i = 0; i < cols.energy.length; i++) if (cols.energy[i]! > SILENT_DB) points++;
        return { src: "columns", energy: cols.energy, quiet: cols.quiet ?? new Uint32Array(0), words: cols.words, points, t0, load };
      }
      // no gm:// here: the JSON lane
      const j = await attempt(() => actions.timelineAudio(k.p, k.c));
      if (!j) return null;
      return { src: "json", parts: j.parts, points: j.parts.reduce((n, p) => n + p.db.length, 0), t0, load: performance.now() - t0 };
    },
  );
  /** Zoomed in to ≥ PEAKS_PPF px a frame, the waveform adds each frame's sample peaks
   * (the `peaks` column, min/max): asked for once per cut revision when first needed. */
  const [peaks] = createResource(
    () => {
      const v = cutView();
      return v && wave() && ppf() >= PEAKS_PPF ? { p: v.project, c: v.cut, rev: v.rev, t: analysisTick() } : undefined;
    },
    async (k) => (await actions.cutColumns(k.p, k.c, ["peaks"]))?.peaks ?? null,
  );
  /** Energy onsets near each part's edges (snap targets): asked beside the columns, never
   * holding up their drawing; the JSON lane carries its own. */
  const [onsetLane] = createResource(
    () => {
      const v = cutView();
      return v ? { p: v.project, c: v.cut, rev: v.rev, t: analysisTick() } : undefined;
    },
    async (k) => (await attempt(() => actions.timelineOnsets(k.p, k.c)))?.parts ?? null,
  );
  const onsetParts = (): { out: [number, number]; onsets?: number[] }[] => {
    const a = audio();
    return a?.src === "json" ? a.parts : (onsetLane() ?? []);
  };
  /** D.02: request → the visible window drawn from that lane (ms), once per lane. */
  const [windowMs, setWindowMs] = createSignal<number | null>(null);
  let windowFor: AudioLane | null = null;
  // Cut-out progress (R.01): accurate mask tracks build in the background on their own;
  // their parts show ticks, asked again every second while a build runs.
  const [matteTick, setMatteTick] = createSignal(0);
  const [mattes] = createResource(
    () => {
      const v = cutView();
      return v ? { p: v.project, c: v.cut, rev: v.rev, t: mattesTick() + matteTick() } : undefined;
    },
    async (k) => (await actions.matteProgress(k.p, k.c).catch(() => null)) ?? null,
  );
  createEffect(() => {
    if (mattes()?.build?.state !== "running") return;
    const t = setTimeout(() => setMatteTick((n) => n + 1), 1000);
    onCleanup(() => clearTimeout(t));
  });
  /** Filmstrip thumbnails: spacing is a power-of-two number of frames near one thumbnail
   * per THUMB_W px, so zooming refetches only when the bucket changes. */
  const stripEvery = () => {
    const f = Math.max(1, THUMB_W / Math.max(1e-6, ppf()));
    return 2 ** Math.ceil(Math.log2(f));
  };
  const [strip] = createResource(
    () => {
      const v = cutView();
      return v ? { p: v.project, c: v.cut, rev: v.rev, e: stripEvery(), t: proxyTick() } : undefined;
    },
    async (k) => (await actions.filmstrip(k.p, k.c, k.e, THUMB_W))?.thumbs ?? null,
  );
  // Open huge files instantly (lane K5): until a video's scrub proxy is built, scrubbing
  // and the filmstrip read the original's keyframes; the timeline's toolbar says so.
  const [proxies] = createResource(
    () => {
      const v = cutView();
      return v ? { p: v.project, t: proxyTick() } : undefined;
    },
    async (k) => (await actions.proxyStatus(k.p).catch(() => null))?.jobs ?? [],
  );
  const building = () => (proxies() ?? []).filter((j) => j.phase === "queued" || j.phase === "building");
  const proxyPct = () => {
    const b = building();
    const frames = b.reduce((n, j) => n + j.frames, 0);
    return frames ? Math.floor((100 * b.reduce((n, j) => n + Math.min(j.covered, j.frames), 0)) / frames) : 0;
  };

  /** Word edges of the cut, output frames (starts and ends of the words it plays), sorted:
   * from the words column, else the transcript pane's data. */
  const wordEdges = createMemo((): number[] => {
    const t = trData();
    const v = cutView();
    if (!v) return [];
    const a = audio();
    const cw = a?.src === "columns" ? a.words : null;
    if (cw?.length) {
      const out: number[] = [];
      for (const w of cw) out.push(w.frame, w.frame + Math.max(1, Math.round(((w.endMs - w.startMs) * rate(v.fps)) / 1000)));
      return out.sort((x, y) => x - y);
    }
    if (!t) return [];
    const out: number[] = [];
    t.frame.forEach((f, i) => {
      if (f < 0) return;
      out.push(f, f + Math.max(1, Math.round(((t.dur_ms[i] ?? 0) * rate(v.fps)) / 1000)));
    });
    return out.sort((a, b) => a - b);
  });
  /** Energy onsets around a part's edges (output frames on its own mapping), sorted. */
  const onsetsOf = (p: PartView): number[] =>
    onsetParts().find((ap) => ap.out[0] <= p.out[0] && p.out[0] < ap.out[1])?.onsets ?? [];
  /** What a dragged overlay or gain edge snaps to: part edges and word edges. */
  const snapTargets = createMemo((): number[] => {
    const s = new Set<number>(wordEdges());
    for (const p of cutView()?.parts ?? []) {
      s.add(p.out[0]);
      s.add(p.out[1]);
    }
    return [...s].sort((a, b) => a - b);
  });

  const fitPx = (v: CutView) => Math.max(0.05, (wrap.clientWidth - 24) / Math.max(1, v.frames));
  const maxScroll = () => {
    const v = cutView();
    return v ? Math.max(0, v.frames * ppf() + 24 - wrap.clientWidth) : 0;
  };
  const scrollTo = (x: number) => setScroll(Math.min(maxScroll(), Math.max(0, x)));
  const ppf = () => {
    const v = cutView();
    return pxPerFrame() ?? (v ? fitPx(v) : 1);
  };
  const xOf = (frame: number) => 12 + frame * ppf() - scroll();
  const frameAt = (x: number) => (x - 12 + scroll()) / ppf();
  const gainY = (db: number) => GAIN_Y + GAIN_H / 2 - (Math.max(-GAIN_RANGE, Math.min(GAIN_RANGE, db)) / GAIN_RANGE) * (GAIN_H / 2 - 2);
  const dbAt = (y: number) => clampDb(((GAIN_Y + GAIN_H / 2 - y) / (GAIN_H / 2 - 2)) * GAIN_RANGE);
  const ovMaxScroll = () => Math.max(0, OV_Y + packed().lanes * (OV_H + OV_GAP) - wrap.clientHeight - RULER + 4);
  const laneTop = (lane: number) => OV_Y + lane * (OV_H + OV_GAP) - ovScroll();

  const zoom = (factor: number) => zoomTo(ppf() * factor);
  // the topic lane (and anything else drawn beside the canvas) follows this mapping
  createEffect(() => {
    void laneW();
    void cutView()?.frames;
    setTlView({ ppf: ppf(), scroll: scroll() });
  });
  onCleanup(() => setTlView(null));
  /** ∿ on: the waveform band along the bottom of each part (off: the filmstrip fills it). */
  const wave = () => prefs.waveform !== false;
  const thumbH = () => (wave() ? PARTS_H - WAVE_H : PARTS_H);

  /** Zoom to `next` px per frame, the playhead held where it is on screen. */
  const zoomTo = (next: number) => {
    const v = cutView();
    if (!v) return;
    const head = viewer()?.frame ?? 0;
    const before = xOf(head);
    const z = Math.min(4000 / rate(v.fps), Math.max(fitPx(v), next));
    setPxPerFrame(z <= fitPx(v) + 1e-9 ? null : z);
    scrollTo(12 + head * ppf() - before);
  };
  /** The zoom slider: 0 = fit … 1000 = 4000 px/s, on a log scale. */
  const sliderOf = () => {
    const v = cutView();
    if (!v) return 0;
    const lo = Math.log(fitPx(v));
    const hi = Math.log(4000 / rate(v.fps));
    return hi <= lo ? 0 : Math.round((1000 * (Math.log(ppf()) - lo)) / (hi - lo));
  };
  const fromSlider = (k: number) => {
    const v = cutView();
    if (!v) return;
    const lo = Math.log(fitPx(v));
    const hi = Math.log(4000 / rate(v.fps));
    zoomTo(Math.exp(lo + ((hi - lo) * k) / 1000));
  };

  /** The selected part and the next one, when they play on in the same source (merge). */
  const mergePair = (): [string, string] | null => {
    const v = cutView();
    const id = selectedPart();
    if (!v || !id) return null;
    const ids = [...new Set(v.parts.map((p) => p.id))];
    const k = ids.indexOf(id);
    if (k < 0 || k + 1 >= ids.length) return null;
    const a = v.parts.find((p) => p.id === id)!;
    const b = v.parts.find((p) => p.id === ids[k + 1])!;
    return a.source === b.source && a.src[1] === b.src[0] ? [a.id, b.id] : null;
  };
  const merge = async () => {
    const v = cutView();
    const m = mergePair();
    if (!v || !m) return;
    if (await attempt(() => actions.partMerge(v.project, m[0], m[1]))) say("Parts merged · ⌘Z undoes");
  };

  /** ⌥L: loop the range, else the part under the playhead. */
  const loop = () => {
    const v = cutView();
    if (!v) return;
    const f = viewer()?.frame ?? 0;
    const span = range() ?? v.parts.find((p) => p.out[0] <= f && f < p.out[1])?.out ?? null;
    if (!span) return;
    const spf = 48000 / rate(v.fps);
    void attempt(() => actions.viewerPlayRange(span[0] * spf, span[1] * spf, true));
  };

  /** Where parts dragged to x would land: their index among the other parts. */
  const dropIndex = (x: number, ids: string[]): number => {
    const v = cutView();
    if (!v) return 0;
    const others = v.parts.filter((p) => !ids.includes(p.id));
    let k = 0;
    for (const p of others) if (x > (xOf(p.out[0]) + xOf(p.out[1])) / 2) k++;
    return k;
  };

  /** Live slide (SPEC §5.3 "reorder with live slide"): while a part is dragged, the
   * others slide to where they would land; offsets (frames) ease toward their targets. */
  const slide = new Map<string, number>();
  let slideRaf = 0;
  /** Each part's offset (frames) toward the order a drop at `x` would make: the moving
   * parts (a selection or a group, decision 0037) land back to back. */
  const slideTargets = (v: CutView, ids: string[], x: number): Map<string, number> => {
    const others = v.parts.filter((p) => !ids.includes(p.id));
    const block = v.parts.filter((p) => ids.includes(p.id));
    const slot = dropIndex(x, ids);
    const order = [...others.slice(0, slot), ...block, ...others.slice(slot)];
    const out = new Map<string, number>();
    let at = 0;
    for (const p of order) {
      out.set(p.id, at - p.out[0]);
      at += p.out[1] - p.out[0];
    }
    return out;
  };

  const draw = () => {
    drawLanes();
    // the ruler band, playhead head and marks included, onto the pinned ruler
    const dpr = devicePixelRatio;
    if (ruler.width !== canvas.width || ruler.height !== Math.round(RULER * dpr)) {
      ruler.width = canvas.width;
      ruler.height = Math.round(RULER * dpr);
      ruler.style.width = canvas.style.width;
      ruler.style.height = `${RULER}px`;
    }
    ruler.getContext("2d")?.drawImage(canvas, 0, 0, canvas.width, ruler.height, 0, 0, ruler.width, ruler.height);
  };
  const drawLanes = () => {
    const t0 = performance.now();
    const v = cutView();
    const dpr = devicePixelRatio;
    const w = wrap.clientWidth;
    // the canvas starts RULER px above the wrap: its ruler band shows in the pinned ruler
    const h = wrap.clientHeight + RULER;
    // Resize the backing store only when the box changed (a resize clears and reallocates).
    if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
      canvas.style.width = `${w}px`;
      canvas.style.height = `${h}px`;
    }
    const g = canvas.getContext("2d");
    if (!g) return;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.fillStyle = css("--surface-sunken");
    g.fillRect(0, 0, w, h);
    // The ruler band and the hairlines between lanes.
    g.fillStyle = css("--surface-panel");
    g.fillRect(0, 0, w, RULER);
    g.fillStyle = css("--sep");
    g.fillRect(0, RULER, w, 1);
    g.fillRect(0, GAIN_Y - 3, w, 1);
    g.fillRect(0, CAP_Y - 3, w, 1);
    g.fillRect(0, OV_Y - 4, w, 1);
    if (!v) return;
    const fps = rate(v.fps);
    const pf = ppf();
    const x0cut = xOf(0);
    const x1cut = xOf(v.frames);
    // Ruler: major ticks with their time, minor ticks between (the first step whose
    // major spacing is at least 72 px).
    const pxPerSec = pf * fps;
    const [step, minor] = STEPS.find(([s]) => s * pxPerSec >= 72) ?? STEPS[STEPS.length - 1]!;
    const sub = step / minor;
    g.font = MONO;
    const s0 = Math.max(0, Math.floor(frameAt(-60) / fps / step) * step);
    for (let s = s0; s <= v.frames / fps + 1e-9; s += sub) {
      const x = Math.round(xOf(s * fps));
      if (x > w + 60) break;
      const major = Math.abs(s / step - Math.round(s / step)) < 1e-6;
      g.fillStyle = css(major ? "--ruler-tick" : "--ruler-tick-minor");
      g.fillRect(x, major ? RULER - 9 : RULER - 4, 1, major ? 9 : 4);
      if (major) {
        g.fillStyle = css("--text-3");
        const sec = Math.round(s * 100) / 100;
        const m = Math.floor(sec / 60);
        const ss = sec - m * 60;
        g.fillText(`${m}:${ss < 10 ? "0" : ""}${Number.isInteger(ss) ? ss : ss.toFixed(1)}`, x + 4, 11);
      }
    }
    // Parts: rounded clips, the filmstrip tiled along the top, the waveform along the
    // bottom band, the label on a scrim; selection is the accent.
    const d = drag();
    const lane = audio();
    const thumbs = strip();
    const hx = hoverX();
    // live slide: the others ease toward where a drop here puts them
    const sliding = d && "move" in d && d.active ? d : null;
    if (sliding) {
      const want = slideTargets(v, sliding.ids, sliding.x);
      let settled = true;
      for (const [id, t] of want) {
        const cur = slide.get(id) ?? 0;
        const next = Math.abs(t - cur) * pf < 0.5 ? t : cur + (t - cur) * 0.35;
        if (next !== t) settled = false;
        slide.set(id, next);
      }
      if (!settled && !slideRaf) slideRaf = requestAnimationFrame(() => { slideRaf = 0; draw(); });
    } else slide.clear();
    for (const [pi, p] of v.parts.entries()) {
      let a = p.out[0];
      let b = p.out[1];
      if (d && "part" in d && d.part.id === p.id) {
        if (d.edge === "in") a = Math.min(b - 1, a + d.delta);
        else b = Math.max(a + 1, b + d.delta);
      }
      const moving = !!sliding?.ids.includes(p.id);
      // a sliding part moves by its eased offset; the dragged ones follow the pointer
      const off = sliding ? (moving ? Math.round((sliding.x - sliding.x0) / pf) : (slide.get(p.id) ?? 0)) : 0;
      a += off;
      b += off;
      const x0 = xOf(a);
      const x1 = xOf(b);
      if (x1 < -2 || x0 > w + 2) continue;
      if (moving) g.globalAlpha = 0.6;
      const sel = selectedPart() === p.id || pickedParts().has(p.id);
      const pw = Math.max(1, x1 - x0 - 1);
      const px = x0 + 0.5;
      const wide = pw > 6;
      g.fillStyle = css(p.role === "hook" ? "--lane-hook" : "--lane-video");
      rr(g, px, PARTS_Y, pw, PARTS_H, wide ? 5 : 1);
      g.fill();
      if (wide) {
        g.save();
        rr(g, px, PARTS_Y, pw, PARTS_H, 5);
        g.clip();
        // Filmstrip: the proxy's thumbnails tiled along the part (in frame order: only
        // the part's visible ones are visited).
        if (thumbs && thumbs.length && pw > 10) {
          if (!moving) g.globalAlpha = 0.92;
          // the part's own frames only (a lengthening drag reveals none of its neighbours')
          const from = Math.max(a, p.out[0], Math.floor(frameAt(-THUMB_W)));
          let k = 0;
          let hi = thumbs.length;
          while (k < hi) {
            const mid = (k + hi) >> 1;
            if (thumbs[mid]!.frame < from) k = mid + 1;
            else hi = mid;
          }
          for (; k < thumbs.length; k++) {
            const t = thumbs[k]!;
            if (t.frame >= Math.min(b, p.out[1])) break;
            const tx = xOf(t.frame);
            if (tx > w) break;
            if (!t.image) continue;
            const tw = Math.min(THUMB_W, (t.image.width / t.image.height) * thumbH());
            g.drawImage(t.image, tx, PARTS_Y, tw, thumbH());
          }
          g.globalAlpha = moving ? 0.35 : 1;
        }
        // The waveform band (∿ on): energy (−60…0 dB) mirrored around its centre line,
        // quiet regions as a faint strip along the bottom.
        const pa = wave() && lane?.src === "json" ? lane.parts[pi] : undefined;
        if (wave()) {
          g.fillStyle = css("--wave-band");
          g.fillRect(px, PARTS_Y + thumbH(), pw, WAVE_H);
        }
        // at the timeline's scale on the part's own frames: a trim drag cuts it off at the
        // moved edge or reveals the source past it, never stretches it (decision 0053)
        const [ox0, ox1] = [xOf(p.out[0] + off), xOf(p.out[1] + off)];
        if (wave() && lane?.src === "columns") drawColumns(g, lane, p.out[0], p.out[1], ox0, ox1, w);
        const el = edgeLevels();
        const dragged = !!d && "part" in d && d.part.id === p.id;
        if (wave() && el && dragged && el.part === p.id && (a < p.out[0] || b > p.out[1])) {
          const mid = PARTS_Y + thumbH() + WAVE_H / 2;
          g.fillStyle = css("--wave");
          const len = p.out[1] - p.out[0];
          for (let k = a - p.out[0]; k < b - p.out[0]; k++) {
            if (k >= 0 && k < len) continue;
            const db = el.db[k - el.first];
            if (db === undefined || db <= SILENT_DB) continue;
            const xx = xOf(p.out[0] + k);
            if (xx < -2 || xx > w + 2) continue;
            const amp = Math.max(0, Math.min(1, (db + 60) / 60)) * (WAVE_H / 2 - 2);
            g.fillRect(xx, mid - amp, Math.max(1, pf - 0.3), amp * 2 + 0.5);
          }
        }
        if (pa && pa.db.length) {
          const mid = PARTS_Y + thumbH() + WAVE_H / 2;
          const stepX = (ox1 - ox0 - 2) / pa.db.length;
          g.fillStyle = css("--wave");
          const k0 = Math.max(0, Math.floor((-2 - ox0 - 1) / stepX));
          for (let k = k0; k < pa.db.length; k++) {
            const xx = ox0 + 1 + k * stepX;
            if (xx > w + 2) break;
            const amp = Math.max(0, Math.min(1, ((pa.db[k] ?? -90) + 60) / 60)) * (WAVE_H / 2 - 2);
            g.fillRect(xx, mid - amp, Math.max(1, stepX - 0.3), amp * 2 + 0.5);
          }
          g.fillStyle = css("--wave-quiet");
          for (const [qa, qb] of pa.quiet) g.fillRect(xOf(qa), PARTS_Y + PARTS_H - 3, Math.max(1, xOf(qb) - xOf(qa)), 3);
        }
        // The label on a scrim, clipped to the part.
        if (pw > 22) {
          const label = `${p.label}${p.role === "hook" ? " · Hook" : ""}`;
          const dur = pw > 74 ? `${((b - a) / fps).toFixed(1)}s` : "";
          g.font = UI(600, 11);
          const lw = g.measureText(label).width;
          g.font = MONO;
          const dw = dur ? g.measureText(dur).width + 6 : 0;
          g.fillStyle = css("--part-scrim");
          rr(g, px + 4, PARTS_Y + 4, Math.min(pw - 8, lw + dw + 10), 17, 4);
          g.fill();
          g.fillStyle = css("--text-1");
          g.font = UI(600, 11);
          g.fillText(label, px + 9, PARTS_Y + 16.5);
          if (dur) {
            g.font = MONO;
            g.fillStyle = css("--text-2");
            g.fillText(dur, px + 9 + lw + 6, PARTS_Y + 16.5);
          }
        }
        g.restore();
      }
      // A group (decision 0037): a bar in the group's own colour over each of its parts.
      if (p.group) {
        g.fillStyle = groupColour(p.group);
        g.fillRect(px, PARTS_Y - 5, pw, 3);
      }
      // Outline: the accent when selected, the sticker's ink otherwise.
      if (wide || sel) {
        rr(g, px + 1, PARTS_Y + 1, pw - 2, PARTS_H - 2, wide ? 6 : 1);
        g.strokeStyle = sel ? css("--accent") : css("--outline");
        g.lineWidth = sel ? 3 : 1.5;
        g.stroke();
      }
      // the selected part (the one the inspector shows): an accent wash over it and a bar
      // along its top, so it reads at a glance
      if (sel) {
        g.fillStyle = css("--accent-wash");
        g.fillRect(px + 2, PARTS_Y + 2, pw - 4, PARTS_H - 4);
        g.fillStyle = css("--accent");
        g.fillRect(px, PARTS_Y - 6, pw, 4);
      }
      // Trim grips on the selected part and the one under the pointer.
      const hovered = hx !== null && hx >= x0 && hx < x1;
      if ((sel || hovered) && pw > 18) {
        g.fillStyle = sel ? css("--accent") : css("--g-9");
        rr(g, px + 3, PARTS_Y + PARTS_H / 2 - 10, 3, 20, 1.5);
        g.fill();
        rr(g, px + pw - 6, PARTS_Y + PARTS_H / 2 - 10, 3, 20, 1.5);
        g.fill();
      }
      g.globalAlpha = 1;
    }
    // Cut-out ticks: one per second along the top of each matted part, solid where its
    // accurate mask track is built, faint where it is still the draft.
    for (const m of mattes()?.parts ?? []) {
      const [ma, mb] = m.out;
      const built = ma + Math.round(((mb - ma) * m.done) / Math.max(1, m.total));
      const step = Math.max(1, Math.round(fps));
      for (let f = ma; f < mb; f += step) {
        const x = xOf(f);
        if (x < -2 || x > w + 2) continue;
        g.fillStyle = f < built ? css("--accent") : css("--tick-pending");
        g.fillRect(x, PARTS_Y + 1, 1, 5);
      }
      g.fillStyle = css("--accent");
      g.fillRect(xOf(ma), PARTS_Y + 1, Math.max(0, xOf(built) - xOf(ma)), 2);
    }
    // A sweep's preview: what it would cut.
    g.fillStyle = css("--sweep");
    for (const [ra, rb] of sweepRibbons()) g.fillRect(xOf(ra), PARTS_Y, Math.max(2, xOf(rb) - xOf(ra)), PARTS_H);
    // Parts being moved: an outline where they would land (the others slid aside).
    if (sliding) {
      const at = slideTargets(v, sliding.ids, sliding.x);
      const block = v.parts.filter((p) => sliding.ids.includes(p.id));
      const xa = xOf(Math.min(...block.map((p) => p.out[0] + (at.get(p.id) ?? 0))));
      const xb = xOf(Math.max(...block.map((p) => p.out[1] + (at.get(p.id) ?? 0))));
      g.strokeStyle = css("--accent");
      g.lineWidth = 2;
      rr(g, xa + 1, PARTS_Y + 1, Math.max(2, xb - xa - 2), PARTS_H - 2, 5);
      g.stroke();
    }
    // Measurement brackets (and one being marked), with their values.
    g.font = MONO;
    g.strokeStyle = css("--accent");
    g.lineWidth = 1.5;
    for (const m of lanes()?.measurements ?? []) {
      const xa = xOf(m.out[0]);
      const xb = xOf(m.out[1]);
      if (xb < 0 || xa > w) continue;
      g.beginPath();
      g.moveTo(xa + 0.5, MEAS_Y + 13);
      g.lineTo(xa + 0.5, MEAS_Y + 7);
      g.lineTo(xb - 0.5, MEAS_Y + 7);
      g.lineTo(xb - 0.5, MEAS_Y + 13);
      g.stroke();
      const label = fmtMeasure(m.value, m.unit);
      const tw = g.measureText(label).width + 8;
      if (xb - xa > tw + 4) {
        const cx = Math.max(xa + 2, Math.min(xb - tw - 2, (xa + xb) / 2 - tw / 2));
        g.fillStyle = css("--accent");
        rr(g, cx, MEAS_Y + 1, tw, 12, 3);
        g.fill();
        g.fillStyle = css("--text-on-accent");
        g.fillText(label, cx + 4, MEAS_Y + 10.5);
      }
    }
    const mf = measureFrom();
    if (mf !== null) {
      const x = xOf(mf);
      g.beginPath();
      g.moveTo(x + 0.5, MEAS_Y + 13);
      g.lineTo(x + 0.5, MEAS_Y + 7);
      g.lineTo(x + 10, MEAS_Y + 7);
      g.stroke();
      g.fillStyle = css("--accent-text");
      g.fillText("measuring…", x + 12, MEAS_Y + 10);
    }
    // Gain-envelope lane: 0 dB in the middle, each segment a plateau at its level with
    // its ramps; the corner points are its handles.
    const y0 = gainY(0);
    g.fillStyle = css("--sep-strong");
    g.fillRect(Math.max(0, x0cut), Math.round(y0), Math.max(0, Math.min(w, x1cut) - Math.max(0, x0cut)), 1);
    g.font = MONO;
    for (const gs of gains()) {
      const xa = xOf(gs.out[0]);
      const xb = xOf(gs.out[1]);
      if (xb < 0 || xa > w) continue;
      const half = Math.max(0, (xb - xa) / 2);
      const ri = Math.min(half, (gs.ramp_in_ms / 1000) * fps * pf);
      const ro = Math.min(half, (gs.ramp_out_ms / 1000) * fps * pf);
      const yg = gainY(gs.gain_db);
      g.beginPath();
      g.moveTo(xa, y0);
      g.lineTo(xa + ri, yg);
      g.lineTo(xb - ro, yg);
      g.lineTo(xb, y0);
      g.closePath();
      g.fillStyle = css("--accent-wash");
      g.fill();
      g.strokeStyle = css("--lane-gain");
      g.lineWidth = 1.5;
      g.stroke();
      g.fillStyle = css("--lane-gain");
      for (const [px, py] of [[xa, y0], [xa + ri, yg], [xb - ro, yg], [xb, y0]] as const) {
        g.beginPath();
        g.arc(px, py, 2.5, 0, Math.PI * 2);
        g.fill();
      }
      if (xb - xa > 46) {
        g.fillStyle = css("--text-1");
        g.fillText(`${gs.gain_db > 0 ? "+" : ""}${gs.gain_db} dB`, xa + ri + 4, gs.gain_db >= 0 ? yg + 11 : yg - 4);
      }
    }
    // Caption lane.
    g.font = UI(500, 10);
    for (const c of v.captions) {
      if (c.role !== "caption") continue;
      const x0 = xOf(c.out[0]);
      const x1 = xOf(c.out[1]);
      if (x1 < 0 || x0 > w) continue;
      const cw = Math.max(1, x1 - x0 - 1);
      g.fillStyle = css("--lane-caption");
      rr(g, x0, CAP_Y, cw, CAP_H, cw > 8 ? 3 : 0);
      g.fill();
      if (x1 - x0 > 26) {
        g.save();
        g.beginPath();
        g.rect(x0, CAP_Y, x1 - x0 - 3, CAP_H);
        g.clip();
        g.fillStyle = css("--text-1");
        g.fillText(c.text.replace(/\n/g, " "), x0 + 5, CAP_Y + 14.5);
        g.restore();
      }
    }
    // Overlay lanes (clipped below the caption lane; they scroll when they overflow).
    const ovs = overlays();
    const ovLane = packed().lane;
    const ld = laneDrag("overlay");
    g.save();
    g.beginPath();
    g.rect(0, OV_Y - 2, w, Math.max(0, h - OV_Y + 2));
    g.clip();
    g.font = UI(500, 10);
    ovs.forEach((o, i) => {
      const y = laneTop(ovLane[i] ?? 0);
      if (y > h || y + OV_H < OV_Y - 2) return;
      const x0 = xOf(o.out[0]);
      const x1 = xOf(o.out[1]);
      if (x1 < 0 || x0 > w) return;
      const ow = Math.max(1, x1 - x0 - 1);
      // a video or image overlay is a layer of the picture: its own colour, and an edge
      // along its foot when it sits behind the picture's layers (decision 0042)
      const pic = o.kind === "video" || o.kind === "image";
      const lane = o.kind === "video" ? "--lane-video" : o.kind === "image" ? "--lane-image" : "--lane-text";
      g.fillStyle = css(lane);
      rr(g, x0, y, ow, OV_H, ow > 8 ? 3 : 0);
      g.fill();
      if (pic) {
        g.strokeStyle = css(`${lane}-edge`);
        g.lineWidth = 1;
        rr(g, x0 + 0.5, y + 0.5, Math.max(1, ow - 1), OV_H - 1, ow > 8 ? 3 : 0);
        g.stroke();
        if (o.under != null) {
          g.fillStyle = css(`${lane}-edge`);
          g.fillRect(x0 + 1, y + OV_H - 3, Math.max(1, ow - 2), 2);
        }
      }
      if (ld?.id === o.id) {
        g.strokeStyle = css("--accent");
        g.lineWidth = 2;
        rr(g, x0 + 1, y + 1, Math.max(1, ow - 2), OV_H - 2, 3);
        g.stroke();
      }
      if (x1 - x0 > 26) {
        g.save();
        g.beginPath();
        g.rect(x0, y, x1 - x0 - 3, OV_H);
        g.clip();
        g.fillStyle = css("--text-1");
        g.fillText(pic && !o.text ? pictureLabel(o) : o.text.replace(/\n/g, " "), x0 + 5, y + 12.5);
        g.restore();
      }
    });
    g.restore();
    // A band being drawn to select parts.
    const bd = drag();
    if (bd && "band" in bd && bd.band.moved) {
      const [lo, hi] = [Math.min(bd.band.x0, bd.band.x), Math.max(bd.band.x0, bd.band.x)];
      g.fillStyle = css("--accent-wash");
      g.fillRect(lo, PARTS_Y - 2, hi - lo, h - PARTS_Y + 2);
      g.strokeStyle = css("--accent");
      g.lineWidth = 1;
      g.strokeRect(lo + 0.5, PARTS_Y - 1.5, Math.max(1, hi - lo - 1), h - PARTS_Y + 1);
    }
    // The range and the marks.
    const r = range();
    if (r) {
      g.fillStyle = css("--accent-wash");
      g.fillRect(xOf(r[0]), 0, Math.max(1, xOf(r[1]) - xOf(r[0])), h);
    }
    // I / O marks: brackets on the ruler (one alone shows where the range will start or end)
    g.fillStyle = css("--accent");
    for (const [m, side] of [[markIn(), 1], [markOut(), -1]] as const) {
      if (m === null) continue;
      const x = Math.round(xOf(m));
      g.fillRect(x - (side < 0 ? 2 : 0), 2, 2, RULER - 2);
      g.fillRect(side > 0 ? x : x - 7, 2, 7, 2);
      g.fillRect(side > 0 ? x : x - 7, RULER - 2, 7, 2);
    }
    // the part edge the keyboard nudges (V): a bar on that edge, a tab on its inner side
    const se = selectedEdge();
    const ep = se ? v.parts.find((p) => p.id === se.part) : undefined;
    if (se && ep) {
      const x = Math.round(xOf(se.edge === "in" ? ep.out[0] : ep.out[1]));
      g.fillStyle = css("--accent");
      g.fillRect(x - 1.5, PARTS_Y - 4, 3, PARTS_H + 8);
      g.fillRect(se.edge === "in" ? x : x - 8, PARTS_Y - 4, 8, 3);
      g.fillRect(se.edge === "in" ? x : x - 8, PARTS_Y + PARTS_H + 1, 8, 3);
    }
    g.fillStyle = css("--warning");
    for (const m of marks()) {
      const x = xOf(m);
      g.beginPath();
      g.moveTo(x - 5, RULER - 9);
      g.lineTo(x + 5, RULER - 9);
      g.lineTo(x, RULER - 1);
      g.fill();
      g.fillRect(x, RULER, 1, h - RULER);
    }
    // Comment markers (decision 0015): a flag in the ruler per thread on the cut.
    for (const m of commentMarks()) {
      const x = xOf(m.frame);
      g.fillStyle = m.resolved ? css("--text-3") : css("--accent");
      g.fillRect(x, 2, 1.5, RULER - 4);
      g.fillRect(x, 2, 7, 6);
    }
    // Recording markers (⌃⌥M while recording, decision 0052): a tick with a dot at its top.
    g.fillStyle = css("--danger");
    for (const m of cutMarkers()) {
      const x = Math.round(xOf(markerFrame(m.cut_ms, fps)));
      g.fillRect(x - 0.75, RULER - 9, 1.5, 9);
      g.beginPath();
      g.arc(x, RULER - 10, 2.5, 0, Math.PI * 2);
      g.fill();
    }
    // The hover line and the time under the pointer (never while dragging).
    if (hx !== null && !d) {
      g.fillStyle = css("--hover-line");
      g.fillRect(Math.round(hx), RULER, 1, h - RULER);
      const label = timecode(Math.max(0, Math.min(v.frames, Math.round(frameAt(hx)))), fps);
      g.font = MONO;
      const tw = g.measureText(label).width + 10;
      const lx = Math.max(0, Math.min(w - tw, hx - tw / 2));
      g.fillStyle = css("--g-6");
      rr(g, lx, 4, tw, 16, 4);
      g.fill();
      g.fillStyle = css("--text-1");
      g.fillText(label, lx + 5, 15.5);
    }
    // The playhead: a line, and a head in the ruler you can drag.
    const head = viewer()?.frame ?? 0;
    const phx = Math.round(xOf(head));
    g.fillStyle = css("--playhead");
    g.fillRect(phx - 0.5, 4, 1.5, h - 4);
    g.beginPath();
    g.moveTo(phx - 6, 2);
    g.lineTo(phx + 7, 2);
    g.lineTo(phx + 7, 12);
    g.lineTo(phx + 0.5, 19);
    g.lineTo(phx - 6, 12);
    g.closePath();
    g.fill();
    perf()?.draws.push(performance.now() - t0);
    // D.02: the first draw of a newly loaded lane closes its request → window time
    if (lane && lane !== windowFor && v) {
      windowFor = lane;
      const ms = performance.now() - lane.t0;
      setWindowMs(ms);
      const pf = perf();
      if (pf) (pf.columns ??= []).push({ load: lane.load, window: ms, frames: lane.src === "columns" ? lane.energy.length : 0 });
    }
  };

  /** The energy column under one part (its frames `[fa, fb)` drawn across `x0…x1`):
   * per pixel the loudest frame in it, or per frame when frames are wider than a pixel;
   * then the quiet regions as a strip along the bottom. Only the visible pixels. */
  const drawColumns = (g: CanvasRenderingContext2D, lane: Extract<AudioLane, { src: "columns" }>, fa: number, fb: number, x0: number, x1: number, w: number) => {
    const span = Math.min(fb, lane.energy.length) - fa;
    const xw = x1 - x0 - 2;
    if (span <= 0 || xw <= 0) return;
    const e = lane.energy;
    const xa = x0 + 1;
    const mid = PARTS_Y + thumbH() + WAVE_H / 2;
    const ampOf = (db: number) => Math.max(0, Math.min(1, (db + 60) / 60)) * (WAVE_H / 2 - 2);
    g.fillStyle = css("--wave");
    const perPx = span / xw;
    if (perPx >= 1) {
      const p0 = Math.max(0, Math.floor(-2 - xa));
      const p1 = Math.min(Math.ceil(xw), Math.ceil(w + 2 - xa));
      for (let px = p0; px < p1; px++) {
        const f0 = fa + Math.floor(px * perPx);
        const f1 = Math.max(f0 + 1, Math.min(fa + span, fa + Math.floor((px + 1) * perPx)));
        let m = -120;
        for (let f = f0; f < f1; f++) if (e[f]! > m) m = e[f]!;
        if (m <= SILENT_DB) continue;
        const amp = ampOf(m);
        g.fillRect(xa + px, mid - amp, 1, amp * 2 + 0.5);
      }
    } else {
      const stepX = xw / span;
      const k0 = Math.max(0, Math.floor((-2 - xa) / stepX));
      for (let k = k0; k < span; k++) {
        const xx = xa + k * stepX;
        if (xx > w + 2) break;
        const db = e[fa + k]!;
        if (db <= SILENT_DB) continue;
        const amp = ampOf(db);
        g.fillRect(xx, mid - amp, Math.max(1, stepX - 0.3), amp * 2 + 0.5);
      }
      // close up: each frame's sample peaks (min/max) as a line through its bar
      const pk = stepX >= PEAKS_PPF ? peaks() : null;
      if (pk && pk.length >= (fa + span) * 2) {
        const half = WAVE_H / 2 - 1;
        g.fillStyle = css("--wave-peak");
        for (let k = k0; k < span; k++) {
          const xx = xa + k * stepX;
          if (xx > w + 2) break;
          const lo = Math.max(-1, pk[(fa + k) * 2]!);
          const hi = Math.min(1, pk[(fa + k) * 2 + 1]!);
          if (hi <= lo) continue;
          g.fillRect(xx + stepX / 2 - 0.5, mid - hi * half, 1, Math.max(0.5, (hi - lo) * half));
        }
      }
    }
    // quiet regions inside the part (pairs of output frames, in order)
    const q = lane.quiet;
    let lo = 0;
    let hi = q.length >> 1;
    while (lo < hi) {
      const m = (lo + hi) >> 1;
      if (q[m * 2 + 1]! <= fa) lo = m + 1;
      else hi = m;
    }
    g.fillStyle = css("--wave-quiet");
    for (let i = lo * 2; i + 1 < q.length; i += 2) {
      const qa = Math.max(fa, q[i]!);
      const qb = Math.min(fa + span, q[i + 1]!);
      if (q[i]! >= fa + span) break;
      if (qb <= qa) continue;
      const ra = xa + (qa - fa) / perPx;
      if (ra > w + 2) break;
      g.fillRect(ra, PARTS_Y + PARTS_H - 3, Math.max(1, (qb - qa) / perPx), 3);
    }
  };

  createEffect(() => {
    cutView();
    viewer();
    void prefs.waveform;
    audio();
    peaks();
    mattes();
    strip();
    lanes();
    gains();
    overlays();
    measureFrom();
    sweepRibbons();
    pxPerFrame();
    scroll();
    ovScroll();
    drag();
    selectedPart();
    pickedParts();
    marks();
    range();
    markIn();
    markOut();
    selectedEdge();
    hoverX();
    commentMarks();
    cutMarkers();
    draw();
  });

  createEffect(on(loopAsk, () => loop(), { defer: true }));

  /** Lets go of a lane drag's core gesture with nothing written (Esc, a click). */
  const releaseLane = (l: LaneDrag) => {
    const v = cutView();
    if (!v) return;
    void l.gesture.then((g) => {
      if (g !== null) void attempt(() => actions.timelineGestureEnd(v.project, g));
    });
  };

  onMount(() => {
    const ro = new ResizeObserver(() => {
      setLaneW(wrap.clientWidth);
      draw();
    });
    ro.observe(wrap);
    onCleanup(() => ro.disconnect());
    const mq = matchMedia("(prefers-color-scheme: dark)");
    const theme = () => {
      tokens = {};
      draw();
    };
    mq.addEventListener("change", theme);
    onCleanup(() => mq.removeEventListener("change", theme));
    // Settings › Appearance switched the theme (prefs.applyAppearance)
    addEventListener("gmclip-theme", theme);
    onCleanup(() => removeEventListener("gmclip-theme", theme));
    const keys = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement).closest("input, textarea, select")) return;
      // Esc cancels a drag: nothing is written
      const d = drag();
      if (e.key === "Escape" && d && !("scrub" in d)) {
        e.preventDefault();
        if ("lane" in d) releaseLane(d.lane);
        void endPeek();
        setDrag(null);
        return;
      }
      // ⌥S: hover scrub on / off
      if (e.altKey && !e.metaKey && !e.ctrlKey && e.code === "KeyS") {
        e.preventDefault();
        toggleHoverScrub("key");
        return;
      }
      // ⌥L: loop the range, else the part under the playhead
      if (e.altKey && !e.metaKey && !e.ctrlKey && e.code === "KeyL") {
        e.preventDefault();
        loop();
        return;
      }
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.key === "-") zoom(1 / 1.6);
      if (e.key === "=" || e.key === "+") zoom(1.6);
    };
    addEventListener("keydown", keys);
    onCleanup(() => removeEventListener("keydown", keys));
    // A drag still open when the timeline goes away lets go of its gesture.
    onCleanup(() => {
      const d = drag();
      if (d && "lane" in d) releaseLane(d.lane);
    });
  });

  const hit = (x: number, y: number): { part: PartView; edge: "in" | "out" | null } | null => {
    const v = cutView();
    if (!v || y < PARTS_Y || y > PARTS_Y + PARTS_H) return null;
    // The nearest edge wins; at a boundary shared by two parts, the side of the pointer
    // decides (left: the earlier part's out, right: the later part's in).
    let best: { part: PartView; edge: "in" | "out"; d: number } | null = null;
    for (const p of v.parts) {
      for (const edge of ["in", "out"] as const) {
        const ex = xOf(edge === "in" ? p.out[0] : p.out[1]);
        const d = Math.abs(x - ex);
        if (d > EDGE) continue;
        const sideOk = edge === "in" ? x >= ex : x < ex;
        const score = d + (sideOk ? 0 : EDGE);
        if (!best || score < best.d) best = { part: p, edge, d: score };
      }
    }
    if (best) return { part: best.part, edge: best.edge };
    const p = v.parts.find((p) => x >= xOf(p.out[0]) && x < xOf(p.out[1]));
    return p ? { part: p, edge: null } : null;
  };

  /** Which side of a span `[xa, xb]` the pointer is on: an edge within EDGE px (the
   * nearer one; never both on a narrow span), else the body. */
  const sideOf = (x: number, xa: number, xb: number): "in" | "out" | "body" => {
    const narrow = xb - xa < EDGE * 3;
    if (!narrow && Math.abs(x - xa) <= EDGE) return "in";
    if (!narrow && Math.abs(x - xb) <= EDGE) return "out";
    if (narrow && x < xa + 3) return "in";
    if (narrow && x > xb - 3) return "out";
    return "body";
  };
  const hitOverlay = (x: number, y: number): { o: Ov; side: "in" | "out" | "body" } | null => {
    if (y < OV_Y) return null;
    const ovs = overlays();
    const { lane } = packed();
    for (let k = ovs.length - 1; k >= 0; k--) {
      const o = ovs[k]!;
      const ly = laneTop(lane[k] ?? 0);
      const xa = xOf(o.out[0]);
      const xb = xOf(o.out[1]);
      if (y >= ly && y <= ly + OV_H && x >= xa - 3 && x <= xb + 3) return { o, side: sideOf(x, xa, xb) };
    }
    return null;
  };
  const hitGain = (x: number, y: number): { g: Gain; side: "in" | "out" | "body" } | null => {
    if (y < GAIN_Y || y > GAIN_Y + GAIN_H) return null;
    for (const gs of gains()) {
      const xa = xOf(gs.out[0]);
      const xb = xOf(gs.out[1]);
      if (x >= xa - 3 && x <= xb + 3) return { g: gs, side: sideOf(x, xa, xb) };
    }
    return null;
  };

  const local = (e: PointerEvent | WheelEvent | MouseEvent) => {
    const r = (e.target === ruler ? ruler : canvas).getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };

  let seekQueued: number | null = null;
  let seekScrub = false;
  /** One seek per animation frame; a scrub step (`scrub`) shows the scrub proxy until
   * the pointer rests, a click seeks exactly. */
  const seekSoon = (f: number, scrub = false) => {
    const first = seekQueued === null;
    seekQueued = Math.max(0, f);
    seekScrub = first ? scrub : seekScrub && scrub;
    if (first)
      requestAnimationFrame(() => {
        const t = seekQueued;
        const sc = seekScrub;
        seekQueued = null;
        if (t !== null) void attempt(() => (sc ? actions.viewerScrubTo(t) : actions.viewerSeek(t)));
      });
  };
  /** The part edge being dragged (its frames-moved readout sits by the edge). */
  const edgeDrag = () => {
    const d = drag();
    return d && "part" in d ? d : null;
  };
  /** The trim monitor (decision 0053): while a part edge is dragged, the viewer shows the
   * frame on the edge as the trim would leave it. One request in flight, one per
   * animation frame, the latest wins; the end waits for the last one. */
  let peekWant: { part: string; edge: "in" | "out"; delta: number } | null = null;
  let peekBusy: Promise<void> | null = null;
  let peeking = false;
  const nextFrame = () => new Promise<void>((r) => requestAnimationFrame(() => r()));
  const peekSoon = (part: string, edge: "in" | "out", delta: number) => {
    peekWant = { part, edge, delta };
    peeking = true;
    peekBusy ??= (async () => {
      await nextFrame();
      while (peekWant) {
        const w = peekWant;
        peekWant = null;
        const v = cutView();
        if (!v) break;
        await actions.viewerTrimPeek(v.project, v.cut, w.part, w.edge, w.delta).catch(() => undefined);
        if (peekWant) await nextFrame();
      }
      peekBusy = null;
    })();
  };
  /** Ends the monitor: the viewer goes back to the cut and the frame it showed. */
  const endPeek = async () => {
    peekWant = null;
    if (!peeking) return;
    peeking = false;
    if (peekBusy) await peekBusy;
    await actions.viewerTrimPeekEnd().catch(() => undefined);
  };
  /** Starts a lane drag: the core holds the entity from now (R-GEST-1). */
  const beginLane = (kind: "overlay" | "gain", id: string, side: "in" | "out" | "body", out: [number, number], db: number, x: number, y: number, pointer: number) => {
    const v = cutView();
    if (!v) return;
    (pressEl ?? canvas).setPointerCapture(pointer);
    const gesture = actions
      .timelineGestureBegin(v.project, kind, id)
      .then((r) => r.gesture)
      .catch(() => null);
    setDrag({ lane: { kind, id, side, out, db, x0: x, y0: y, df: 0, ddb: 0, moved: false, gesture } });
  };
  /** Where the last press landed (fractional px; the double-click reads it). */
  let lastDown: { x: number; y: number } | null = null;
  const onDown = (e: PointerEvent) => {
    pressEl = e.currentTarget as HTMLCanvasElement;
    lastDown = local(e);
    // the middle button pans through time (a mouse without a sideways wheel)
    if (e.button === 1) {
      e.preventDefault();
      (pressEl ?? canvas).setPointerCapture(e.pointerId);
      setDrag({ pan: true, x0: local(e).x, s0: scroll() });
      return;
    }
    if (e.button !== 0) return;
    const { x, y } = local(e);
    const v0 = cutView();
    // ⌘-click a part adds it to the selection, ⇧-click selects the run up to it (decision
    // 0037); elsewhere they set marks and ranges
    const hp = v0 && (isAddClick(e) || e.shiftKey) && !e.altKey ? hit(x, y) : null;
    if (hp && !hp.edge) {
      // a click picks it (on release); a drag draws a band over the parts it crosses
      (pressEl ?? canvas).setPointerCapture(e.pointerId);
      setDrag({ band: { x0: x, x, add: isAddClick(e), run: e.shiftKey, base: selection(), on: hp.part.id, moved: false } });
      return;
    }
    if (v0 && (isAddClick(e) || e.shiftKey) && !e.altKey) {
      const f = Math.max(0, Math.min(v0.frames, Math.round(frameAt(x))));
      if (isAddClick(e)) {
        // ⌘-click (Ctrl-click off a Mac): a mark (again on one removes it)
        const near = marks().find((m) => Math.abs(xOf(m) - x) <= 5);
        setMarks(near !== undefined ? marks().filter((m) => m !== near) : [...marks(), f].sort((a, b) => a - b));
      } else {
        // ⇧-click: a range from the nearest mark before it (else the playhead)
        const before = [...marks()].reverse().find((m) => m < f);
        const from = before ?? viewer()?.frame ?? 0;
        setRange(from === f ? null : [Math.min(from, f), Math.max(from, f)]);
      }
      return;
    }
    // an overlay: drag it (a model cut's), else a click seeks to its start
    const ho = hitOverlay(x, y);
    // the blade on an overlay cuts it there (on a part edge each piece follows its part)
    if (ho && tlTool() === "blade" && v0 && editable()) {
      const f = Math.round(frameAt(x));
      if (f > ho.o.out[0] && f < ho.o.out[1]) void editing(attempt(() => actions.overlaySplitAt(v0.project, v0.cut, ho.o.id, f))).then(() => refetchLanes());
      return;
    }
    if (ho) {
      if (editable()) beginLane("overlay", ho.o.id, ho.side, ho.o.out, 0, x, y, e.pointerId);
      else void attempt(() => actions.viewerSeek(ho.o.out[0]));
      return;
    }
    if (y >= OV_Y) {
      // the empty space under the lanes: a drag there selects the parts it spans
      if (v0) {
        (pressEl ?? canvas).setPointerCapture(e.pointerId);
        setDrag({ band: { x0: x, x, add: isAddClick(e), run: false, base: selection(), on: null, moved: false } });
      }
      return;
    }
    // the gain lane: drag a segment; a drag on empty lane draws one (double-click: 1 s)
    if (y >= GAIN_Y && y <= GAIN_Y + GAIN_H && v0) {
      const hg = hitGain(x, y);
      if (!gainsEditable()) {
        if (hg) void attempt(() => actions.viewerSeek(hg.g.out[0]));
        return;
      }
      if (hg) beginLane("gain", hg.g.id, hg.side, hg.g.out, hg.g.gain_db, x, y, e.pointerId);
      else {
        (pressEl ?? canvas).setPointerCapture(e.pointerId);
        setDrag({ lane: { kind: "gain", id: "", side: "body", out: [0, 0], db: dbAt(y), x0: x, y0: y, df: 0, ddb: 0, moved: false, gesture: Promise.resolve(null) } });
      }
      return;
    }
    // the blade: a click on a part splits it there
    if (tlTool() === "blade" && v0 && y >= PARTS_Y && y <= PARTS_Y + PARTS_H) {
      const f = Math.round(frameAt(x));
      if (v0.parts.some((p) => f > p.out[0] && f < p.out[1])) void attempt(() => actions.split(v0.project, v0.cut, f));
      return;
    }
    (pressEl ?? canvas).setPointerCapture(e.pointerId);
    const h = hit(x, y);
    if (h?.edge) {
      setDrag({ part: h.part, edge: h.edge, x0: x, delta: 0, snapped: "" });
      const v = cutView();
      const reach = Math.min(36_000, Math.ceil(laneW() / Math.max(ppf(), 1e-6)));
      const part = h.part.id;
      setEdgeLevels(null);
      if (v)
        void actions
          .partEnergy(v.project, v.cut, part, reach, reach)
          .then((r) => {
            const d = drag();
            if (r && d && "part" in d && d.part.id === part) setEdgeLevels({ part, ...r });
          })
          .catch(() => undefined);
      clearPicked();
      setSelectedPart(h.part.id);
      return;
    }
    if (h) {
      // A part's body: a click seeks, a drag moves the part (with the rest of the
      // selection, or its group).
      if (!selection().includes(h.part.id)) pickPart(h.part.id, "only");
      else setSelectedPart(h.part.id);
      setDrag({ move: h.part, ids: selection(), x0: x, x, active: false });
      return;
    }
    clearPicked();
    // empty space below the ruler: a drag draws a band over the parts, a click seeks
    if (y >= RULER && v0) {
      setDrag({ band: { x0: x, x, add: false, run: false, base: [], on: null, moved: false, seek: true } });
      return;
    }
    setDrag({ scrub: true });
    seekSoon(Math.round(frameAt(x)), true);
  };
  /** A lane drag's frame change, snapped (⌥ off) to part and word edges. */
  const laneDelta = (l: LaneDrag, x: number, alt: boolean): number => {
    const pf = ppf();
    let df = Math.round((x - l.x0) / pf);
    if (alt) return df;
    const max = SNAP_PX / pf;
    const t = snapTargets();
    const edges = l.side === "in" ? [l.out[0]] : l.side === "out" ? [l.out[1]] : [l.out[0], l.out[1]];
    let best: number | null = null;
    for (const e0 of edges) {
      const s = nearest(t, e0 + df, max);
      if (s !== null && (best === null || Math.abs(s - (e0 + df)) < Math.abs(best))) best = s - (e0 + df);
    }
    if (best !== null) df += best;
    return df;
  };
  const onMove = (e: PointerEvent) => {
    const { x, y } = local(e);
    const d = drag();
    if (!d) {
      const ho = editable() ? hitOverlay(x, y) : null;
      const hg = gainsEditable() ? hitGain(x, y) : null;
      const h = hit(x, y);
      const side = ho?.side ?? hg?.side;
      setHover(tlTool() === "blade" && ((y >= PARTS_Y && y <= PARTS_Y + PARTS_H) || ho) ? "crosshair" : side === "in" || side === "out" || h?.edge ? "col-resize" : side === "body" ? "grab" : y < RULER ? "ew-resize" : y >= GAIN_Y && y <= GAIN_Y + GAIN_H && gainsEditable() ? "copy" : "default");
      setHoverX(x);
      const v = cutView();
      const mk = y < RULER && v ? cutMarkers().find((m) => Math.abs(xOf(markerFrame(m.cut_ms, rate(v.fps))) - x) <= 4) : undefined;
      setMarkerHover(mk ? markerTip(mk) : "");
      // hover scrub (when on): over the ruler, the viewer follows the pointer (paused only)
      if (prefs.hover_scrub && y < RULER && !viewer()?.playing) seekSoon(Math.round(frameAt(x)), true);
      return;
    }
    if ("scrub" in d) {
      seekSoon(Math.round(frameAt(x)), true);
      return;
    }
    if ("pan" in d) {
      scrollTo(d.s0 - (x - d.x0));
      return;
    }
    if ("band" in d) {
      const b = { ...d.band, x, moved: d.band.moved || Math.abs(x - d.band.x0) > 4 };
      setDrag({ band: b });
      const v = cutView();
      if (b.moved && v) {
        const [lo, hi] = [Math.min(b.x0, x), Math.max(b.x0, x)];
        const ids = v.parts.filter((p) => xOf(p.out[1]) > lo && xOf(p.out[0]) < hi).map((p) => p.id);
        const all = withGroups(v, b.add ? [...b.base, ...ids] : ids);
        setPickedParts(new Set(all));
        setSelectedPart(ids[0] ?? all[0] ?? null);
      }
      return;
    }
    if ("lane" in d) {
      const l = d.lane;
      const moved = l.moved || Math.abs(x - l.x0) > 3 || Math.abs(y - l.y0) > 3;
      if (!l.id) {
        // a new gain segment: dragging sizes it
        setDrag({ lane: { ...l, moved, df: Math.round((x - l.x0) / ppf()) } });
        return;
      }
      // a gain body moves vertically too (0.5 dB steps); small horizontal jitter is held
      const ddb = l.kind === "gain" && l.side === "body" ? clampDb(l.db + dbAt(y) - dbAt(l.y0)) - l.db : 0;
      const df = Math.abs(x - l.x0) <= 3 && !l.moved ? 0 : laneDelta(l, x, e.altKey || !snapping());
      setDrag({ lane: { ...l, moved, df, ddb } });
      return;
    }
    if ("move" in d) {
      setDrag({ ...d, x, active: d.active || Math.abs(x - d.x0) > 8 });
      return;
    }
    // Whole frames, snapped to the nearest word edge or energy onset within a few pixels
    // (⌥ off), never past the part's other edge.
    const len = d.part.out[1] - d.part.out[0];
    const edgeAt = d.edge === "in" ? d.part.out[0] : d.part.out[1];
    let raw = Math.round((x - d.x0) / ppf());
    let snapped: "" | "word" | "onset" = "";
    if (!e.altKey && snapping()) {
      const want = edgeAt + raw;
      const max = SNAP_PX / ppf();
      const sw = nearest(wordEdges(), want, max);
      const so = nearest(onsetsOf(d.part), want, max);
      const s = sw === null ? so : so === null ? sw : Math.abs(so - want) < Math.abs(sw - want) ? so : sw;
      if (s !== null) {
        raw = s - edgeAt;
        snapped = s === sw ? "word" : "onset";
      }
    }
    const delta = d.edge === "in" ? Math.min(raw, len - 1) : Math.max(raw, 1 - len);
    setDrag({ ...d, snapped, delta });
    if (delta !== d.delta || !peeking) peekSoon(d.part.id, d.edge, delta);
  };
  /** Releases a lane drag: one command (one undo step) that also ends the gesture. */
  const upLane = async (l: LaneDrag) => {
    const v = cutView();
    if (!v) return;
    if (!l.id) {
      // a drag on empty gain lane draws a segment there at that level (a click alone adds
      // nothing: a stray click made segments nobody meant; double-click adds 1 s)
      if (!l.moved || l.df === 0) return;
      const f = Math.round(frameAt(l.x0));
      const [a, b] = l.moved && l.df !== 0 ? [Math.min(f, f + l.df), Math.max(f, f + l.df)] : [f - Math.round(rate(v.fps) / 2), f + Math.round(rate(v.fps) / 2)];
      const lo = Math.max(0, a);
      const hi = Math.min(v.frames, b);
      const db = Math.abs(l.db) < 1 ? 6 : l.db;
      if (hi - lo >= 2) await attempt(() => actions.gainSpan(v.project, v.cut, lo, hi, db));
      return;
    }
    const g = await l.gesture;
    if (!l.moved || (l.df === 0 && l.ddb === 0)) {
      if (g !== null) void attempt(() => actions.timelineGestureEnd(v.project, g));
      // a click on an overlay seeks to its start, as before
      if (l.kind === "overlay" && !l.moved) void attempt(() => actions.viewerSeek(l.out[0]));
      return;
    }
    const [a, b] = laneSpan(l);
    const from = l.side === "out" ? null : a;
    const to = l.side === "in" ? null : b;
    if (l.kind === "overlay") await attempt(() => actions.overlayTimeSet(v.project, v.cut, l.id, from, to, g));
    else await attempt(() => actions.gainSegmentEdit(v.project, v.cut, l.id, l.df === 0 ? null : from, l.df === 0 ? null : to, l.ddb === 0 ? null : clampDb(l.db + l.ddb), g));
  };
  /** Whether the last release ended a drag that moved (its dblclick is not a delete). */
  let lastUpMoved = false;
  const onUp = () => {
    // the release lands where the last move was
    if (pendingMove) flush();
    const d = drag();
    const v = cutView();
    lastUpMoved = !!d && (("lane" in d && d.lane.moved) || ("move" in d && d.active) || ("part" in d && d.delta !== 0) || ("band" in d && d.band.moved));
    if (d && "lane" in d) {
      // the preview holds until the commit lands and the lanes are read back, so the
      // overlay never jumps back: the commit's own reply comes before the cut's next
      // revision reaches `cutView`, and dropping the preview then drew the old span
      // for a moment (a drag started in that moment hit the old span)
      void editing(upLane(d.lane))
        .then(() => refetchLanes())
        .finally(() => {
          if (drag() === d) setDrag(null);
        });
      return;
    }
    setDrag(null);
    if (d && "band" in d && !d.band.moved) {
      // a click, not a drag: on a part, ⌘ toggles it and ⇧ picks the run up to it;
      // in the empty space, the selection clears
      if (d.band.on) pickPart(d.band.on, d.band.run ? "run" : "toggle");
      else if (!d.band.add) clearPicked();
      if (d.band.seek) seekSoon(Math.round(frameAt(d.band.x0)), true);
      return;
    }
    if (!d || "scrub" in d || "pan" in d || "band" in d || !v) return;
    if ("move" in d) {
      if (!d.active) {
        // a click (no drag) on a part of a selection selects just it (and its group)
        pickPart(d.move.id, "only");
        seekSoon(Math.round(frameAt(d.x0)));
        return;
      }
      const to = dropIndex(d.x, d.ids);
      if (d.ids.length <= 1) {
        const from = v.parts.findIndex((p) => p.id === d.move.id);
        if (to !== from) void attempt(() => actions.reorderTo(v.project, v.cut, d.move.id, to));
        return;
      }
      // several parts: back to back at the drop (unless they already sit there together)
      const ids = [...new Set(v.parts.map((p) => p.id))];
      const at = ids.map((id, i) => (d.ids.includes(id) ? i : -1)).filter((i) => i >= 0);
      const together = at[at.length - 1]! - at[0]! === at.length - 1;
      if (!(together && to === at[0])) void attempt(() => actions.partsMoveTo(v.project, v.cut, d.ids, to));
      return;
    }
    void endPeek();
    setEdgeLevels(null);
    if (d.delta === 0) return;
    // The core maps the drag onto the source and clamps it (no frame math here).
    void attempt(() => actions.trim(v.project, v.cut, d.part.id, d.edge, d.delta));
  };
  const onCancel = () => {
    const d = drag();
    if (d && "lane" in d) releaseLane(d.lane);
    void endPeek();
    setDrag(null);
  };
  /** Double-click on a gain segment removes it. */
  const onDbl = (e: MouseEvent) => {
    if (lastUpMoved) return;
    // the pointer-down's own position: a dblclick's coordinates are whole pixels
    const { x, y } = lastDown ?? local(e);
    const v = cutView();
    const hg = hitGain(x, y);
    if (v && hg && gainsEditable()) void attempt(() => actions.gainSpanDelete(v.project, hg.g.id));
    else if (v && !hg && gainsEditable() && y >= GAIN_Y && y <= GAIN_Y + GAIN_H) {
      // double-click on empty gain lane: 1 s at that level (+6 dB near the middle)
      const f = Math.round(frameAt(x));
      const half = Math.round(rate(v.fps) / 2);
      const lo = Math.max(0, f - half);
      const hi = Math.min(v.frames, f + half);
      const db = Math.abs(dbAt(y)) < 1 ? 6 : dbAt(y);
      if (hi - lo >= 2) void attempt(() => actions.gainSpan(v.project, v.cut, lo, hi, db));
    }
  };
  /** Whether the lanes scroll vertically (a short timeline) and can move toward `dy`. */
  const canScrollLanes = (dy: number) =>
    body.scrollHeight > body.clientHeight + 1 && (dy > 0 ? body.scrollTop + body.clientHeight < body.scrollHeight - 1 : body.scrollTop > 0);
  const onWheel = (e: WheelEvent) => {
    e.preventDefault();
    const { y } = local(e);
    const upDown = Math.abs(e.deltaY) > Math.abs(e.deltaX);
    if (e.altKey) {
      // one zoom step per notch, applied with the frame (several notches in a frame add up)
      wheel.zoom += e.deltaY < 0 ? 1 : -1;
    } else if (e.shiftKey) {
      // ⇧-wheel: time, anywhere (macOS turns a mouse's ⇧-wheel into deltaX itself)
      wheel.dx += e.deltaX || e.deltaY;
    } else if (upDown && y >= OV_Y && ovMaxScroll() > 0) wheel.ov += e.deltaY;
    else if (upDown && y >= GAIN_Y && canScrollLanes(e.deltaY)) {
      // a short timeline: below the parts, the wheel moves its lanes up and down first
      body.scrollTop += e.deltaY;
      return;
    } else wheel.dx += e.deltaX + e.deltaY;
    soon();
  };
  // decision 0042: videos and pictures dropped on the timeline become picture layers
  // (video and image overlays) starting at the frame under the drop
  onMount(() => {
    setTimelineDropHook(() => (paths: string[], at: [number, number]) => {
      const v = cutView();
      const r = canvas.getBoundingClientRect();
      if (!v || !editable() || at[0] < r.left || at[0] > r.right || at[1] < r.top || at[1] > r.bottom) return false;
      const videos = paths.filter((p) => /\.(mp4|mov|m4v|mkv|webm)$/i.test(p));
      const images = paths.filter((p) => /\.(png|jpe?g)$/i.test(p));
      if (!videos.length && !images.length) return false;
      const f = Math.max(0, Math.min(v.frames - 1, Math.round(frameAt(at[0] - r.left))));
      void (async () => {
        for (const p of videos) await attempt(() => actions.videoOverlayAdd(v.project, v.cut, f, p));
        for (const p of images) await attempt(() => actions.imageOverlayAddPath(v.project, v.cut, f, p));
        await refetchLanes();
      })();
      const n = videos.length + images.length;
      say(`Added ${n === 1 ? "a layer" : `${n} layers`} at ${(f / rate(v.fps)).toFixed(1)} s — ⌘Z undoes it`);
      return true;
    });
  });
  onCleanup(() => setTimelineDropHook(null));
  /** The scrollbar under the lanes: its thumb is the visible span of the cut. */
  let track!: HTMLDivElement;
  const thumb = () => {
    void laneW();
    const full = maxScroll() + wrap.clientWidth;
    if (maxScroll() <= 0 || full <= 0) return null;
    return { left: scroll() / full, width: wrap.clientWidth / full };
  };
  const onTrackDown = (e: PointerEvent) => {
    const t = thumb();
    if (e.button !== 0 || !t) return;
    e.preventDefault();
    const r = track.getBoundingClientRect();
    const full = maxScroll() + wrap.clientWidth;
    const at = (e.clientX - r.left) / r.width;
    // on the track, not the thumb: centre the view there, then drag on from it
    if (at < t.left || at > t.left + t.width) scrollTo((at - t.width / 2) * full);
    const x0 = e.clientX;
    const s0 = scroll();
    track.setPointerCapture(e.pointerId);
    const move = (m: PointerEvent) => scrollTo(s0 + ((m.clientX - x0) / r.width) * full);
    const up = () => {
      track.removeEventListener("pointermove", move);
      track.removeEventListener("pointerup", up);
      track.removeEventListener("pointercancel", up);
    };
    track.addEventListener("pointermove", move);
    track.addEventListener("pointerup", up);
    track.addEventListener("pointercancel", up);
  };
  /** Input waiting for the next animation frame: the last pointer move and the wheel's
   * sum, applied once per frame however many events arrive (R-TL-1: a flood of input
   * never queues a draw per event). */
  let pendingMove: PointerEvent | null = null;
  const wheel = { dx: 0, ov: 0, zoom: 0 };
  let scheduled = false;
  const flush = () => {
    scheduled = false;
    const m = pendingMove;
    pendingMove = null;
    if (m) onMove(m);
    if (wheel.zoom) zoom(1.6 ** wheel.zoom);
    if (wheel.ov) setOvScroll(Math.min(ovMaxScroll(), Math.max(0, ovScroll() + wheel.ov)));
    if (wheel.dx) scrollTo(scroll() + wheel.dx);
    wheel.dx = wheel.ov = wheel.zoom = 0;
  };
  const soon = () => {
    if (scheduled) return;
    scheduled = true;
    requestAnimationFrame(flush);
  };
  const onMoveSoon = (e: PointerEvent) => {
    pendingMove = e;
    soon();
  };

  return (
    <section class="timeline" aria-label="Timeline">
      <Toolbar label="Timeline tools" class="timeline-bar">
        <ToolbarGroup>
          <IconButton icon="pointer" label="Select tool" hint="Select: drag edges to trim, parts to reorder" pressed={tlTool() === "select"} onClick={() => setTlTool("select")} />
          <IconButton icon="scissors" label="Blade tool" hint="Blade: click a part or an overlay to split it there" pressed={tlTool() === "blade"} onClick={() => setTlTool(tlTool() === "blade" ? "select" : "blade")} />
          <IconButton icon="merge" label="Merge with the next part" hint={mergePair() ? "Merge the selected part with the next one (they play on in the source)" : "Select a part that plays on into the next one to merge them"} disabled={!mergePair()} data-testid="part-merge" onClick={() => void merge()} />
          <IconButton icon="link" label="Group" keys="⌘G" hint={canGroup() ? "Group the selected parts: they move together" : "Select two or more parts (⌘-click or ⇧-click) to group them"} disabled={!canGroup()} data-testid="parts-group" onClick={() => void groupSelected()} />
          <IconButton icon="unlink" label="Ungroup" keys="⌘⇧G" hint={canUngroup() ? "Ungroup the selected group" : "Select a grouped part to ungroup it"} disabled={!canUngroup()} data-testid="parts-ungroup" onClick={() => void ungroupSelected()} />
        </ToolbarGroup>
        <ToolbarSep />
        <ToolbarGroup>
          <Toggle icon="magnet" label="Snapping" hint={snapping() ? "Snapping on: edges snap to words, sound onsets and parts (hold ⌥ for frames)" : "Snapping off: edges move by frames"} on={snapping()} onChange={setSnapping} />
          <Toggle icon="wave" label="Waveform" hint={wave() ? "Waveform on under each part" : "Waveform off: the filmstrip fills the part"} on={wave()} onChange={(on) => setPref("waveform", on)} data-testid="waveform-toggle" />
          <Toggle
            icon="zoom-in"
            label="Zoom and layout lanes"
            hint={sectionLanesShown() ? "Zoom and layout sections shown above the parts" : "Show the zoom and layout lanes (sections over the parts, decision 0046)"}
            on={sectionLanesShown()}
            onChange={setSectionLanesOn}
            data-testid="section-lanes-toggle"
          />
        </ToolbarGroup>
        <Show when={markIn() !== null || markOut() !== null || marks().length}>
          <ToolbarSep />
          <button type="button" class="ghost small" data-testid="marks-clear" onClick={() => { setMarks([]); setRange(null); }}>
            <Icon name="x" size={13} />
            Clear marks
          </button>
        </Show>
          <Show when={building().length}>
          <Chip
            class="proxy-chip"
            icon="loader"
            testid="proxy-building"
            tooltip={`Making a light copy for smooth scrubbing (${proxyPct()}%). Until it covers a spot, scrubbing and the filmstrip read the original's keyframes; playback and paused frames are always the exact picture.`}
          >
            <span class="pc-long">Proxy building… scrubbing reads the original · {proxyPct()}%</span>
            <span class="pc-short">Proxy {proxyPct()}%</span>
          </Chip>
        </Show>
        <span class="spacer" />
        <ToolbarGroup>
          <IconButton icon="zoom-out" label="Zoom out" keys="−" onClick={() => zoom(1 / 1.6)} />
          <input
            type="range"
            class="zoom-slider"
            aria-label="Zoom"
            min="0"
            max="1000"
            value={sliderOf()}
            onInput={(e) => fromSlider(Number(e.currentTarget.value))}
          />
          <IconButton icon="zoom-in" label="Zoom in" keys="=" onClick={() => zoom(1.6)} />
          <IconButton icon="fit" label="Fit" onClick={() => { setPxPerFrame(null); setScroll(0); }} />
        </ToolbarGroup>
      </Toolbar>
      <div class="tl-ruler-row">
        <div class="lane-head ruler-head" />
        <canvas
          ref={ruler}
          class="tl-ruler"
          data-testid="timeline-ruler"
          data-rec-markers={cutMarkers().map((m) => `${m.n}@${m.cut_ms}`).join(",")}
          title={markerHover() || undefined}
          style={{ cursor: drag() && "pan" in drag()! ? "grabbing" : "ew-resize" }}
          onPointerDown={onDown}
          onPointerMove={onMoveSoon}
          onPointerUp={onUp}
          onPointerCancel={onCancel}
          onPointerLeave={() => {
            setHoverX(null);
            setMarkerHover("");
          }}
          onDblClick={onDbl}
          onWheel={onWheel}
          aria-hidden="true"
        />
      </div>
      <LayerTracks overlays={lanes()?.overlays ?? []} editable={editable()} refetch={() => void refetchLanes()} />
      <SectionLanes />
      <div class="timeline-body" ref={body} data-testid="timeline-body" data-lanes-min-h={MIN_LANES_H}>
        <div class="lane-heads" aria-hidden="true">
          <For each={LANES}>
            {(l) => (
              <div class={`lane-head lane-${l.name.toLowerCase()}`} style={{ top: `${l.y - RULER}px`, height: `${l.h}px` }}>
                <Icon name={l.icon} size={13} />
                <span>{l.name}</span>
              </div>
            )}
          </For>
        </div>
      <div class="timeline-wrap" ref={wrap}>
        <canvas
          ref={canvas}
          class="timeline-canvas"
          data-frames={cutView()?.frames ?? 0}
          data-ppf={ppf()}
          data-scroll={scroll()}
          data-audio-points={audio()?.points ?? 0}
          data-audio-src={audio()?.src ?? ""}
          data-peaks={peaks()?.length ? peaks()!.length / 2 : 0}
          data-columns-ms={audio() ? `${audio()!.load.toFixed(1)}/${windowMs()?.toFixed(1) ?? ""}` : ""}
          data-overlay-lanes={packed().lanes}
          data-overlay-kinds={(lanes()?.overlays ?? []).map((o) => `${o.kind ?? "text"}${o.under != null ? ":under" : ""}${o.matte ? ":matte" : ""}`).join(",")}
          data-overlays-editable={editable() ? "1" : "0"}
          data-lanes-y={`${MEAS_Y},${PARTS_Y},${GAIN_Y},${CAP_Y},${OV_Y},${OV_H + OV_GAP}`}
          data-measuring={measureFrom() ?? ""}
          data-dragging={drag() && "lane" in drag()! ? "lane" : drag() ? "1" : ""}
          data-snapped={(() => { const d = drag(); return d && "part" in d ? d.snapped : ""; })()}
          data-edge-levels={edgeLevels()?.db.length ?? 0}
          data-drop={(() => { const d = drag(); return d && "move" in d && d.active ? dropIndex(d.x, d.ids) : ""; })()}
          data-waveform={wave() ? "1" : "0"}
          data-matte-ticks={(mattes()?.parts ?? []).map((m) => `${m.done}/${m.total}`).join(",")}
          data-matte-build={mattes()?.build ? `${mattes()!.build!.state}${mattes()!.build!.auto ? " auto" : ""}` : ""}
          data-onsets={onsetParts().flatMap((p) => p.onsets ?? []).join(",")}
          data-marks={marks().join(",")}
          data-comments={commentMarks().map((m) => m.frame).join(",")}
          data-range={range()?.join("-") ?? ""}
          data-mark-in={markIn() ?? ""}
          data-mark-out={markOut() ?? ""}
          data-edge={(() => { const e = selectedEdge(); return e ? `${e.part}:${e.edge}` : ""; })()}
          data-hover-frame={hoverX() === null ? "" : Math.max(0, Math.round(frameAt(hoverX()!)))}
          data-hover-scrub={prefs.hover_scrub ? "1" : "0"}
          style={{ cursor: drag() && "pan" in drag()! ? "grabbing" : hover() }}
          onPointerDown={onDown}
          onPointerMove={onMoveSoon}
          onPointerUp={onUp}
          onPointerCancel={onCancel}
          onPointerLeave={() => setHoverX(null)}
          onDblClick={onDbl}
          onWheel={onWheel}
          aria-hidden="true"
        />
        <Show when={edgeDrag()}>
          {(d) => (
            <div
              class={`tl-trim-readout ${d().edge}`}
              data-testid="trim-readout"
              data-delta={d().delta}
              style={{ left: `${xOf(d().part.out[d().edge === "in" ? 0 : 1] + d().delta)}px`, top: `${PARTS_Y - RULER + 4}px` }}
            >
              {d().delta > 0 ? "+" : d().delta < 0 ? "−" : "±"}
              {Math.abs(d().delta)}f
            </div>
          )}
        </Show>
      </div>
      </div>
      <div class="tl-hscroll" aria-hidden="true">
        <div class="tl-hscroll-track" ref={track} data-testid="timeline-hscroll" onPointerDown={onTrackDown} onWheel={onWheel}>
          <Show when={thumb()}>
            {(t) => <div class="tl-hscroll-thumb" style={{ left: `${t().left * 100}%`, width: `${Math.max(t().width * 100, 2)}%` }} />}
          </Show>
        </div>
      </div>
      <ol class="sr-only mirror" aria-label="Parts">
        <For each={cutView()?.parts ?? []}>
          {(p) => (
            <li>
              <button type="button" onClick={() => { setSelectedPart(p.id); void attempt(() => actions.viewerSeek(p.out[0])); }}>
                Part {p.label}{p.role === "hook" ? ", hook" : ""}: frames {p.out[0]}–{p.out[1]}
              </button>
            </li>
          )}
        </For>
      </ol>
      <ol class="sr-only mirror" aria-label="Overlays">
        <For each={overlays()}>
          {(o, i) => (
            <li>
              <button type="button" data-overlay={o.id} onClick={() => void attempt(() => actions.viewerSeek(o.out[0]))}>
                {o.kind === "video" || o.kind === "image" ? pictureLabel(o) : `${o.role}: ${o.text}`} (frames {o.out[0]}–{o.out[1]}, lane {(packed().lane[i()] ?? 0) + 1})
              </button>
            </li>
          )}
        </For>
      </ol>
      <ol class="sr-only mirror" aria-label="Gain">
        <For each={gains()}>
          {(gs) => (
            <li>
              <button type="button" data-gain={gs.id} onClick={() => void attempt(() => actions.viewerSeek(gs.out[0]))}>
                {gs.gain_db > 0 ? "+" : ""}{gs.gain_db} dB (frames {gs.out[0]}–{gs.out[1]})
              </button>
            </li>
          )}
        </For>
      </ol>
      <ol class="sr-only mirror" aria-label="Measurements">
        <For each={lanes()?.measurements ?? []}>
          {(m) => (
            <li>
              <button type="button" data-measurement={m.id} onClick={() => void attempt(() => actions.viewerSeek(m.out[0]))}>
                {fmtMeasure(m.value, m.unit)} (frames {m.out[0]}–{m.out[1]}, {m.evidence} evidence frames)
              </button>
            </li>
          )}
        </For>
      </ol>
      <ol class="sr-only mirror" aria-label="Captions">
        <For each={(cutView()?.captions ?? []).filter((c) => c.role === "caption")}>
          {(c) => (
            <li>
              <button type="button" onClick={() => void attempt(() => actions.viewerSeek(c.out[0]))}>
                {c.role}: {c.text} (frames {c.out[0]}–{c.out[1]})
              </button>
            </li>
          )}
        </For>
      </ol>
    </section>
  );
}
