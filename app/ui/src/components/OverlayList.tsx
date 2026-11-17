// Images and callouts on the cut (inspector): add a box or an arrow at the playhead, paste
// or drop an image on the viewer, then place each by numbers (% of the canvas). Every edit
// is one command.
import { createResource, createSignal, For, onMount, Show } from "solid-js";
import { actions, rate, type CoverRow, type OverlayRow, type PlateRow } from "../actions";
import { attempt, cutView, say, viewer } from "../store";
import { Button, Empty, IconButton, Panel, Switch, tip } from "../design/components";
import { Icon } from "../design/Icon";
import "../look.css";

function Pct(props: { label: string; value: number; onCommit: (v: number) => void }) {
  return (
    <label class="num" {...tip(`${props.label} (% of the canvas)`)}>
      <span>{props.label}</span>
      <input type="number" step="0.5" value={Math.round(props.value * 1000) / 10} data-value={Math.round(props.value * 1000) / 10}
        onChange={(e) => { const v = Number(e.currentTarget.value); if (Number.isFinite(v)) props.onCommit(v / 100); }} />
    </label>
  );
}

export function ImageThumb(props: { project: string; asset: string }) {
  let el!: HTMLCanvasElement;
  onMount(() => {
    void attempt(() => actions.imageThumb(props.project, props.asset)).then((t) => {
      if (!t) return;
      el.width = t.w;
      el.height = t.h;
      const bin = atob(t.rgb);
      const px = new Uint8ClampedArray(t.w * t.h * 4);
      for (let i = 0; i < t.w * t.h; i++) {
        px[i * 4] = bin.charCodeAt(i * 3);
        px[i * 4 + 1] = bin.charCodeAt(i * 3 + 1);
        px[i * 4 + 2] = bin.charCodeAt(i * 3 + 2);
        px[i * 4 + 3] = 255;
      }
      el.getContext("2d")?.putImageData(new ImageData(px, t.w, t.h), 0, 0);
    });
  });
  return <canvas ref={el} class="ov-thumb" width={1} height={1} aria-hidden="true" />;
}

export function OverlayList() {
  const key = () => {
    const v = cutView();
    return v ? { p: v.project, c: v.cut, rev: v.rev } : null;
  };
  const [view, { refetch }] = createResource(key, async (k) => (await attempt(() => actions.overlaysView(k.p, k.c))) ?? null);
  const list = () => view()?.overlays ?? [];
  const covers = (): CoverRow[] => view()?.covers ?? [];
  const blurred = () => view()?.background === "blurred_source";
  // decision 0028: a backdrop (a video or still under every part) and its settings
  const backdrop = () => {
    const b = view()?.background as { backdrop?: { asset: string; loop: boolean; fit: "cover" | "contain" } } | null | undefined;
    return b && typeof b === "object" && b.backdrop ? b.backdrop : null;
  };
  const [mediaPath, setMediaPath] = createSignal("");
  // decision 0029: background plates from the style library (built-in, the project's, mine)
  const [plates, { refetch: refetchPlates }] = createResource(key, async (k) => (await attempt(() => actions.platesView(k.p, k.c))) ?? null);
  const plateRows = () => {
    const p = plates();
    if (!p) return [] as (PlateRow & { mine: boolean })[];
    const out: (PlateRow & { mine: boolean })[] = p.project.map((r) => ({ ...r, mine: false }));
    for (const r of p.mine) if (!out.some((x) => x.name === r.name)) out.push({ ...r, mine: true });
    return out;
  };
  const [plateTop, setPlateTop] = createSignal("#0c1118");
  const [plateBottom, setPlateBottom] = createSignal("#1b2a4a");
  const [plateName, setPlateName] = createSignal("");
  const applyPlate = async (name: string) => {
    const v = cutView();
    const f = v?.framing?.active;
    if (!v || !f) return;
    if (!name) {
      await run(() => actions.backgroundSet(v.project, f, { solid: { colour: "#0c1118" } }));
    } else {
      const row = plateRows().find((r) => r.name === name);
      await run(() => actions.plateApply(v.project, f, name, row?.mine ?? false));
    }
    void refetchPlates();
  };
  const savePlate = async () => {
    const v = cutView();
    const f = v?.framing?.active;
    const n = plateName().trim();
    if (!v || !f || !n) return;
    if (await attempt(() => actions.plateSave(v.project, n, [plateTop(), plateBottom()]))) {
      await run(() => actions.plateApply(v.project, f, n, false));
      setPlateName("");
      void refetchPlates();
    }
  };
  const pickOr = async (typed: string) => {
    const t = typed.trim();
    if (t) return t;
    const r = await attempt(() => actions.pickVideo());
    return r?.picked ?? null;
  };
  const chooseBackdrop = async () => {
    const v = cutView();
    if (!v) return;
    const p = await pickOr(mediaPath());
    if (p) {
      const b = backdrop();
      void run(() => actions.backdropSet(v.project, v.cut, p, b?.loop ?? true, b?.fit ?? "cover"));
      setMediaPath("");
    }
  };
  const addVideo = async () => {
    const v = cutView();
    if (!v) return;
    const p = await pickOr(mediaPath());
    if (p) {
      void run(() => actions.videoOverlayAdd(v.project, v.cut, viewer()?.frame ?? 0, p));
      setMediaPath("");
    }
  };
  const backdropTweak = (patch: Partial<{ loop: boolean; fit: "cover" | "contain" }>) => {
    const v = cutView();
    const f = v?.framing?.active;
    const b = backdrop();
    if (v && f && b) void run(() => actions.backgroundSet(v.project, f, { backdrop: { ...b, ...patch } }));
  };
  const backdropOff = () => {
    const v = cutView();
    const f = v?.framing?.active;
    if (v && f) void run(() => actions.backgroundSet(v.project, f, { solid: { colour: "#0c1118" } }));
  };
  const setBackground = (on: boolean) => {
    const v = cutView();
    const f = v?.framing?.active;
    if (v && f) void run(() => actions.backgroundSet(v.project, f, on ? "blurred_source" : { solid: { colour: "#0c1118" } }));
  };
  const run = async (fn: () => Promise<unknown>) => {
    if (await attempt(fn)) void refetch();
  };
  const [naming, setNaming] = createSignal<string | null>(null);
  const [tname, setTname] = createSignal("");
  const fps = () => rate(cutView()?.fps);
  const kindLabel = (o: OverlayRow) =>
    o.shape ? (o.shape.kind === "arrow" ? "Arrow" : "Box")
      : o.kind === "image" ? "Image"
      : o.kind === "video" ? "Video"
      : ({ text_hook: "Hook", section_pill: "Pill", run_clock: "Run clock" } as Record<string, string>)[o.role ?? ""] ?? (o.band === "lower_third" ? "Lower third" : o.band === "end_card" ? "End card" : "Text");
  const secs = (o: OverlayRow) => (o.out ? `${(o.out[0] / fps()).toFixed(1)}–${(o.out[1] / fps()).toFixed(1)} s` : o.label ? `“${o.label}”` : "");
  const add = (kind: "box" | "arrow") => {
    const v = cutView();
    if (v) void run(() => actions.calloutAdd(v.project, v.cut, viewer()?.frame ?? 0, kind));
  };
  const setRect = (o: OverlayRow, k: "x" | "y" | "w" | "h", val: number) => {
    const v = cutView();
    if (!v) return;
    const r = { x: o.geom.x, y: o.geom.y, w: o.geom.w, h: o.geom.h ?? 0.1, [k]: val };
    void run(() => actions.overlayGeom(v.project, o.id, { rect: r }));
  };
  const setPlace = (o: OverlayRow, k: "x" | "y" | "w", val: number) => {
    const v = cutView();
    if (!v) return;
    void run(() => actions.overlayPlace(v.project, o.id, { x: o.geom.x, y: o.geom.y, w: o.geom.w, [k]: val }));
  };
  // decision 0042: where an image or video sits among the picture's layers
  const layerName = (role: string) => ({ cam: "the camera", face: "the camera", screen: "the screen", detail: "the detail" } as Record<string, string>)[role.replace(/-\d+$/, "")] ?? role;
  const stackOptions = (o: OverlayRow) => {
    const ls = view()?.picture_layers ?? [];
    const opts: { value: string; label: string }[] = [{ value: "", label: "In front of the picture" }];
    ls.forEach((l, i) => opts.push({ value: String(l.z), label: i === 0 ? "Behind the whole picture" : `Behind ${layerName(l.role)}` }));
    if (o.under != null && !ls.some((l) => l.z === o.under)) opts.push({ value: String(o.under), label: `Behind layer ${o.under}` });
    return opts;
  };
  const setStack = (o: OverlayRow, value: string) => {
    const v = cutView();
    if (v) void run(() => actions.overlayStackSet(v.project, o.id, value === "" ? null : Number(value)));
  };
  const setEnd = (o: OverlayRow, end: "from" | "to", k: 0 | 1, val: number) => {
    const v = cutView();
    const shape = o.shape;
    if (!v || !shape) return;
    const p: [number, number] = [...(shape[end] ?? [0.5, 0.5])];
    p[k] = val;
    void run(() => actions.overlayGeom(v.project, o.id, { shape: { ...shape, [end]: p } }));
  };
  return (
    <Panel
      class="look"
      testid="overlay-list"
      label="Images and callouts"
      title="Images and callouts"
      actions={
        <>
          <Button variant="ghost" size="sm" icon="plus" data-testid="callout-box" tooltip="A box at the playhead (3 s)" onClick={() => add("box")}>Box</Button>
          <Button variant="ghost" size="sm" icon="pointer" data-testid="callout-arrow" tooltip="An arrow at the playhead (3 s)" onClick={() => add("arrow")}>Arrow</Button>
          <Button variant="ghost" size="sm" icon="plus" data-testid="cover-add" tooltip="Blur the picture at the canvas centre on the playhead's take (pin it to motion below to follow a face)" onClick={() => { const v = cutView(); if (v) void run(() => actions.coverAdd(v.project, v.cut, viewer()?.frame ?? 0)); }}>Blur</Button>
          <Button variant="ghost" size="sm" icon="plus" data-testid="video-overlay-add" tooltip="A video picture-in-picture at the playhead, silent (the path below, or choose one)" onClick={() => void addVideo()}>Video</Button>
        </>
      }
    >
      <Switch label="Blurred background" testid="blurred-fill" on={blurred()} disabled={!cutView()?.framing?.active}
        hint="Fill the frame behind the layers with the part's own picture, blurred and dimmed" onChange={setBackground} />
      <div class="look-inline" data-testid="plate-row">
        <label class="small" for="plate-pick">Plate</label>
        <select id="plate-pick" aria-label="Background plate" data-testid="plate-pick" value={plates()?.current ?? ""}
          disabled={!cutView()?.framing?.active} onChange={(e) => void applyPlate(e.currentTarget.value)}>
          <option value="">None</option>
          <For each={plateRows()}>{(r) => <option value={r.name}>{r.name}{r.mine ? " (mine)" : ""}</option>}</For>
        </select>
      </div>
      <form class="look-inline" data-testid="plate-new" onSubmit={(e) => { e.preventDefault(); void savePlate(); }}>
        <input type="color" aria-label="Plate top colour" data-testid="plate-top" value={plateTop()} onInput={(e) => setPlateTop(e.currentTarget.value)} />
        <input type="color" aria-label="Plate bottom colour" data-testid="plate-bottom" value={plateBottom()} onInput={(e) => setPlateBottom(e.currentTarget.value)} />
        <input aria-label="Plate name" data-testid="plate-name" maxLength={60} placeholder="New plate name" value={plateName()} onInput={(e) => setPlateName(e.currentTarget.value)} />
        <Button type="submit" size="sm" data-testid="plate-save" disabled={!plateName().trim() || !cutView()?.framing?.active}
          tooltip="Save this gradient as a plate (here and in my style library) and use it">Save plate</Button>
      </form>
      <div class="look-inline backdrop-row" data-testid="backdrop-row" data-on={backdrop() ? "1" : "0"}>
        <input aria-label="Video or picture path" data-testid="media-path" placeholder="A video or picture path (or choose one)"
          value={mediaPath()} onInput={(e) => setMediaPath(e.currentTarget.value)}
          onKeyDown={(e) => { if (e.key === "Enter") void chooseBackdrop(); }} />
        <Button size="sm" data-testid="backdrop-set" disabled={!cutView()?.framing?.active}
          tooltip="Put a video or still under every part; it runs on across the cuts" onClick={() => void chooseBackdrop()}>Use as backdrop</Button>
      </div>
      <Show when={backdrop()}>
        {(b) => (
          <div class="look-inline" data-testid="backdrop-on">
            <span class="small" data-testid="backdrop-name">Backdrop: {view()?.backdrop?.name ?? "stored file"}</span>
            <Show when={view()?.backdrop?.video}>
              <Switch label="Loop" testid="backdrop-loop" on={b().loop} hint="Start the backdrop again when it ends (else its last frame holds)"
                onChange={(on) => backdropTweak({ loop: on })} />
            </Show>
            <select aria-label="Backdrop fit" data-testid="backdrop-fit" value={b().fit}
              onChange={(e) => backdropTweak({ fit: e.currentTarget.value as "cover" | "contain" })}>
              <option value="cover">Fill</option>
              <option value="contain">Fit</option>
            </select>
            <IconButton icon="x" size="sm" label="Remove backdrop" onClick={backdropOff} />
          </div>
        )}
      </Show>
      <Show when={covers().length}>
        <div class="cover-rows" role="group" aria-label="Covers">
          <For each={covers()}>
            {(c) => (
              <div class="ov-row" data-testid="cover-row">
                <header class="ov-head">
                  <span class="ov-kind" aria-hidden="true"><Icon name="layers" size={14} /></span>
                  <span class="ov-label">{c.style === "pixelate" ? "Pixelate" : c.style === "solid" ? "Solid cover" : "Blur"} · {(c.out[0] / fps()).toFixed(1)}–{(c.out[1] / fps()).toFixed(1)} s</span>
                  <Show when={c.tracked} fallback={
                    <IconButton icon="pointer" size="sm" label="Pin to motion" hint="Follow what is under the cover's centre from the playhead, through every take of that video it is on. Synced cameras are not followed yet" data-testid="cover-pin"
                      onClick={() => { const v = cutView(); if (v) void run(async () => {
                        const r = await actions.coverTrack(v.project, v.cut, c.id, viewer()?.frame ?? 0);
                        const takes = r && r.takes > 1 ? ` over ${r.takes} takes` : "";
                        if (r) say(r.lost ? `Cover pinned for ${r.seconds.toFixed(1)} s${takes}, then the point was lost — ⌘Z undoes it` : `Cover pinned to motion for ${r.seconds.toFixed(1)} s${takes} — ⌘Z undoes it`);
                        return r;
                      }); }} />
                  }>
                    <span class="small" data-testid="cover-pinned">Pinned</span>
                    <IconButton icon="x" size="sm" label="Unpin" hint="Stop following the motion" data-testid="cover-unpin"
                      onClick={() => { const v = cutView(); if (v) void run(() => actions.coverUnpin(v.project, c.id)); }} />
                  </Show>
                </header>
              </div>
            )}
          </For>
        </div>
      </Show>
      <Show when={list().length} fallback={<Empty compact icon="layers" title="No images or callouts" />}>
        <For each={list()}>
          {(o) => (
            <div class="ov-row" data-testid="overlay-row" data-kind={o.shape?.kind ?? o.kind}>
              <header class="ov-head">
                <Show when={o.asset} fallback={<span class="ov-kind" aria-hidden="true"><Icon name={o.shape?.kind === "arrow" ? "pointer" : o.kind === "text" ? "type" : o.kind === "video" ? "play" : "layout"} size={14} /></span>}>
                  {(a) => <ImageThumb project={cutView()!.project} asset={a()} />}
                </Show>
                <span class="ov-label">{kindLabel(o)} · {secs(o)}</span>
                <Show when={o.kind !== "video"}>
                  <Show when={o.tracked} fallback={
                    <IconButton icon="pointer" size="sm" label="Pin to motion" hint="Follow what is under it in the video (a box's centre, an arrow's tip) and move with it, through every take of that video it spans. Synced cameras are not followed yet" data-testid="track-pin"
                      onClick={() => { const v = cutView(); if (v) void run(async () => {
                        const r = await actions.overlayTrack(v.project, v.cut, o.id);
                        const takes = r && r.takes > 1 ? ` over ${r.takes} takes` : "";
                        if (r) say(r.lost ? `Pinned for ${r.seconds.toFixed(1)} s${takes}, then the point was lost — ⌘Z undoes it` : `Pinned to motion for ${r.seconds.toFixed(1)} s${takes} — ⌘Z undoes it`);
                        return r;
                      }); }} />
                  }>
                    <span class="small" data-testid="track-pinned">Pinned</span>
                    <IconButton icon="x" size="sm" label="Unpin" hint="Stop following the motion" data-testid="track-unpin"
                      onClick={() => { const v = cutView(); if (v) void run(() => actions.overlayUnpin(v.project, o.id)); }} />
                  </Show>
                </Show>
                <IconButton icon="star" size="sm" label="Save as template" hint="Save as a template in the brand kit" data-testid="template-save" onClick={() => { setNaming(o.id); setTname(kindLabel(o)); }} />
                <IconButton icon="x" size="sm" label="Remove" onClick={() => { const v = cutView(); if (v) void run(() => actions.overlayRemove(v.project, o.id)); }} />
              </header>
              <Show when={naming() === o.id}>
                <form class="look-inline" onSubmit={(e) => { e.preventDefault(); const v = cutView(); const n = tname().trim(); if (v && n) void attempt(() => actions.templateSave(v.project, o.id, n)).then((r) => { if (r) setNaming(null); }); }}>
                  <input aria-label="Template name" data-testid="template-name" maxLength={60} value={tname()} onInput={(e) => setTname(e.currentTarget.value)} />
                  <Button type="submit" size="sm" data-testid="template-name-save">Save</Button>
                </form>
              </Show>
              <Show when={o.kind === "image" || o.kind === "video"}>
                <div class="look-inline" data-testid="overlay-stack-row">
                  <select aria-label="Layer order" data-testid="overlay-stack" value={o.under == null ? "" : String(o.under)}
                    {...tip("Where it sits: in front of the picture, or behind one of its layers (e.g. behind a cut-out speaker)")}
                    onChange={(e) => setStack(o, e.currentTarget.value)}>
                    <For each={stackOptions(o)}>{(op) => <option value={op.value} selected={op.value === (o.under == null ? "" : String(o.under))}>{op.label}</option>}</For>
                  </select>
                  <Show when={o.kind === "video"}>
                    <Switch label="Cut out person" testid="overlay-matte" on={!!o.matte}
                      hint="Keep only the person in this video (no green screen); the rest shows what is behind it"
                      onChange={(on) => { const v = cutView(); if (v) void run(() => actions.overlayMatteSet(v.project, o.id, on)); }} />
                  </Show>
                </div>
                <div class="grid4" role="group" aria-label="Place">
                  <Pct label="x" value={o.geom.x} onCommit={(v) => setPlace(o, "x", v)} />
                  <Pct label="y" value={o.geom.y} onCommit={(v) => setPlace(o, "y", v)} />
                  <Pct label="w" value={o.geom.w} onCommit={(v) => setPlace(o, "w", v)} />
                </div>
              </Show>
              <Show when={o.shape?.kind === "box"}>
                <div class="grid4" role="group" aria-label="Box">
                  <Pct label="x" value={o.geom.x} onCommit={(v) => setRect(o, "x", v)} />
                  <Pct label="y" value={o.geom.y} onCommit={(v) => setRect(o, "y", v)} />
                  <Pct label="w" value={o.geom.w} onCommit={(v) => setRect(o, "w", v)} />
                  <Pct label="h" value={o.geom.h ?? 0} onCommit={(v) => setRect(o, "h", v)} />
                </div>
              </Show>
              <Show when={o.shape?.kind === "arrow"}>
                <div class="grid4" role="group" aria-label="Arrow">
                  <Pct label="from x" value={o.shape?.from?.[0] ?? 0} onCommit={(v) => setEnd(o, "from", 0, v)} />
                  <Pct label="from y" value={o.shape?.from?.[1] ?? 0} onCommit={(v) => setEnd(o, "from", 1, v)} />
                  <Pct label="to x" value={o.shape?.to?.[0] ?? 0} onCommit={(v) => setEnd(o, "to", 0, v)} />
                  <Pct label="to y" value={o.shape?.to?.[1] ?? 0} onCommit={(v) => setEnd(o, "to", 1, v)} />
                </div>
              </Show>
            </div>
          )}
        </For>
      </Show>
    </Panel>
  );
}
