// "Follow cursor" on one part (SPEC §5.7; AJ: "only needed some times, but a nice
// feature to have"): opt-in. On, the app finds the mouse cursor on the part's screen
// recording (2 frames a second) and zooms the screen crop in on it with eased moves;
// off takes the moves away. Each is one undo step; the preview draws what the export
// draws. The same follow is an agent tool (`crop_follow_cursor`). A screen gmclip
// recorded follows its logged cursor instead (decision 0045: exact, no frames decoded),
// and `RecordedCursor` sets how that cursor is drawn.
import { createMemo, createResource, createSignal, For, Show } from "solid-js";
import { actions, recordingActions, type PartView } from "../actions";
import type { CursorClick, SourceCursor } from "../gen/commands";
import { Switch } from "../design/components";
import { attempt, cutView, editing, say } from "../store";

const ZOOMS = [1.5, 2, 3];

export function CursorFollow(props: { part: PartView }) {
  const [busy, setBusy] = createSignal(false);
  const [zoom, setZoom] = createSignal(2);
  // how the last follow found the cursor: the recording's input log or the pixels
  const [method, setMethod] = createSignal<"input_log" | "pixels" | null>(null);
  const how = () => (method() === "input_log" ? " · from the input log" : method() === "pixels" ? " · found in the picture" : "");
  let box: HTMLDivElement | undefined;
  const on = () => (props.part.follow_moves ?? 0) > 0;
  const run = async (next: boolean) => {
    const v = cutView();
    if (!v || busy()) return;
    setBusy(true);
    const r = await attempt(() => actions.followCursor(v.project, v.cut, props.part.id, next, zoom()));
    setBusy(false);
    // the switch shows the part's state (a follow that found nothing stays off)
    queueMicrotask(() => {
      const el = box?.querySelector<HTMLInputElement>("input[role=switch]");
      if (el) el.checked = on();
    });
    if (!r) return;
    setMethod(r.method ?? null);
    const log = r.method === "input_log";
    if (!next) say("Cursor follow off · ⌘Z undoes");
    else if (r.moves === 0) say(r.found === 0 ? `No cursor found on part ${props.part.n} (${log ? `${r.samples} logged samples` : `read ${r.samples} frames`})` : "The cursor stayed put: nothing to follow");
    else say(`Following the cursor: ${r.moves} move${r.moves === 1 ? "" : "s"} (${log ? "from the recording's input log" : `cursor seen in ${r.found} of ${r.samples} frames`}) · ⌘Z undoes`);
  };
  return (
    <Show when={props.part.layers.some((l) => l.role === "screen")}>
      <div ref={box} class="cursor-follow" role="group" aria-label="Follow cursor" data-testid="cursor-follow" aria-busy={busy()}>
        <Switch
          label="Follow cursor"
          testid="follow-cursor"
          hint="Zoom the screen in on the mouse cursor, moving smoothly when it moves (from the input log of a gmclip recording, else found in the picture 2 frames a second)"
          on={on()}
          disabled={busy()}
          onChange={(x) => void run(x)}
        />
        <select aria-label="Cursor zoom" data-testid="follow-zoom" value={String(zoom())} disabled={busy()}
          onChange={(e) => { setZoom(Number(e.currentTarget.value)); if (on()) void run(true); }}>
          {ZOOMS.map((z) => <option value={String(z)}>{z}×</option>)}
        </select>
        <Show when={busy()}><span class="muted small" data-testid="follow-busy">Finding the cursor…</span></Show>
        <Show when={on() && !busy()}><span class="muted small" data-testid="follow-moves" data-method={method() ?? undefined}>{props.part.follow_moves} moves{how()}</span></Show>
      </div>
    </Show>
  );
}

/** The cursor's smoothness, 0–1 (an older smoothing window read as its nearest share). */
const smoothness = (c: SourceCursor) => c.smoothness ?? (c.smooth_ms ? Math.min(1, Math.round((c.smooth_ms / 250) * 20) / 20) : 0);
/** How a click shows (`clicks` read when `click` is unset). */
const clickOf = (c: SourceCursor): CursorClick => c.click ?? (c.clicks ? "ring" : "none");

const IDLE = [
  { ms: 0, label: "Never" },
  { ms: 1000, label: "After 1 s" },
  { ms: 2000, label: "After 2 s" },
  { ms: 5000, label: "After 5 s" },
];

const LOOPS = [
  { ms: 0, label: "Off" },
  { ms: 500, label: "Last 0.5 s" },
  { ms: 1000, label: "Last 1 s" },
  { ms: 2000, label: "Last 2 s" },
];

/** A recorded screen's cursor (decision 0045): the recording hid it and gmclip draws it
 * from the log, so it can be hidden, sized, smoothed, faded when idle and marked on each
 * click. Its feel (decision 0046): smoothness (a spring; replaces the smoothing window,
 * which is still read while smoothness is unset), a ring or ripple per click and a dip
 * in size, small slow shakes held still, a glide back to the start for loops, and motion
 * blur (off by default: it renders sub-frames). Each change is one `source_cursor_set`
 * (one undo step). Shown for a part whose picture is a source with a logged cursor
 * (matched by its picture size). */
export function RecordedCursor(props: { part: PartView }) {
  const [list, { refetch }] = createResource(
    () => { const v = cutView(); return v ? `${v.project}\n${v.rev}` : null; },
    async (k) => (await attempt(() => recordingActions.sourceList(k.split("\n")[0]!))) ?? null,
  );
  // the logged sources this part's layers show (by picture size), else none
  const mine = createMemo(() => {
    const l = list();
    if (!l) return [];
    const cursors = l.cursors ?? {};
    const sizes = props.part.layers.map((x) => `${Math.round(x.src_size[0])}x${Math.round(x.src_size[1])}`);
    return l.sources.filter((s) => cursors[s.id] && sizes.includes(`${s.width ?? 0}x${s.height ?? 0}`)).map((s) => ({ id: s.id, name: s.name, cursor: cursors[s.id]! }));
  });
  const [pick, setPick] = createSignal<string | null>(null);
  const cur = () => mine().find((m) => m.id === pick()) ?? mine()[0];
  const [busy, setBusy] = createSignal(false);
  const set = async (patch: Partial<Pick<SourceCursor, "visible" | "scale" | "smooth_ms" | "hide_idle_ms" | "clicks" | "click" | "smoothness" | "click_shrink" | "shake_px" | "loop_ms" | "motion_blur">>) => {
    const v = cutView();
    const c = cur();
    if (!v || !c || busy()) return;
    setBusy(true);
    const r = await editing(attempt(() => recordingActions.cursorSet(v.project, { source: c.id, ...patch })));
    setBusy(false);
    void refetch();
    if (r) say("Cursor changed · ⌘Z undoes");
  };
  return (
    <Show when={cur()}>
      {(c) => (
        <div class="cursor-follow recorded-cursor" role="group" aria-label="Recorded cursor" data-testid="recorded-cursor" aria-busy={busy()}>
          <Show when={mine().length > 1}>
            <select aria-label="Screen recording" data-testid="cursor-source" value={c().id} onChange={(e) => setPick(e.currentTarget.value)}>
              <For each={mine()}>{(m) => <option value={m.id}>{m.name}</option>}</For>
            </select>
          </Show>
          <Switch label="Show cursor" testid="cursor-visible" hint="Draw the cursor the recording logged (the recording itself has none)" on={c().cursor.visible} disabled={busy()} onChange={(x) => void set({ visible: x })} />
          <Show when={c().cursor.visible}>
            <label class="appearance-range small">
              <span>Size</span>
              <input type="range" min="0.5" max="3" step="0.25" data-testid="cursor-scale" value={c().cursor.scale} disabled={busy()} onChange={(e) => void set({ scale: Number(e.currentTarget.value) })} />
              <span class="tabular">{c().cursor.scale}×</span>
            </label>
            <label class="appearance-range small" data-tip="0 is the path as recorded; higher glides after it (a spring up to 250 ms behind)">
              <span>Smoothness</span>
              <input type="range" min="0" max="1" step="0.05" data-testid="cursor-smoothness" value={smoothness(c().cursor)} disabled={busy()}
                onChange={(e) => void set({ smoothness: Number(e.currentTarget.value), smooth_ms: 0 })} />
              <span class="tabular">{smoothness(c().cursor) ? `${Math.round(smoothness(c().cursor) * 100)}%` : "off"}</span>
            </label>
            <label class="appearance-range small">
              <span>Hide when idle</span>
              <select data-testid="cursor-idle" value={String(c().cursor.hide_idle_ms)} disabled={busy()} onChange={(e) => void set({ hide_idle_ms: Number(e.currentTarget.value) })}>
                <For each={IDLE.some((x) => x.ms === c().cursor.hide_idle_ms) ? IDLE : [...IDLE, { ms: c().cursor.hide_idle_ms, label: `After ${c().cursor.hide_idle_ms / 1000} s` }]}>
                  {(x) => <option value={String(x.ms)}>{x.label}</option>}
                </For>
              </select>
            </label>
            <label class="appearance-range small">
              <span>Clicks</span>
              <select data-testid="cursor-click" value={clickOf(c().cursor)} disabled={busy()} onChange={(e) => void set({ click: e.currentTarget.value as CursorClick })}>
                <option value="none">No effect</option>
                <option value="ring">Ring</option>
                <option value="ripple">Ripple</option>
              </select>
            </label>
            <Switch label="Shrink on click" testid="cursor-click-shrink" hint="The cursor dips a little on each click" on={!!c().cursor.click_shrink} disabled={busy()} onChange={(x) => void set({ click_shrink: x })} />
            <label class="appearance-range small" data-tip="Small slow moves under this many pixels are held still">
              <span>Remove shakes</span>
              <input type="range" min="0" max="20" step="1" data-testid="cursor-shake" value={c().cursor.shake_px ?? 0} disabled={busy()} onChange={(e) => void set({ shake_px: Number(e.currentTarget.value) })} />
              <span class="tabular">{c().cursor.shake_px ? `${c().cursor.shake_px} px` : "off"}</span>
            </label>
            <label class="appearance-range small" data-tip="The cursor glides back to where each part began over its last moments, so the clip loops">
              <span>Loop to start</span>
              <select data-testid="cursor-loop" value={String(c().cursor.loop_ms ?? 0)} disabled={busy()} onChange={(e) => void set({ loop_ms: Number(e.currentTarget.value) })}>
                <For each={LOOPS.some((x) => x.ms === (c().cursor.loop_ms ?? 0)) ? LOOPS : [...LOOPS, { ms: c().cursor.loop_ms ?? 0, label: `Last ${(c().cursor.loop_ms ?? 0) / 1000} s` }]}>
                  {(x) => <option value={String(x.ms)}>{x.label}</option>}
                </For>
              </select>
            </label>
            <Switch label={<>Motion blur <span class="muted small">slower export</span></>} testid="cursor-blur" hint="Blur the cursor while it moves (5 sub-frames each frame it moves: slower export)" on={!!c().cursor.motion_blur} disabled={busy()} onChange={(x) => void set({ motion_blur: x })} />
          </Show>
        </div>
      )}
    </Show>
  );
}
