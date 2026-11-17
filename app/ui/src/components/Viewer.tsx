// The viewer (SPEC D5): frames from the render thread drawn by WebGL2; pointer input goes
// back up the same socket as records (gizmo drags, scrub by wheel, transport keys).
import { createEffect, createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";
import { actions } from "../actions";
import { attempt, cutView, say, setLoopAsk, viewer } from "../store";
import { Chip, IconButton, Menu, Toggle } from "../design/components";
import { Icon } from "../design/Icon";
import { prefs, setPref } from "../prefs";
import { jumpPart } from "../keys";
import { FrameSocket, Keys, RecordKind, type Frame } from "../viewer/framesocket";
import { Nv12Renderer } from "../viewer/nv12gl";
import { logDraw, runS2a } from "../viewer/s2a";
import { imageDropHandlers, installImagePaste } from "./imageDrop";

export function timecode(frame: number, fps: number): string {
  const r = Math.max(1, fps);
  const s = Math.floor(frame / r);
  // frames into the second (29.97: a fractional rate, so whole frames past s·fps)
  const ff = Math.max(0, Math.floor(frame - s * r + 1e-6));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}.${String(ff).padStart(2, "0")}`;
}

export interface CanvasRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export function Viewer(props: { aspect?: () => number | undefined; measure?: boolean; overlay?: (r: CanvasRect) => JSX.Element; onDblClick?: () => void } = {}) {
  const [rect, setRect] = createSignal<CanvasRect>({ x: 0, y: 0, w: 0, h: 0 });
  let box!: HTMLDivElement;
  let canvas!: HTMLCanvasElement;
  const [stats, setStats] = createSignal("");
  const [status, setStatus] = createSignal<"connecting" | "live" | "refused" | "closed">("connecting");
  const [s2aLog, setS2aLog] = createSignal("");
  let fs: FrameSocket | null = null;
  let gl: Nv12Renderer | null = null;
  let pending: Frame | null = null;
  let lastDraw: { frame: Frame; drawAt: number } | null = null;
  let raf = 0;
  let frames = 0;
  let statT = performance.now();
  let lastRender = 0;
  let fps = 0;
  let statsEl: HTMLSpanElement | undefined;

  const fit = () => {
    const v = cutView();
    const aspect = props.aspect?.() ?? (v ? v.canvas.w / v.canvas.h : 9 / 16);
    const bw = box.clientWidth;
    const bh = box.clientHeight;
    let w = bw;
    let h = w / aspect;
    if (h > bh) {
      h = bh;
      w = h * aspect;
    }
    canvas.style.width = `${Math.floor(w)}px`;
    canvas.style.height = `${Math.floor(h)}px`;
    setRect({ x: (bw - Math.floor(w)) / 2, y: (bh - Math.floor(h)) / 2, w: Math.floor(w), h: Math.floor(h) });
    fs?.send(RecordKind.Viewport, { x: Math.round(w * devicePixelRatio), y: Math.round(h * devicePixelRatio) });
  };

  const tick = (now: number) => {
    raf = requestAnimationFrame(tick);
    if (lastDraw && lastDraw.drawAt < now) {
      // The first rAF after a draw: that frame is on its way to the screen.
      const f = lastDraw.frame;
      logDraw({ frame: f.outFrame, w: f.w, h: f.h, echoT: f.echoT, renderUs: f.renderUs, dropped: f.dropped, receivedAt: f.receivedAt, drawAt: lastDraw.drawAt, photonAt: now });
      lastDraw = null;
    }
    if (!pending || !gl) return;
    const f = pending;
    pending = null;
    gl.upload(f.w, f.h, f.y, f.uv);
    gl.draw();
    lastDraw = { frame: f, drawAt: performance.now() };
    frames++;
    lastRender = f.renderUs;
    // Size and render time are the frame just drawn (a step while paused shows its own
    // numbers); the rate is averaged over the last second.
    if (now - statT > 1000) {
      fps = Math.round((frames * 1000) / (now - statT));
      frames = 0;
      statT = now;
    }
    setStats(`${f.w}×${f.h} · ${fps} fps · render ${(lastRender / 1000).toFixed(1)} ms`);
    if (statsEl) statsEl.dataset.frame = String(f.outFrame);
  };

  // A cut with another canvas aspect refits the box (and tells the render thread).
  // (the store reconciles in place: read the numbers, not the object, so a framing switch
  // that changes the canvas size refits the frame)
  createEffect(() => {
    void cutView()?.canvas?.w;
    void cutView()?.canvas?.h;
    void props.aspect?.();
    if (box) fit();
  });

  onMount(async () => {
    // Cleanups are registered before any await (Solid has no owner after one).
    let off: (() => void) | undefined;
    onCleanup(() => off?.());
    // A closed socket (the window was hidden for a minute) comes back by itself once the
    // core re-arms it on show (decision 0010); the core redraws the current frame.
    let gone = false;
    onCleanup(() => {
      gone = true;
    });
    onCleanup(FrameSocket.onClose(() => {
      setStatus("closed");
      off?.();
      void FrameSocket.reconnect(() => gone).then((s) => {
        if (!s || gone) return;
        fs = s;
        off = s.onFrame((f) => {
          pending = f;
        });
        setStatus("live");
        fit();
      });
    }));
    try {
      gl = new Nv12Renderer(canvas);
    } catch (e) {
      say(`The viewer cannot draw: ${String(e)}`, "error");
      return;
    }
    raf = requestAnimationFrame(tick);
    const ro = new ResizeObserver(fit);
    ro.observe(box);
    onCleanup(() => ro.disconnect());
    canvas.addEventListener("webglcontextlost", (e) => {
      e.preventDefault();
      say("The preview lost its GPU context; reopen the cut to restore it.", "error");
    });
    try {
      fs = await FrameSocket.get();
      setStatus("live");
      off = fs.onFrame((f) => {
        pending = f;
      });
      fit();
    } catch {
      setStatus("refused");
    }
  });
  onCleanup(() => cancelAnimationFrame(raf));
  onCleanup(installImagePaste());

  const norm = (e: PointerEvent) => {
    const r = canvas.getBoundingClientRect();
    return { x: (e.clientX - r.left) / r.width, y: (e.clientY - r.top) / r.height };
  };
  const mods = (e: { shiftKey: boolean; altKey: boolean; ctrlKey: boolean; metaKey: boolean }) =>
    (e.shiftKey ? 1 : 0) | (e.altKey ? 2 : 0) | (e.ctrlKey ? 4 : 0) | (e.metaKey ? 8 : 0);
  const ptype = (e: PointerEvent) => (e.pointerType === "pen" ? 1 : e.pointerType === "touch" ? 2 : 0);

  const onDown = (e: PointerEvent) => {
    canvas.setPointerCapture(e.pointerId);
    fs?.send(RecordKind.Down, { ...norm(e), buttons: e.buttons, mods: mods(e), pointer: ptype(e), t: e.timeStamp });
  };
  const onMove = (e: PointerEvent) => {
    if (!e.buttons) return;
    fs?.send(RecordKind.Move, { ...norm(e), buttons: e.buttons, mods: mods(e), pointer: ptype(e), t: e.timeStamp });
  };
  const onUp = (e: PointerEvent) => fs?.send(RecordKind.Up, { ...norm(e), mods: mods(e), pointer: ptype(e), t: e.timeStamp });
  const onCancel = (e: PointerEvent) => fs?.send(RecordKind.Cancel, { t: e.timeStamp });
  const onWheel = (e: WheelEvent) => {
    e.preventDefault();
    fs?.send(RecordKind.Wheel, { dx: e.deltaX, dy: e.deltaY, t: e.timeStamp });
  };
  const onKey = (e: KeyboardEvent) => {
    // Esc during a drag: R-GEST-1 cancel (the layer goes back, nothing is written)
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      fs?.send(RecordKind.Cancel, { t: e.timeStamp });
      return;
    }
    // ⌥-arrows nudge a part edge (the global keys); ⌘ is the menus'
    if (e.altKey || e.metaKey || e.ctrlKey) return;
    const code = e.key === " " ? Keys.PlayPause : e.key === "ArrowLeft" ? Keys.StepBack : e.key === "ArrowRight" ? Keys.StepForward : 0;
    if (!code) return;
    e.preventDefault();
    e.stopPropagation();
    fs?.send(RecordKind.Key, { code, t: e.timeStamp });
  };

  const s2a = async () => {
    const v = cutView();
    if (!v) return undefined;
    const r = await attempt(() => runS2a(v, setS2aLog));
    if (r) await attempt(() => actions.s2aReport(r));
    return r;
  };
  (globalThis as unknown as { __gmclipS2a?: () => Promise<unknown> }).__gmclipS2a = s2a;

  const vs = () => viewer();
  /** The off-canvas ghost (SPEC §5.7): the dragged layer, else the selected one, when it
   * reaches past the frame's edge — its whole box drawn over the stage around the frame,
   * so what the export crops away stays visible while placing it. */
  const ghost = () => {
    const v = cutView();
    const r = rect();
    if (!v || r.w <= 0) return null;
    let b: [number, number, number, number] | null = vs()?.drag ?? null;
    if (!b) {
      const sel = vs()?.selected;
      const l = sel ? v.parts.find((p) => p.plan_id === sel[0])?.layers.find((x) => x.id === sel[1]) : undefined;
      if (l) b = l.place;
    }
    if (!b) return null;
    const [x, y, w, h] = b;
    const { w: cw, h: ch } = v.canvas;
    if (x >= -0.5 && y >= -0.5 && x + w <= cw + 0.5 && y + h <= ch + 0.5) return null;
    const k = r.w / cw;
    return { left: r.x + x * k, top: r.y + y * k, width: w * k, height: h * k };
  };
  const portrait = () => {
    const v = cutView();
    const a = props.aspect?.() ?? (v ? v.canvas.w / v.canvas.h : 9 / 16);
    return a < 1;
  };
  const fpsOf = () => vs()?.fps ?? 30;
  return (
    <section class="viewer" aria-label="Viewer">
      <div class="viewer-box" ref={box} data-guides={vs()?.guides?.length ?? 0} data-drag={vs()?.drag?.map((v) => Math.round(v)).join(",") ?? ""} onDragOver={imageDropHandlers.onDragOver} onDrop={imageDropHandlers.onDrop}>
        <canvas
          ref={canvas}
          class="viewer-canvas"
          tabindex="0"
          aria-label="Preview frame. Drag a layer to move it; drag a corner to resize; space plays."
          onPointerDown={onDown}
          onPointerMove={onMove}
          onPointerUp={onUp}
          onPointerCancel={onCancel}
          onWheel={onWheel}
          onKeyDown={onKey}
          onDblClick={() => props.onDblClick?.()}
        />
        {/* a reactive child, not a non-keyed <Show>: the overlay switches between comment
            pins, the crop boxes and the brush (each always set), and follows the rect */}
        {props.overlay?.(rect())}
        <Show when={ghost()}>
          {(g) => <div class="offcanvas-ghost" data-testid="offcanvas-ghost" style={{ left: `${g().left}px`, top: `${g().top}px`, width: `${g().width}px`, height: `${g().height}px` }} />}
        </Show>
        <Show when={prefs.safe_zones && rect().w > 0}>
          <div class="safe-zones" data-testid="safe-zones" style={{ left: `${rect().x}px`, top: `${rect().y}px`, width: `${rect().w}px`, height: `${rect().h}px` }}>
            <Show
              when={portrait()}
              fallback={
                <>
                  <div class="safe-frame sf-action"><span class="safe-label">Action safe</span></div>
                  <div class="safe-frame sf-title" />
                </>
              }
            >
              {/* Instagram's overlays on a 1080 × 1920 Reel (AGENTS.md §9): top 220 px,
                  bottom 360 px, right 120 px between y 1000 and 1560. */}
              <div class="safe-zone sz-top"><span class="safe-label">App UI</span></div>
              <div class="safe-zone sz-bottom"><span class="safe-label">Caption + buttons</span></div>
              <div class="safe-zone sz-right" />
            </Show>
          </div>
        </Show>
        <span class="viewer-stats" ref={statsEl} data-testid="viewer-stats" hidden={!(prefs.developer && prefs.render_stats)}>{stats()}</span>
        <Show when={status() !== "live"}>
          <div class="viewer-empty">
            <Icon name={status() === "refused" ? "circle-alert" : "loader"} size={14} class={status() === "refused" ? "" : "spin"} />
            {status() === "refused"
              ? "The preview socket refused this page (one viewer per launch)."
              : status() === "closed"
                ? "The preview paused while the window was hidden; it comes back by itself…"
                : "Connecting to the preview…"}
          </div>
        </Show>
        <Show when={vs()?.error}>
          <div class="viewer-error" role="alert"><Icon name="circle-alert" size={14} />{vs()?.error}</div>
        </Show>
      </div>
      <div class="transport" role="toolbar" aria-label="Transport">
        <div class="tp-left">
          <span class="tc" data-testid="timecode" data-frame={vs()?.frame ?? 0} data-scrub-path={vs()?.scrub_path ?? ""}>
            <span class="tc-now">{timecode(vs()?.frame ?? 0, fpsOf())}</span>
            <span class="tc-sep"> / </span>
            <span class="tc-total">{timecode(vs()?.frames ?? 0, fpsOf())}</span>
          </span>
          <Show when={(vs()?.rate ?? 1) !== 1}>
            <Chip tone="accent" class="rate" testid="rate" tooltip={(vs()?.rate ?? 1) < 0 ? "Playing backwards (J): silent; K or Space stops" : undefined}>
              {(vs()?.rate ?? 1) < 0 ? "◀ " : ""}×{Math.abs(vs()?.rate ?? 1).toFixed(2).replace(/\.?0+$/, "")}{vs()?.audio?.muted ? " muted" : ""}
            </Chip>
          </Show>
          <Show when={vs()?.audio?.no_sound}>
            {(why) => <Chip tone="warning" icon="triangle-alert" testid="no-sound" role="status" tooltip={`This cut's sound would not build: ${why()}`}>no sound</Chip>}
          </Show>
          <Show when={vs()?.scrub_path === "proxy" || vs()?.scrub_path === "keyframe"}>
            <Chip class="scrub-chip" testid="scrub-source" tooltip="Scrubbing shows a light copy of the video; the exact frame follows when you stop">
              {vs()?.scrub_path === "proxy" ? "scrub preview" : "keyframes"}
            </Chip>
          </Show>
          <Show when={vs()?.draft_matte}>
            <Chip tone="warning" class="draft-matte" testid="viewer-draft-matte" tooltip="A quick draft cut-out: the export builds the accurate one first (Look → Build accurate mattes)">draft matte</Chip>
          </Show>
        </div>
        <div class="tp-center">
          <IconButton icon="skip-back" label="Previous part" keys="↑" onClick={() => jumpPart(-1)} />
          <IconButton icon="chevron-left" label="Back one frame" keys="←" onClick={() => void attempt(() => actions.viewerStep(-1))} />
          <button type="button" class="tp-play" aria-label={vs()?.playing ? "Pause" : "Play"} data-tip={vs()?.playing ? "Pause" : "Play"} data-kbd="Space" onClick={() => void attempt(actions.viewerToggle)}>
            <Icon name={vs()?.playing ? "pause" : "play"} size={16} />
          </button>
          <IconButton icon="chevron-right" label="Forward one frame" keys="→" onClick={() => void attempt(() => actions.viewerStep(1))} />
          <IconButton icon="skip-forward" label="Next part" keys="↓" onClick={() => jumpPart(1)} />
          <IconButton icon="repeat" label="Loop the range or the part" keys="⌥L" onClick={() => setLoopAsk((n) => n + 1)} />
        </div>
        <div class="tp-right">
          <Toggle icon="audio-lines" label="Scrub audio" data-testid="scrub-audio" on={vs()?.scrub ?? true} onChange={(on) => void attempt(() => actions.viewerScrub(on))} />
          <Toggle icon="hd" label="Full quality" keys="E" on={vs()?.full ?? false} onChange={(on) => void attempt(() => actions.viewerFull(on))} />
          {/* the preview's measuring tools: the developer setting's (decision 0054) */}
          <Show when={props.measure !== false && prefs.developer}>
            <Menu
              label="More viewer options"
              icon="more"
              items={() => [
                { label: "Show render stats", checked: prefs.render_stats, onSelect: () => setPref("render_stats", !prefs.render_stats) },
                { label: "Measure the preview (S2a)", icon: "ruler", onSelect: () => void s2a() },
              ]}
            />
          </Show>
        </div>
      </div>
      <Show when={s2aLog()}>
        <div class="s2a-log">{s2aLog()}</div>
      </Show>
    </section>
  );
}
