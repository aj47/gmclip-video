// The inspector: the part under the playhead (or the selected one) and its layers' crop
// and place, edited as numbers (source px / canvas px). Every edit is one command.
import { createMemo, createResource, createSignal, For, Show } from "solid-js";
import { actions, rate, type LayerFrom, type LayerView, type PartView, type ValueSource } from "../actions";
import { Button, IconButton } from "../design/components";
import { askCrop, attempt, cutView, editing, groupFor, layerGroup, say, selectedPart, setLayerGroup, setSelectedPart, viewer } from "../store";
import { CaptionGallery } from "./CaptionGallery";
import { OverlayList } from "./OverlayList";
import { BrandKit } from "./BrandKit";
import { RotoPanel } from "./RotoPanel";
import { CursorFollow, RecordedCursor } from "./CursorFollow";
import { SectionControls } from "./SectionControls";
import { CameraBubble, LayerLook, ScreenStudioLook } from "./ScreenLook";

function Num(props: { label: string; value: number; onCommit: (v: number) => void; readonly?: boolean }) {
  return (
    <label class="num">
      <span>{props.label}</span>
      <input
        type="number"
        step="1"
        value={Math.round(props.value * 10) / 10}
        readOnly={props.readonly}
        onChange={(e) => {
          const v = Number(e.currentTarget.value);
          if (Number.isFinite(v)) props.onCommit(v);
        }}
      />
    </label>
  );
}

const ROLE_NAME: Record<string, string> = { cam: "Camera", face: "Camera", screen: "Screen", detail: "Detail inset", extra: "Extra", clip: "Clip" };
const roleName = (r: string) => ROLE_NAME[r.split("-")[0] ?? r] ?? r;
/** m:ss.s */
const clock = (s: number) => `${Math.floor(s / 60)}:${(s % 60).toFixed(1).padStart(4, "0")}`;

/** A layer in words: how much of the video it shows and where it sits in the frame. */
function layerSummary(l: LayerView, canvas: { w: number; h: number } | undefined): string {
  const shows = `${Math.round(l.crop[2])}×${Math.round(l.crop[3])} px of the video`;
  if (!canvas) return shows;
  const [x, y, w, h] = l.place;
  const full = (a: number, b: number) => Math.abs(a - b) < 2;
  const where =
    full(w, canvas.w) && full(h, canvas.h) && full(x, 0) && full(y, 0)
      ? "fills the frame"
      : full(w, canvas.w)
        ? `full width, ${Math.round((y / canvas.h) * 100)}% down`
        : `${Math.round((w / canvas.w) * 100)}% wide, at ${Math.round((x / canvas.w) * 100)}% / ${Math.round((y / canvas.h) * 100)}%`;
  return `${shows} · ${where}`;
}

const FROM_LABEL: Record<ValueSource, string> = { split: "split", part: "this part", layout: "layout", framing: "default", own: "own layer" };

/** Where a crop or place value comes from (U.09: inherited values shown with their source). */
function FromTag(props: { from: LayerFrom | null | undefined; field: "crop" | "place" }) {
  const k = () => props.from?.[props.field];
  const tip = () => {
    const f = props.from!;
    switch (k()) {
      case "split":
        return `Set by the visual split at ${(f.split_s ?? 0).toFixed(2)} s into the part`;
      case "part":
        return f.owner === "layout" ? `This part's own value, over the linked layout "${f.layout ?? ""}"` : "This part's own value, over the default";
      case "layout":
        return `From the linked layout "${f.layout ?? ""}": edits to the layout show here`;
      case "framing":
        return "From the framing's default stack";
      default:
        return "A layer this part added";
    }
  };
  return (
    <Show when={k()}>
      {(kind) => <span class="from-tag" data-from={kind()} data-testid={`from-${props.field}`} data-tip={tip()}>{FROM_LABEL[kind()]}</span>}
    </Show>
  );
}

/** The part's linked layout in the framing shown: pick one to link, or unlink. */
function LayoutLink(props: { part: PartView }) {
  const f = () => cutView()?.framing ?? null;
  const linked = () => f()?.part_layout_ids?.[props.part.id] ?? null;
  const set = (layout: string | null) => {
    const v = cutView();
    const fr = f()?.active;
    if (v && fr) void attempt(() => actions.layoutSet(v.project, props.part.id, fr, layout));
  };
  return (
    <Show when={(f()?.layouts.length ?? 0) > 0}>
      <div class="row layout-link" role="group" aria-label="Linked layout">
        <span class="muted small">Layout</span>
        <select aria-label="Link to a layout" data-testid="layout-link" value={linked() ?? ""} onChange={(e) => set(e.currentTarget.value || null)}>
          <option value="">Not linked (default)</option>
          <For each={f()!.layouts}>{(l) => <option value={l.id}>{l.name}</option>}</For>
        </select>
        <Show when={linked()}>
          <IconButton icon="unlink" size="sm" label="Unlink the layout" hint="Unlink: this part goes back to the framing's default stack" data-testid="layout-unlink" onClick={() => set(null)} />
        </Show>
      </div>
    </Show>
  );
}

/** The paired hi-res picture's offset against the sound, for the stretch under the
 *  playhead: ±1 frame, typed (seconds), or "the offset changes here" (piecewise). */
function PictureOffset() {
  const fps = () => rate(cutView()?.fps);
  const stretch = () => {
    const rows = cutView()?.picture_offsets ?? [];
    const f = viewer()?.frame ?? 0;
    let cur = rows[0];
    for (const r of rows) if (r.from_frame !== null && r.from_frame <= f) cur = r;
    return cur;
  };
  const off = () => stretch()?.offset_s ?? cutView()?.picture_offset_s ?? 0;
  const set = (s: number, split = false) => {
    const v = cutView();
    if (v && Number.isFinite(s)) void attempt(() => actions.pictureOffset(v.project, v.cut, s, viewer()?.frame ?? 0, split));
  };
  return (
    <div class="row picture-offset" role="group" aria-label="Hi-res picture offset">
      <span class="muted small">Hi-res picture</span>
      <button type="button" class="ghost" aria-label="Picture one frame earlier" onClick={() => set(off() + 1 / fps())}>‹</button>
      <input aria-label="Picture offset (s)" type="number" step="0.001" value={off().toFixed(3)} onChange={(e) => set(Number(e.currentTarget.value))} />
      <button type="button" class="ghost" aria-label="Picture one frame later" onClick={() => set(off() - 1 / fps())}>›</button>
      <span class="muted small">s after the sound file ({Math.round(off() * fps())} frames)</span>
      <Show when={(cutView()?.picture_offsets?.length ?? 0) > 1}>
        <span class="muted small">· stretch from {stretch()?.from_s.toFixed(0)} s of {cutView()!.picture_offsets!.length}</span>
      </Show>
      <button type="button" class="ghost small" title="Start a new offset at the playhead (the picture drifts from here on)" onClick={() => set(off(), true)}>Offset changes here</button>
    </div>
  );
}

export function Inspector() {
  const part = createMemo<PartView | undefined>(() => {
    const v = cutView();
    if (!v) return undefined;
    const sel = selectedPart();
    if (sel) {
      const p = v.parts.find((p) => p.id === sel);
      if (p) return p;
    }
    const f = viewer()?.frame ?? 0;
    return v.parts.find((p) => f >= p.out[0] && f < p.out[1]) ?? v.parts[0];
  });
  const selLayer = () => viewer()?.selected ?? null;
  // the layers whose numbers are unfolded (kept as the cut is read back after an edit)
  const [openNums, setOpenNums] = createSignal<ReadonlySet<string>>(new Set());
  // a hold: the whole take's screen on a stream frame (may be outside the take)
  const [holdS, setHoldS] = createSignal("");

  const place = (p: PartView, l: LayerView, k: 0 | 1 | 2, val: number) => {
    const v = cutView();
    if (!v) return;
    const next: [number, number, number] = [l.place[0], l.place[1], l.place[2]];
    next[k] = val;
    void attempt(() => actions.placePx(v.project, v.cut, v.rev, p.plan_id, l.id, next[0], next[1], next[2], groupFor(p.id, l.model)));
  };
  const crop = (p: PartView, l: LayerView, k: 0 | 1 | 2 | 3, val: number) => {
    const v = cutView();
    if (!v) return;
    const next: [number, number, number, number] = [...l.crop];
    next[k] = val;
    void attempt(() => actions.cropPx(v.project, v.cut, v.rev, p.plan_id, l.id, ...next, groupFor(p.id, l.model)));
  };

  const nudge = (p: PartView, edge: "in" | "out", delta: number) => {
    const v = cutView();
    if (v) void attempt(() => actions.trim(v.project, v.cut, p.id, edge, delta));
  };

  return (
    <aside class="inspector" aria-label="Inspector">
      <Show when={cutView()?.picture_offset_s != null}>
        <PictureOffset />
      </Show>
      {/* decision 0046: the one-click look on a recording, and the section picked on the lanes */}
      <ScreenStudioLook />
      <SectionControls />
      <Show
        when={part()}
        fallback={
          <div class="empty-state compact">
            <p class="empty">Pick a part on the timeline to see it here.</p>
            <Show when={cutView()?.parts[0]}>
              {(p0) => <button type="button" class="small" onClick={() => setSelectedPart(p0().id)}>Show part 1</button>}
            </Show>
          </div>
        }
      >
        {(p) => (
          <>
            <header class="insp-head insp-part" data-testid="insp-part">
              <h2>Part {p().label}</h2>
              <span class="pill">{p().role ?? "part"}</span>
              <span class="muted small insp-when">
                {clock(p().out[0] / rate(cutView()?.fps))}–{clock(p().out[1] / rate(cutView()?.fps))} · {((p().out[1] - p().out[0]) / rate(cutView()?.fps)).toFixed(1)} s
              </span>
            </header>
            <p class="muted small insp-src">From {clock(p().src[0] / rate(cutView()?.fps))}–{clock(p().src[1] / rate(cutView()?.fps))} of its video ({p().source})</p>
            {/* the part's look onto others: the most asked-for thing here (AJ 2026-10-19) */}
            <div class="insp-apply" role="group" aria-label="Use this part's layout and crops on" data-testid="insp-apply">
              <span class="small">Use this part's layout &amp; crops on</span>
              <div class="row">
                <Button size="sm" variant="secondary" data-testid="insp-apply-video" tooltip="Every part cut from the same video as this one (one undo)"
                  onClick={() => { const v = cutView(); if (v) void editing(attempt(() => actions.layoutCopySameSource(v.project, v.cut, p().id))).then((r) => r && say(`Part ${p().n}'s layout and crops are on ${r.parts} more part${r.parts === 1 ? "" : "s"} from the same video — ⌘Z undoes it`)); }}>
                  This video's parts
                </Button>
                <Button size="sm" variant="ghost" data-testid="insp-apply-all" tooltip="Every part of the cut (one undo)"
                  onClick={() => { const v = cutView(); if (v) void editing(attempt(() => actions.layoutApplyAll(v.project, v.cut, p().id))).then((r) => r && say(`Part ${p().n}'s layout and crops are on every part — ⌘Z undoes it`)); }}>
                  All parts
                </Button>
                <Button size="sm" variant="ghost" data-testid="insp-apply-next" tooltip="The part after this one"
                  onClick={() => { const v = cutView(); if (v) void editing(attempt(() => actions.layoutCopyNext(v.project, v.cut, p().id, 1))).then((r) => r && say(`Part ${p().n}'s layout is on the next part — ⌘Z undoes it`)); }}>
                  Next part
                </Button>
              </div>
            </div>
            <LayoutLink part={p()} />
            <CameraBubble part={p()} />
            <h3>Layers <span class="muted small">front first</span></h3>
            <Show when={layerGroup().length > 1}>
              <div class="insp-group row small" role="status" data-testid="insp-group">
                <span>
                  {roleName(cutView()?.parts.flatMap((q) => q.layers).find((x) => x.model === layerGroup()[0]!.layer)?.role ?? "layer")} grouped on {new Set(layerGroup().map((m) => m.part)).size} parts: a crop or place set on one is set on all
                </span>
                <Button size="sm" variant="ghost" icon="x" data-testid="insp-ungroup" onClick={() => setLayerGroup([])}>Ungroup</Button>
              </div>
            </Show>
            <For each={[...p().layers].sort((a, b) => b.z - a.z)} fallback={<p class="empty">This part has no layers.</p>}>
              {(l) => {
                const isSel = () => selLayer()?.[0] === p().plan_id && selLayer()?.[1] === l.id;
                const grouped = () => !!groupFor(p().id, l.model);
                return (
                  <section class={`layer${isSel() ? " sel" : ""}${l.visible ? "" : " off"}${grouped() ? " grouped" : ""}`} aria-label={`Layer ${l.role}`}>
                    <header>
                      <button type="button" class="link layer-name" data-tip="Select it on the preview" onClick={() => void attempt(() => actions.viewerSelect(p().plan_id, l.id))}>{roleName(l.role)}</button>
                      <span class="layer-actions">
                        <IconButton icon="pointer" size="sm" label={`Select ${roleName(l.role)} on the preview to move or resize it`} pressed={isSel()} data-testid="layer-select"
                          onClick={() => void attempt(() => actions.viewerSelect(p().plan_id, l.id))} />
                        <Button size="sm" variant="secondary" icon="crop" data-testid="layer-edit-crop" tooltip="Edit this layer's crop on the video (or double-click the preview)" onClick={() => askCrop(p().id, l.role)}>Edit crop</Button>
                        <IconButton icon={l.visible ? "eye" : "eye-off"} size="sm" label={l.visible ? `Hide ${roleName(l.role)}` : `Show ${roleName(l.role)}`} pressed={l.visible} disabled={!l.model}
                          onClick={() => { const v = cutView(); const m = l.model; if (v && m) void attempt(() => actions.layerVisible(v.project, m, !l.visible)); }} />
                        <Show when={l.addition && l.model}>
                          <IconButton icon="x" size="sm" label={`Remove this ${l.role} inset`} data-testid="inset-remove" onClick={() => { const v = cutView(); const m = l.model; if (v && m) void attempt(() => actions.insetDelete(v.project, v.cut, m)); }} />
                        </Show>
                      </span>
                    </header>
                    <p class="layer-summary small">{layerSummary(l, cutView()?.canvas)}</p>
                    <details class="layer-numbers" open={!!l.addition || openNums().has(l.role)}
                      onToggle={(e) => { const on = e.currentTarget.open; setOpenNums((m) => { const n = new Set(m); if (on) n.add(l.role); else n.delete(l.role); return n; }); }}>
                      <summary class="small muted">Numbers <span class="from-row"><span>crop</span><FromTag from={l.from} field="crop" /><span>place</span><FromTag from={l.from} field="place" /></span></summary>
                      <div class="num-caption small muted">Crop: the part of the video it shows (video pixels)</div>
                      <div class="grid4" role="group" aria-label="Crop (source pixels)">
                        <Num label="x" value={l.crop[0]} onCommit={(v) => crop(p(), l, 0, v)} />
                        <Num label="y" value={l.crop[1]} onCommit={(v) => crop(p(), l, 1, v)} />
                        <Num label="w" value={l.crop[2]} onCommit={(v) => crop(p(), l, 2, v)} />
                        <Num label="h" value={l.crop[3]} onCommit={(v) => crop(p(), l, 3, v)} />
                      </div>
                      <div class="num-caption small muted">Place: where it sits in the frame (frame pixels; height follows the crop's shape)</div>
                      <div class="grid4" role="group" aria-label="Place (canvas pixels)">
                        <Num label="x" value={l.place[0]} onCommit={(v) => place(p(), l, 0, v)} />
                        <Num label="y" value={l.place[1]} onCommit={(v) => place(p(), l, 1, v)} />
                        <Num label="w" value={l.place[2]} onCommit={(v) => place(p(), l, 2, v)} />
                        <Num label="h" value={l.place[3]} readonly onCommit={() => undefined} />
                      </div>
                    </details>
                  </section>
                );
              }}
            </For>
            <LayerLook part={p()} />
            <div class="row">
              <button type="button" onClick={() => { const v = cutView(); const f = viewer()?.frame; if (v && f !== undefined) void attempt(() => actions.split(v.project, v.cut, f)); }}>Split at playhead</button>
              <button type="button" class="danger" onClick={() => { const v = cutView(); if (v) { void attempt(() => actions.deletePart(v.project, p().id)); setSelectedPart(null); } }}>Delete part</button>
            </div>
            <details class="insp-timing">
              <summary class="small muted">Trim by frames and hold</summary>
              <div class="row" role="group" aria-label="Trim (keyboard)">
                <span class="muted">In</span>
                <button type="button" aria-label="Move the in point one frame earlier" onClick={() => nudge(p(), "in", -1)}>−1</button>
                <button type="button" aria-label="Move the in point one frame later" onClick={() => nudge(p(), "in", 1)}>+1</button>
                <span class="muted">Out</span>
                <button type="button" aria-label="Move the out point one frame earlier" onClick={() => nudge(p(), "out", -1)}>−1</button>
                <button type="button" aria-label="Move the out point one frame later" onClick={() => nudge(p(), "out", 1)}>+1</button>
              </div>
              <form class="row small" onSubmit={(e) => { e.preventDefault(); const v = cutView(); const t = Number(holdS()); if (v && holdS() && Number.isFinite(t)) void attempt(() => actions.holdAt(v.project, v.cut, p().id, t)).then((r) => { if (r) setHoldS(""); }); }}>
                <span class="muted">Hold the screen on stream time</span>
                <input aria-label="Hold on stream time (s)" data-testid="hold-at" type="number" step="0.001" class="short" value={holdS()} onInput={(e) => setHoldS(e.currentTarget.value)} />
                <span class="muted">s</span>
                <button type="submit" disabled={!holdS()}>Hold</button>
              </form>
            </details>
            <CursorFollow part={p()} />
            <RecordedCursor part={p()} />
            <RotoPanel part={p()} />
          </>
        )}
      </Show>
      <SoundAndCamera />
      <CaptionGallery />
      <OverlayList />
      <BrandKit />
    </aside>
  );
}

/** Story-reel parity: part gain, gain over a span (lift a quiet on-screen voice), and the
 *  camera hidden over a span (the source camera dropped out). A span runs from a marked
 *  frame to the playhead. */
function SoundAndCamera() {
  const [sv, { refetch }] = createResource(
    () => cutView() && { p: cutView()!.project, c: cutView()!.cut, rev: cutView()!.rev },
    async (k) => (await attempt(() => actions.soundView(k.p, k.c))) ?? null,
  );
  const [mark, setMark] = createSignal<number | null>(null);
  const [db, setDb] = createSignal(10);
  const [musicPath, setMusicPath] = createSignal("");
  // a speed ramp (decision 0033): to this speed, over this long, from the playhead
  const [rampTo, setRampTo] = createSignal(2);
  const [rampMs, setRampMs] = createSignal(500);
  const [rampCurve, setRampCurve] = createSignal<"linear" | "ease_in" | "ease_out" | "ease_in_out">("ease_in_out");
  const fps = () => rate(cutView()?.fps);
  const span = (): [number, number] | null => {
    const m = mark();
    const f = viewer()?.frame;
    if (m === null || f === undefined || f === m) return null;
    return [Math.min(m, f), Math.max(m, f)];
  };
  const secs = (f: number | null) => (f === null ? "?" : (f / fps()).toFixed(2));
  const run = async (fn: () => Promise<unknown>) => {
    if (await attempt(fn)) void refetch();
  };
  const part = () => {
    const v = cutView();
    const f = viewer()?.frame ?? 0;
    const p = v?.parts.find((x) => f >= x.out[0] && f < x.out[1]);
    return p ? sv()?.parts.find((x) => x.id === p.id) : undefined;
  };
  // the take under the playhead (its output frames)
  const take = () => {
    const v = cutView();
    const f = viewer()?.frame ?? 0;
    return v?.parts.find((x) => f >= x.out[0] && f < x.out[1]);
  };
  return (
    <section class="sound" aria-label="Sound and camera" data-testid="inspector-sound">
      <h3>Sound and camera</h3>
      <Show when={part()}>
        {(p) => (
          <>
            <label class="row small">Part gain
              <input aria-label="Part gain (dB)" type="number" step="0.5" min="-60" max="24" value={p().gain_db}
                onChange={(e) => { const v = cutView(); if (v) void run(() => actions.partGain(v.project, p().id, Number(e.currentTarget.value))); }} />
              dB
            </label>
            <Show when={p().under}>
              {(u) => (
                <div class="row small" role="group" aria-label="System audio" data-testid="under-row"
                  data-tip="The recording's system audio under your voice: this part's level, else the recording's own. −60 dB is silent">
                  <span>System audio</span>
                  <input aria-label="System audio under the voice (dB)" data-testid="under-db" type="number" step="1" min="-60" max="12"
                    value={u().part_db ?? u().member_db ?? -60}
                    onChange={(e) => { const v = cutView(); if (v) void run(() => actions.partUnder(v.project, [p().id], Number(e.currentTarget.value))); }} />
                  <span class="muted">dB{u().part_db == null ? " (the recording's)" : " on this part"}</span>
                  <Show when={u().part_db != null}>
                    <Button size="sm" variant="ghost" data-testid="under-all" onClick={() => {
                      const v = cutView(); const db = u().part_db ?? null;
                      if (v) void run(async () => { await actions.syncUnder(v.project, u().member, db); return actions.partUnder(v.project, [p().id], null); });
                    }}>Use on every part</Button>
                  </Show>
                </div>
              )}
            </Show>
            <label class="row small" data-tip="Faster or slower: the sound keeps its pitch from ½× to 2× and is muted beyond; captions follow">Speed
              <select aria-label="Part speed" data-testid="part-speed" value={String(p().speed ?? 1)}
                onChange={(e) => { const v = cutView(); if (v) void run(async () => {
                  const r = await actions.partSpeed(v.project, p().id, Number(e.currentTarget.value));
                  if (r && !r.sound) say("Past 2× either way the part is silent (its sound would smear) — ⌘Z undoes it");
                  return r;
                }); }}>
                <For each={[0.25, 0.5, 0.75, 1, 1.25, 1.5, 2, 3, 4]}>{(s) => <option value={String(s)}>{s}×</option>}</For>
              </select>
            </label>
            <div class="row small ramp-row" role="group" aria-label="Speed ramp" data-tip="Ease the speed from the part's own to another, starting at the playhead; the part plays at the new speed after it. Sound keeps its pitch while both speeds are ½× to 2×">
              <Show when={p().ramp} fallback={
                <>
                  <span>Ramp to</span>
                  <select aria-label="Ramp to speed" data-testid="ramp-to" value={String(rampTo())} onChange={(e) => setRampTo(Number(e.currentTarget.value))}>
                    <For each={[0.25, 0.5, 0.75, 1, 1.25, 1.5, 2, 3, 4]}>{(s) => <option value={String(s)}>{s}×</option>}</For>
                  </select>
                  <span>over</span>
                  <input aria-label="Ramp length (ms)" data-testid="ramp-ms" class="ramp-ms" type="number" min="40" step="100" value={rampMs()} onInput={(e) => setRampMs(Number(e.currentTarget.value))} />
                  <span>ms</span>
                  <select aria-label="Ramp curve" data-testid="ramp-curve" value={rampCurve()} onChange={(e) => setRampCurve(e.currentTarget.value as "linear" | "ease_in" | "ease_out" | "ease_in_out")}>
                    <option value="ease_in_out">ease in-out</option>
                    <option value="ease_in">ease in</option>
                    <option value="ease_out">ease out</option>
                    <option value="linear">linear</option>
                  </select>
                  <Button size="sm" icon="activity" data-testid="ramp-add" onClick={() => {
                    const v = cutView(); const t = take(); const f = viewer()?.frame ?? 0;
                    if (!v || !t) return;
                    const startMs = Math.max(0, Math.round(((f - t.out[0]) / fps()) * 1000));
                    void run(async () => {
                      const r = await actions.partRamp(v.project, p().id, { to: rampTo(), start_ms: startMs, dur_ms: Math.round(rampMs()), curve: rampCurve() });
                      if (r && !r.sound) say("The ramp reaches past ½×–2×, so the part is silent (its sound would smear) — ⌘Z undoes it");
                      return r;
                    });
                  }}>Ramp from here</Button>
                </>
              }>
                {(r) => (
                  <>
                    <span data-testid="ramp-now">Ramps {p().speed ?? 1}× → {r().to}× ({r().curve.replace(/_/g, " ")})</span>
                    <Button size="sm" variant="ghost" icon="x" data-testid="ramp-remove" onClick={() => { const v = cutView(); if (v) void run(() => actions.partRamp(v.project, p().id, null)); }}>Remove ramp</Button>
                  </>
                )}
              </Show>
            </div>
          </>
        )}
      </Show>
      <Show when={take()}>
        {(t) => {
          const covering = (rows: { id: string; from: number | null; to: number | null }[] | undefined) =>
            (rows ?? []).find((r) => r.from !== null && r.to !== null && r.from <= t().out[0] && r.to >= t().out[1]);
          const exact = (r: { from: number | null; to: number | null } | undefined) => !!r && r.from === t().out[0] && r.to === t().out[1];
          const hide = () => covering(sv()?.hides);
          const quiet = () => covering(sv()?.no_captions);
          return (
            <div class="row small" role="group" aria-label="This take">
              <label class="check" title={hide() && !exact(hide()) ? "Part of a longer span (listed below)" : "Hide the camera for this whole take"}>
                <input type="checkbox" data-testid="take-hide-cam" checked={!!hide()} disabled={!!hide() && !exact(hide())}
                  onChange={(e) => { const v = cutView(); if (!v) return; const h = hide(); void run(() => (e.currentTarget.checked ? actions.camHide(v.project, v.cut, t().out[0], t().out[1]) : actions.camHideDelete(v.project, v.cut, h!.id))); }} />
                Camera hidden on this take
              </label>
              <label class="check" title={quiet() && !exact(quiet()) ? "Part of a longer span (listed below)" : "No captions over this whole take"}>
                <input type="checkbox" data-testid="take-no-captions" checked={!!quiet()} disabled={!!quiet() && !exact(quiet())}
                  onChange={(e) => { const v = cutView(); if (!v) return; const q = quiet(); void run(() => (e.currentTarget.checked ? actions.noCaptions(v.project, v.cut, t().out[0], t().out[1]) : actions.noCaptionsDelete(v.project, v.cut, q!.id))); }} />
                No captions on this take
              </label>
            </div>
          );
        }}
      </Show>
      <div class="row small">
        <button type="button" class="ghost" data-testid="mark-span" onClick={() => setMark(viewer()?.frame ?? null)}>Mark start here</button>
        <span class="muted">{span() ? `${secs(span()![0])}–${secs(span()![1])} s (to the playhead)` : mark() !== null ? "now move the playhead to the span's end" : "mark a span, then move the playhead"}</span>
      </div>
      <div class="row small">
        <input aria-label="Span gain (dB)" type="number" step="1" min="-40" max="24" value={db()} onInput={(e) => setDb(Number(e.currentTarget.value))} />
        <button type="button" disabled={!span()} data-testid="gain-span" onClick={() => { const v = cutView(); const s = span(); if (v && s) void run(() => actions.gainSpan(v.project, v.cut, s[0], s[1], db())).then(() => setMark(null)); }}>Gain this span</button>
        <button type="button" disabled={!span()} data-testid="no-captions" onClick={() => { const v = cutView(); const s = span(); if (v && s) void run(() => actions.noCaptions(v.project, v.cut, s[0], s[1])).then(() => setMark(null)); }}>No captions here</button>
        <button type="button" disabled={!span()} data-testid="hide-cam" onClick={() => { const v = cutView(); const s = span(); if (v && s) void run(() => actions.camHide(v.project, v.cut, s[0], s[1])).then(() => setMark(null)); }}>Hide the camera here</button>
      </div>
      <For each={sv()?.gains ?? []}>
        {(g) => (
          <p class="row small" data-testid="gain-row">
            <span>{g.gain_db > 0 ? "+" : ""}{g.gain_db} dB · {secs(g.from)}–{secs(g.to)} s</span>
            <button type="button" class="ghost" aria-label="Remove this gain" onClick={() => { const v = cutView(); if (v) void run(() => actions.gainSpanDelete(v.project, g.id)); }}>✕</button>
          </p>
        )}
      </For>
      <div class="row small" role="group" aria-label="Music" data-tip="Music or a sound effect over the whole cut (M4A or WAV): −18 dB, ducked under your voice">
        <input aria-label="Music file (M4A or WAV)" data-testid="music-path" placeholder="Music file path (.m4a, .wav)" value={musicPath()} onInput={(e) => setMusicPath(e.currentTarget.value)} />
        <button type="button" disabled={!musicPath().trim()} data-testid="music-add" onClick={() => { const v = cutView(); if (v) void run(() => actions.musicAdd(v.project, v.cut, musicPath().trim())).then(() => setMusicPath("")); }}>Add music</button>
      </div>
      <For each={sv()?.sounds ?? []}>
        {(m) => (
          <p class="row small" data-testid="sound-row">
            <span>♪ {m.name} · {secs(m.from)}–{secs(m.to)} s</span>
            <input aria-label="Music gain (dB)" type="number" step="1" min="-60" max="12" value={m.gain_db}
              onChange={(e) => { const v = cutView(); if (v) void run(() => actions.soundSet(v.project, m.id, { gain_db: Number(e.currentTarget.value) })); }} />
            <label class="check">
              <input type="checkbox" data-testid="sound-duck" checked={m.duck}
                onChange={(e) => { const v = cutView(); if (!v) return; const on = e.currentTarget.checked; void run(() => actions.soundSet(v.project, m.id, on ? { duck: { depth_db: 12, attack_ms: 200, release_ms: 600 } } : { no_duck: true })); }} />
              Duck under the voice
            </label>
            <button type="button" class="ghost" aria-label="Remove this music" onClick={() => { const v = cutView(); if (v) void run(() => actions.soundRemove(v.project, m.id)); }}>✕</button>
          </p>
        )}
      </For>
      <For each={sv()?.no_captions ?? []}>
        {(q) => (
          <p class="row small" data-testid="quiet-row">
            <span>No captions · {secs(q.from)}–{secs(q.to)} s</span>
            <button type="button" class="ghost" aria-label="Captions here again" onClick={() => { const v = cutView(); if (v) void run(() => actions.noCaptionsDelete(v.project, v.cut, q.id)); }}>✕</button>
          </p>
        )}
      </For>
      <For each={sv()?.hides ?? []}>
        {(h) => (
          <p class="row small" data-testid="hide-row">
            <span>Camera hidden · {secs(h.from)}–{secs(h.to)} s</span>
            <button type="button" class="ghost" aria-label="Show the camera again" onClick={() => { const v = cutView(); if (v) void run(() => actions.camHideDelete(v.project, v.cut, h.id)); }}>✕</button>
          </p>
        )}
      </For>
    </section>
  );
}
