// The timeline's picture layers as tracks (AJ 2026-10-19: "see layers on the timeline …
// reorganizing the height should be really easy, as well as changing the individual clips
// on each layer"). One row per picture layer of the parts (by role: screen, camera, a
// detail inset) and one per image or video overlay, front-most at the top, in draw order
// (decision 0042). Drag a row's grip up or down to restack: the layers' z follow the new
// order and each overlay sits just under the layer row above it (none above: in front of
// the picture). A layer row's clips are its parts (a click selects that part's layer in
// the inspector); an overlay row's clip moves and trims like the overlay lanes. The eye
// shows or hides a row.
//
// AJ 2026-10-28 ("i should be able to cut it"): the blade cuts an overlay's clip where it
// is clicked, the scissors on its row cut it at every part edge (each piece then moves with
// its part), and the pieces of one video or image share its row.
//
// AJ 2026-10-20 ("if I select the camera, I can see which element it is … group them and
// change the crop for multiple cameras … and positioning"): the layer selected on the
// preview or in the inspector is marked on its row and clip; ⌘- or ⇧-click clips of one row
// to group them (a click on the row's name groups the whole row), and a crop or a place
// written on one grouped layer is written on all of them.
import { createMemo, createSignal, For, Show } from "solid-js";
import { actions, type TimelineLanes } from "../actions";
import { askCrop, attempt, cutView, editing, layerGroup, say, selectedOverlay, selectedPart, setLayerGroup, setSelectedOverlay, setSelectedPart, tlTool, tlView, viewer } from "../store";
import { showTab } from "../layout";
import { Icon } from "../design/Icon";

type Ov = TimelineLanes["overlays"][number];

/** A row: a layer role across the parts, or one image or video overlay. */
type Clip = { part: string; layer: string | null; out: [number, number]; visible: boolean };
type Track =
  | { kind: "layer"; key: string; role: string; z: number; models: string[]; visible: boolean; clips: Clip[] }
  | { kind: "overlay"; key: string; o: Ov; os: Ov[]; index: number };

const ROW_H = 26;
const ROLE_NAME: Record<string, string> = { cam: "Camera", face: "Camera", screen: "Screen", detail: "Detail", extra: "Extra", clip: "Clip" };
const roleName = (r: string) => {
  const [base, n] = r.split("-");
  const name = ROLE_NAME[base ?? r] ?? r;
  return n ? `${name} ${Number(n) + 1}` : name;
};

export function LayerTracks(props: { overlays: Ov[]; editable: boolean; refetch: () => void }) {
  const tracks = createMemo<Track[]>(() => {
    const v = cutView();
    if (!v) return [];
    // the part layers by role: their z (the most common), their model layers, their parts
    const roles = new Map<string, { zs: Map<number, number>; models: Set<string>; clips: Clip[] }>();
    for (const p of v.parts) {
      for (const l of p.layers) {
        const r = roles.get(l.role) ?? { zs: new Map<number, number>(), models: new Set<string>(), clips: [] as Clip[] };
        r.zs.set(l.z, (r.zs.get(l.z) ?? 0) + 1);
        if (l.model) r.models.add(l.model);
        const last = r.clips[r.clips.length - 1];
        // a part shown as several plan parts: one clip
        if (last && last.part === p.id && last.out[1] === p.out[0]) last.out = [last.out[0], p.out[1]];
        else r.clips.push({ part: p.id, layer: l.model, out: [p.out[0], p.out[1]], visible: l.visible });
        roles.set(l.role, r);
      }
    }
    const layers: Track[] = [...roles].map(([role, r]) => ({
      kind: "layer",
      key: `layer:${role}`,
      role,
      z: [...r.zs].sort((a, b) => b[1] - a[1])[0]![0],
      models: [...r.models],
      visible: r.clips.some((c) => c.visible),
      clips: r.clips,
    }));
    // one row per picture: the pieces of a cut overlay (same media, same place in the stack)
    const rows = new Map<string, Extract<Track, { kind: "overlay" }>>();
    props.overlays.forEach((o, index) => {
      if (o.kind !== "video" && o.kind !== "image") return;
      const same = o.media ? `${o.kind}:${o.media}:${o.under ?? "top"}:${o.matte}` : o.id;
      const row = rows.get(same);
      if (row) row.os.push(o);
      else rows.set(same, { kind: "overlay", key: `overlay:${o.id}`, o, os: [o], index });
    });
    const ovs: Track[] = [...rows.values()];
    // front first: an overlay without `under` above everything, one with `under` just
    // under the layers of that z
    const rank = (t: Track) => (t.kind === "layer" ? t.z : t.o.under == null ? Number.POSITIVE_INFINITY : t.o.under - 0.5);
    return [...layers, ...ovs].sort((a, b) => rank(b) - rank(a) || (a.kind === "overlay" && b.kind === "overlay" ? b.index - a.index : 0));
  });

  // a row being dragged to a new place in the stack
  const [move, setMove] = createSignal<{ key: string; y0: number; dy: number } | null>(null);
  const dropAt = () => {
    const m = move();
    if (!m) return -1;
    const from = tracks().findIndex((t) => t.key === m.key);
    return Math.max(0, Math.min(tracks().length - 1, from + Math.round(m.dy / ROW_H)));
  };
  /** The stack in its new order, written back: z for the layer rows (front highest), and
   * each overlay just under the layer row above it (none: in front of the picture). */
  const restack = async (order: Track[]) => {
    const v = cutView();
    if (!v) return;
    const layerRows = order.filter((t): t is Extract<Track, { kind: "layer" }> => t.kind === "layer");
    // the layers keep their z while their order holds (only overlays moved); else front
    // highest, 10 apart
    const kept = layerRows.every((t, i) => i === 0 || layerRows[i - 1]!.z > t.z);
    const z = new Map(layerRows.map((t, i) => [t.key, kept ? t.z : (layerRows.length - i) * 10]));
    const writes: (() => Promise<unknown>)[] = [];
    for (const t of layerRows) {
      const want = z.get(t.key)!;
      if (want === t.z) continue;
      for (const m of t.models) writes.push(() => actions.layerZ(v.project, m, want));
    }
    let above: number | null = null;
    for (const t of order) {
      if (t.kind === "layer") above = z.get(t.key)!;
      else if ((t.o.under ?? null) !== above) {
        const u = above;
        for (const o of t.os) writes.push(() => actions.overlayStackSet(v.project, o.id, u));
      }
    }
    if (!writes.length) return;
    await editing((async () => {
      for (const w of writes) if (!(await attempt(w))) break;
    })());
    props.refetch();
    say("Layers restacked — ⌘Z undoes each step");
  };
  const onGripDown = (e: PointerEvent, t: Track) => {
    if (e.button !== 0 || !props.editable) return;
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    setMove({ key: t.key, y0: e.clientY, dy: 0 });
  };
  const onGripMove = (e: PointerEvent) => {
    const m = move();
    if (m) setMove({ ...m, dy: e.clientY - m.y0 });
  };
  const onGripUp = () => {
    const m = move();
    const to = dropAt();
    setMove(null);
    if (!m) return;
    const list = [...tracks()];
    const from = list.findIndex((t) => t.key === m.key);
    if (from < 0 || to === from) return;
    const [t] = list.splice(from, 1);
    list.splice(to, 0, t!);
    void restack(list);
  };
  /** The row's offset while another row is dragged past it. */
  const shift = (i: number, key: string) => {
    const m = move();
    if (!m) return 0;
    if (m.key === key) return m.dy;
    const from = tracks().findIndex((t) => t.key === m.key);
    const to = dropAt();
    if (from < i && i <= to) return -ROW_H;
    if (to <= i && i < from) return ROW_H;
    return 0;
  };

  // the timeline's mapping (output frame → px in the lanes' body)
  const x = (f: number) => {
    const t = tlView();
    return t ? 12 + f * t.ppf - t.scroll : 0;
  };
  const w = (n: number) => Math.max(2, n * (tlView()?.ppf ?? 0));

  // an overlay clip dragged along its row: its body moves, its edges trim
  const [clipDrag, setClipDrag] = createSignal<{ id: string; side: "in" | "out" | "body"; x0: number; df: number; out: [number, number] } | null>(null);
  const spanOf = (o: Ov): [number, number] => {
    const d = clipDrag();
    if (!d || d.id !== o.id) return o.out;
    const total = cutView()?.frames ?? 0;
    let [a, b] = d.out;
    if (d.side === "body") {
      const len = b - a;
      a = Math.max(0, Math.min(total - len, a + d.df));
      b = a + len;
    } else if (d.side === "in") a = Math.max(0, Math.min(b - 1, a + d.df));
    else b = Math.min(total, Math.max(a + 1, b + d.df));
    return [a, b];
  };
  const onClipDown = (e: PointerEvent, o: Ov) => {
    if (e.button !== 0) return;
    e.stopPropagation();
    if (!props.editable) return void attempt(() => actions.viewerSeek(o.out[0]));
    const el = e.currentTarget as HTMLElement;
    const v = cutView();
    const t = tlView();
    if (tlTool() === "blade" && v && t && t.ppf > 0) {
      // the blade: cut it at the frame clicked (the clip's row body is the lanes' x axis)
      const body = el.parentElement!.getBoundingClientRect();
      const f = Math.round((e.clientX - body.left - 12 + t.scroll) / t.ppf);
      if (f > o.out[0] && f < o.out[1]) void editing(attempt(() => actions.overlaySplitAt(v.project, v.cut, o.id, f))).then(() => props.refetch());
      return;
    }
    const r = el.getBoundingClientRect();
    const side = e.clientX - r.left < 6 ? "in" : r.right - e.clientX < 6 ? "out" : "body";
    el.setPointerCapture(e.pointerId);
    setClipDrag({ id: o.id, side, x0: e.clientX, df: 0, out: o.out });
  };
  const onClipMove = (e: PointerEvent) => {
    const d = clipDrag();
    const ppf = tlView()?.ppf ?? 0;
    if (d && ppf > 0) setClipDrag({ ...d, df: Math.round((e.clientX - d.x0) / ppf) });
  };
  const onClipUp = (o: Ov) => {
    const d = clipDrag();
    const v = cutView();
    if (!d || !v) return setClipDrag(null);
    if (d.df === 0) {
      setClipDrag(null);
      // selected, a frame in (the lanes' start can round a frame before the plan's), so
      // its box on the preview drags
      setSelectedOverlay(o.id);
      const at = viewer()?.frame ?? -1;
      if (at <= o.out[0] || at >= o.out[1]) void attempt(() => actions.viewerSeek(Math.min(o.out[0] + 1, o.out[1] - 1)));
      showTab("look", "overlay-list");
      return;
    }
    const [a, b] = spanOf(o);
    void editing(attempt(() => actions.overlayTimeSet(v.project, v.cut, o.id, d.side === "out" ? null : a, d.side === "in" ? null : b, null)))
      .then(() => props.refetch())
      .finally(() => setClipDrag(null));
  };

  /** The layer selected on the preview (its part and role), so its row and clip show it. */
  const sel = createMemo(() => {
    const s = viewer()?.selected;
    if (!s) return null;
    const p = cutView()?.parts.find((q) => q.plan_id === s[0]);
    const l = p?.layers.find((x) => x.id === s[1]);
    return p && l ? { part: p.id, role: l.role } : null;
  });
  const inGroup = (c: Clip) => !!c.layer && layerGroup().some((m) => m.part === c.part && m.layer === c.layer);
  /** ⌘/⇧-click: the clip in or out of the group (a clip of another row starts a new one). */
  const toggleGroup = (c: Clip, t: Extract<Track, { kind: "layer" }>) => {
    if (!c.layer) return;
    const rowOf = new Set(t.clips.map((x) => `${x.part}:${x.layer}`));
    let g = layerGroup().filter((m) => rowOf.has(`${m.part}:${m.layer}`));
    // the selected clip of this row joins the group it starts
    const s = sel();
    if (!g.length && s && s.role === t.role && s.part !== c.part) {
      const first = t.clips.find((x) => x.part === s.part && x.layer);
      if (first) g = [{ part: first.part, layer: first.layer! }];
    }
    g = inGroup(c) ? g.filter((m) => !(m.part === c.part && m.layer === c.layer)) : [...g, { part: c.part, layer: c.layer }];
    setLayerGroup(g);
    if (g.length > 1) say(`${roleName(t.role)} on ${g.length} parts grouped: a crop or place set on one is set on all`);
  };
  /** The row's name: every clip of the row grouped. */
  const groupRow = (t: Extract<Track, { kind: "layer" }>) => {
    const g = t.clips.filter((c) => c.layer).map((c) => ({ part: c.part, layer: c.layer! }));
    const same = g.length === layerGroup().length && g.every((m) => layerGroup().some((x) => x.part === m.part && x.layer === m.layer));
    setLayerGroup(same ? [] : g);
    if (!same && g.length > 1) {
      say(`Every ${roleName(t.role)} grouped (${g.length} parts): a crop or place set on one is set on all`);
      const c = t.clips.find((x) => x.part === selectedPart()) ?? t.clips[0];
      if (c && sel()?.role !== t.role) void pickLayerClip(c, t.role);
    }
  };
  /** A layer's clip: its part selected, the playhead on it, and that layer selected on the
   * preview, so a drag there moves it (even under another layer). */
  const pickLayerClip = async (c: { part: string; out: [number, number] }, role: string) => {
    const v = cutView();
    setSelectedPart(c.part);
    setSelectedOverlay(null);
    const at = viewer()?.frame ?? -1;
    const f = at >= c.out[0] && at < c.out[1] ? at : c.out[0];
    if (f !== at) await attempt(() => actions.viewerSeek(f));
    const pp = v?.parts.find((q) => q.id === c.part && f >= q.out[0] && f < q.out[1]) ?? v?.parts.find((q) => q.id === c.part);
    const l = pp?.layers.find((x) => x.role === role);
    if (pp && l) await attempt(() => actions.viewerSelect(pp.plan_id, l.id));
    showTab("look");
  };
  const toggle = (t: Track) => {
    const v = cutView();
    if (!v || !props.editable) return;
    if (t.kind === "layer") {
      void editing((async () => {
        for (const m of t.models) await attempt(() => actions.layerVisible(v.project, m, !t.visible));
      })());
    } else {
      const to = t.o.hidden ? 1 : 0;
      void editing((async () => {
        for (const o of t.os) await attempt(() => actions.overlayOpacity(v.project, o.id, to));
      })()).then(() => props.refetch());
    }
  };
  /** The scissors: every piece of the row cut at the part edges it spans, so each piece
   * moves with its part. */
  const cutAtParts = (t: Extract<Track, { kind: "overlay" }>) => {
    const v = cutView();
    if (!v || !props.editable) return;
    void editing((async () => {
      let n = 0;
      for (const o of t.os) {
        const r = await attempt(() => actions.overlaySplitParts(v.project, o.id));
        if (r) n += r.pieces.length;
      }
      if (n) say(`${label(t)} cut into ${n} pieces at the part edges: each moves with its part (⌘Z undoes it)`);
    })()).then(() => props.refetch());
  };
  const label = (t: Track) =>
    t.kind === "layer" ? roleName(t.role) : `${t.o.kind === "video" ? "Video" : "Image"}${t.o.matte ? " · cut out" : ""}`;

  return (
    <Show when={tracks().length > 1 || tracks().some((t) => t.kind === "overlay")}>
      <section class="layer-tracks" aria-label="Layers" data-testid="layer-tracks" data-order={tracks().map((t) => t.key).join(",")} style={{ height: `${tracks().length * ROW_H + 2}px` }}>
        <For each={tracks()}>
          {(t, i) => (
            <div
              class={`lt-row lt-${t.kind}${move()?.key === t.key ? " moving" : ""}${t.kind === "layer" && sel()?.role === t.role ? " sel" : ""}`}
              data-testid="layer-track"
              data-key={t.key}
              style={{ top: `${i() * ROW_H}px`, transform: `translateY(${shift(i(), t.key)}px)` }}
            >
              <div class="lt-head">
                <button type="button" class="lt-grip" aria-label={`Move ${label(t)} up or down the stack`} data-tip="Drag up or down: in front of or behind the other layers" data-testid="layer-track-grip"
                  onPointerDown={(e) => onGripDown(e, t)} onPointerMove={onGripMove} onPointerUp={onGripUp} onPointerCancel={() => setMove(null)}>
                  <Icon name="grip" size={12} />
                </button>
                <Show when={t.kind === "layer" ? t : null} fallback={<span class="lt-name">{label(t)}</span>}>
                  {(l) => (
                    <button type="button" class="lt-name" data-testid="layer-track-name" aria-pressed={l().clips.length > 1 && l().clips.every(inGroup)}
                      data-tip={`Group every ${roleName(l().role)}: a crop or place set on one is set on all (click again to ungroup)`} onClick={() => groupRow(l())}>
                      {label(t)}
                    </button>
                  )}
                </Show>
                <Show when={t.kind === "overlay" && t.o.kind === "video" && props.editable ? t : null}>
                  {(ot) => (
                    <button type="button" class="lt-eye" aria-label={`Cut ${label(ot())} at every part edge`} data-tip="Cut it at every part edge: each piece then moves with its part when parts are moved"
                      data-testid="layer-track-cut-parts" onClick={() => cutAtParts(ot())}>
                      <Icon name="scissors" size={12} />
                    </button>
                  )}
                </Show>
                <button type="button" class="lt-eye" aria-label={`Show or hide ${label(t)}`} aria-pressed={t.kind === "layer" ? t.visible : !t.o.hidden} data-testid="layer-track-eye" onClick={() => toggle(t)}>
                  <Icon name={(t.kind === "layer" ? t.visible : !t.o.hidden) ? "eye" : "eye-off"} size={12} />
                </button>
              </div>
              <div class="lt-body">
                <Show when={t.kind === "layer" ? t : null}>
                  {(l) => (
                    <For each={l().clips}>
                      {(c) => (
                        <button type="button"
                          class={`lt-clip${c.visible ? "" : " off"}${sel()?.part === c.part && sel()?.role === l().role ? " sel" : selectedPart() === c.part ? " in-part" : ""}${inGroup(c) ? " grouped" : ""}`}
                          style={{ left: `${x(c.out[0])}px`, width: `${w(c.out[1] - c.out[0])}px` }}
                          data-tip={`${roleName(l().role)} on this part: click to place it in the inspector, ⌘-click to group it with others, double-click to crop it`} data-testid="layer-track-layer-clip"
                          data-grouped={String(inGroup(c))} aria-pressed={sel()?.part === c.part && sel()?.role === l().role}
                          aria-label={`${roleName(l().role)}, part ${(cutView()?.parts.findIndex((q) => q.id === c.part) ?? 0) + 1}`}
                          onClick={(e) => {
                            if (e.metaKey || e.ctrlKey || e.shiftKey) {
                              toggleGroup(c, l());
                              void pickLayerClip(c, l().role);
                              return;
                            }
                            // a plain click on a clip outside the group ends it
                            if (layerGroup().length && !inGroup(c)) setLayerGroup([]);
                            void pickLayerClip(c, l().role);
                          }}
                          onDblClick={() => askCrop(c.part, l().role)} />
                      )}
                    </For>
                  )}
                </Show>
                <Show when={t.kind === "overlay" ? t : null}>
                  {(ot) => (
                    <For each={ot().os}>
                      {(o) => (
                        <div class={`lt-clip lt-ov ${o.kind}${o.hidden ? " off" : ""}${selectedOverlay() === o.id ? " sel" : ""}`} data-testid="layer-track-clip" data-overlay={o.id}
                          style={{ left: `${x(spanOf(o)[0])}px`, width: `${w(spanOf(o)[1] - spanOf(o)[0])}px`, cursor: tlTool() === "blade" ? "crosshair" : undefined }}
                          onPointerDown={(e) => onClipDown(e, o)} onPointerMove={onClipMove} onPointerUp={() => onClipUp(o)} onPointerCancel={() => setClipDrag(null)}>
                          <span>{label(ot())}</span>
                        </div>
                      )}
                    </For>
                  )}
                </Show>
              </div>
            </div>
          )}
        </For>
        <Show when={viewer()}>
          {(vw) => <span class="lt-playhead" style={{ left: `calc(var(--lane-header-w) + ${x(vw().frame)}px)` }} />}
        </Show>
      </section>
    </Show>
  );
}
