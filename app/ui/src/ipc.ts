// The UI wire (SPEC §12.1): `gm_call` for every command, `gm_subscribe` for event
// scopes, `gm://data` for binary columns. In the app these are Tauri IPC; under
// `--dev-bridge` the same calls ride a token-protected loopback WebSocket so the UI runs
// in a browser (Playwright). Nothing else in the UI talks to the core.
import { Channel, invoke } from "@tauri-apps/api/core";
import { gmFetchUrl } from "./platform";
import type { Scopes } from "./gen/events";

export interface AppError {
  code: string;
  message: string;
  detail?: unknown;
}

export class CallError extends Error {
  constructor(public readonly error: AppError) {
    super(error.message);
  }
}

type Reply = { ok: true; output: unknown; as_of?: number } | { ok: false; error: AppError };

/** One delivery of a scope's events; `E` is the scope's generated event type (A2.01). */
export interface EventBatch<E = Record<string, unknown>> {
  sub: number;
  scope: string;
  events: E[];
}

/** A `gm_subscribe` scope. */
export type Scope = Exclude<keyof Scopes, "project"> | `project:${string}`;
/** The events a scope carries, from the app's Rust event types (`gen/events.ts`). */
export type EventOf<S extends Scope> = S extends `project:${string}` ? Scopes["project"] : S extends keyof Scopes ? Scopes[S] : never;

interface Transport {
  kind: "tauri" | "bridge";
  call(req: Record<string, unknown>): Promise<Reply>;
  subscribe(scope: string, cb: (b: EventBatch) => void): Promise<number>;
  /** A `gm://data` body, or null when there is none. */
  data(url: string): Promise<ArrayBuffer | null>;
}

interface BridgeInfo {
  port: number;
  token: string;
}

function bridgeInfo(): BridgeInfo | null {
  const w = globalThis as unknown as { __GMCLIP_DEV_BRIDGE__?: BridgeInfo };
  return w.__GMCLIP_DEV_BRIDGE__ ?? null;
}

export function inTauri(): boolean {
  return "__TAURI_INTERNALS__" in globalThis;
}

const tauri: Transport = {
  kind: "tauri",
  call: (req) => invoke<Reply>("gm_call", { req }),
  subscribe: async (scope, cb) => {
    const ch = new Channel<EventBatch>();
    ch.onmessage = cb;
    return invoke<number>("gm_subscribe", { scope, onEvent: ch });
  },
  data: async (url) => {
    try {
      const r = await fetch(gmFetchUrl(url));
      return r.ok ? await r.arrayBuffer() : null;
    } catch {
      return null;
    }
  },
};

function bridge(info: BridgeInfo): Transport {
  let next = 1;
  const pending = new Map<number, (r: Reply) => void>();
  const pendingData = new Map<number, (b: ArrayBuffer | null) => void>();
  const subs = new Map<number, (b: EventBatch) => void>();
  const ready = new Promise<WebSocket>((resolve, reject) => {
    const ws = new WebSocket(`ws://127.0.0.1:${info.port}/`);
    // `data` answers arrive as binary: the request id (u32 LE), then the body
    ws.binaryType = "arraybuffer";
    ws.onopen = () => {
      ws.send(info.token);
      resolve(ws);
    };
    ws.onerror = () => reject(new Error("dev bridge unreachable"));
    // A socket that never opens fails its calls instead of holding them forever.
    setTimeout(() => {
      if (ws.readyState === WebSocket.CONNECTING) {
        ws.close();
        reject(new Error("dev bridge did not answer"));
      }
    }, 10_000);
    ws.onclose = () => {
      // Nothing more will arrive: fail every pending call instead of hanging.
      for (const [, done] of pending) done({ ok: false, error: { code: "Internal", message: "the connection to gmclip closed" } });
      pending.clear();
      for (const [, done] of pendingData) done(null);
      pendingData.clear();
    };
    ws.onmessage = (m) => {
      if (m.data instanceof ArrayBuffer) {
        if (m.data.byteLength < 4) return;
        const id = new DataView(m.data).getUint32(0, true);
        pendingData.get(id)?.(m.data.byteLength > 4 ? m.data.slice(4) : null);
        pendingData.delete(id);
        return;
      }
      const msg = JSON.parse(String(m.data)) as { id?: number; reply?: Reply; event?: EventBatch };
      if (msg.event) subs.get(msg.event.sub)?.(msg.event);
      else if (typeof msg.id === "number" && msg.reply) {
        pending.get(msg.id)?.(msg.reply);
        pending.delete(msg.id);
      }
    };
  });
  const send = async (body: Record<string, unknown>): Promise<Reply> => {
    const ws = await ready;
    // A closed bridge (a newer page took it over, or gmclip quit) answers at once.
    if (ws.readyState !== WebSocket.OPEN) return { ok: false, error: { code: "Internal", message: "the connection to gmclip closed" } };
    const id = next++;
    return new Promise<Reply>((resolve) => {
      pending.set(id, resolve);
      ws.send(JSON.stringify({ id, ...body }));
    });
  };
  // Dev bridge only (R-DEV-1; never in the app): lets a test replay what a native menu
  // item sends, since a page under Playwright has no native menu.
  (globalThis as unknown as { __gmclipBridgeCall?: unknown }).__gmclipBridgeCall = (command: string, args: Record<string, unknown>, project?: string) =>
    send({ call: project ? { command, args, project } : { command, args } });
  return {
    kind: "bridge",
    data: async (url) => {
      const ws = await ready.catch(() => null);
      if (!ws || ws.readyState !== WebSocket.OPEN) return null;
      const id = next++;
      return new Promise<ArrayBuffer | null>((resolve) => {
        pendingData.set(id, resolve);
        ws.send(JSON.stringify({ id, data: url }));
      });
    },
    call: (req) => send({ call: req }),
    subscribe: async (scope, cb) => {
      const r = await send({ subscribe: scope });
      if (!r.ok) throw new CallError(r.error);
      const sid = r.output as number;
      subs.set(sid, cb);
      return sid;
    },
  };
}

let transport: Transport | null = null;
function t(): Transport {
  if (transport) return transport;
  const b = bridgeInfo();
  if (inTauri()) transport = tauri;
  else if (b) transport = bridge(b);
  else throw new Error("no gmclip core: open this page in the app or through --dev-bridge");
  return transport;
}

export function transportKind(): "tauri" | "bridge" | "none" {
  try {
    return t().kind;
  } catch {
    return "none";
  }
}

type MutationPhase = "start" | "done" | "error";
const mutationListeners = new Set<(p: MutationPhase) => void>();
/** Hears every edit's start and end (the "Saved" indicator). */
export function onMutation(f: (p: MutationPhase) => void) {
  mutationListeners.add(f);
}

/** Commands that change a project (everything else only reads or drives the viewer). */
const MUTATING = new Set(["cut_import", "history_restore", "cut_trim", "cut_split", "part_delete", "layer_place_px", "layer_crop_px", "layer_set", "undo", "redo", "cut_set", "part_time_set", "part_nudge_ms", "part_move", "cut_chronological", "words_strike_ord", "words_restore_ord", "word_nudge_ord", "caption_edit", "caption_keyword_toggle", "moment_set_state", "moment_excerpt_set", "video_open", "video_pick", "cut_output_set", "layout_preset", "layout_apply_all", "layer_crop_norm", "inset_add", "hook_layout_keep", "inset_delete", "layout_copy_next", "layout_preset_apply", "captions_set", "caption_fix", "caption_replace", "cue_break", "caption_keep", "caption_y", "term_add", "hold_at", "caption_keywords", "hook_set", "pill_add", "run_measure", "clock_add", "short_overlay_delete", "sweep_apply_app", "hold_toggle", "part_reorder_to", "project_restore_point", "agent_run", "batch_start", "owner_request", "agent_client_set", "models_download", "export_verdict", "clippy_pair", "clippy_send", "agents_pause", "approval_resolve", "agent_run_stop", "agent_client_allow", "word_respell", "word_edge_reset", "words_add_part", "words_stretch_last", "picture_offset_set", "gain_span_add", "gain_span_delete", "part_gain_set", "cam_hide_add", "cam_hide_delete", "loudness_set", "cut_from_moment", "no_captions_add", "no_captions_delete", "run_measure_src", "story_apply", "story_suggestion_state", "story_brief_set", "overlay_time_set", "overlay_split_at", "overlay_split", "overlay_split_parts", "gain_segment_edit", "stream_edit_ensure", "topic_label_ord", "topic_edge_ord", "topic_split_ord", "topic_state", "topic_delete", "fluff_state", "longform_from_topics", "cut_pull_from_parent", "framing_add", "framing_pick", "comment_add_at", "comment_resolve", "comment_delete", "comments_fix", "edit_proposal_state", "review_ask", "golden_mark", "thumb_add_at", "thumb_cover", "thumbnail_set", "thumb_agent"]);

// the look (studio): mutating app-shell commands
for (const c of ["caption_style_apply", "caption_style_save_mine", "image_overlay_add", "callout_add", "brand_kit_save", "brand_kit_use_mine", "brand_logo_set", "lower_third_add_at", "end_card_add_app", "overlay_template_save_app", "overlay_template_insert_at", "framing_set", "caption_preset_apply", "part_set_speed", "part_set_ramp", "backdrop_set", "video_overlay_add", "plate_save", "overlay_track", "overlay_track_set", "cover_track", "cover_add", "layer_track_set"]) MUTATING.add(c);

// decision 0024: openings, hook lines, drafts
for (const c of ["opening_add", "opening_add_words", "opening_set", "opening_delete", "opening_choose", "copy_draft_state"]) MUTATING.add(c);

// keyboard editing (lane K5): the I/O range ripple-deleted
MUTATING.add("cut_range_delete");
// 9:16 and 16:9 together (lane K5): a missing framing made from the layout defaults
for (const c of ["framing_make", "export_both"]) MUTATING.add(c);
// end card to the long-form (lane K5)
for (const c of ["end_card_longform_add_app", "end_card_longform_remove"]) MUTATING.add(c);

// viewer gizmos (U.12): a text overlay dragged, the caption seat moved
for (const c of ["overlay_drag", "caption_seat"]) MUTATING.add(c);

// ideas 2026-10-16 (lane K4): layouts drawn, names fixed, long-forms forked/picked, B-roll, reactions
for (const c of ["layout_draw_save", "term_fix_everywhere", "longform_fork", "longform_pick", "broll_add", "reactions_find"]) MUTATING.add(c);

// decision 0045: how a recorded screen's cursor is drawn
MUTATING.add("source_cursor_set");
// decision 0046: sections, a layer's frame and padding, the camera bubble, the one-click look
for (const c of ["section_add", "section_set", "section_remove", "zoom_auto_app", "layer_frame_set", "place_set", "layout_builtin_add", "screen_studio_apply", "restore_to"]) MUTATING.add(c);

// ideas 2026-10-16 (lane K2): polish suggestions applied (the view may star stressed words when the style asks)
for (const c of ["polish_apply", "polish_view"]) MUTATING.add(c);

/** Runs one command. Throws `CallError` with the core's error. */
let viewerChain: Promise<void> = Promise.resolve();

/** Commands that move the playhead. */
const MOVES = new Set(["viewer_seek", "viewer_step", "viewer_play", "viewer_toggle", "viewer_play_range", "viewer_play_src", "viewer_rate", "viewer_load"]);
/** The frame the last exact seek asked for, until the viewer reports it (null after any
 * other move): a seek's reply comes before the viewer's state shows the frame. */
let seekTarget: number | null = null;
/** Where the playhead is going: the last seek's frame while the viewer has not shown it. */
export const pendingSeek = (): number | null => seekTarget;
/** The viewer showed `frame`: a seek to it has landed. */
export function seekReported(frame: number) {
  if (seekTarget === frame) seekTarget = null;
}

/** Edits on their way to the core (everything but undo and redo themselves, and the
 * commands that wait on a person or a long job rather than edit: a file panel, a model
 * download, an agent or batch run). */
const edits = new Set<Promise<unknown>>();
const NOT_WAITED = new Set(["undo", "redo", "video_pick", "video_open", "models_download", "agent_run", "batch_start", "clippy_pair", "clippy_send", "owner_request"]);
/** Resolves once every edit sent so far has been answered (an undo sent before an edit
 * still in flight lands first and undoes the edit before it), or after `maxMs`. */
export async function editsAnswered(maxMs = 10_000): Promise<void> {
  const end = Date.now() + maxMs;
  while (edits.size && Date.now() < end) {
    await Promise.race([Promise.allSettled([...edits]), new Promise((d) => setTimeout(d, Math.max(0, end - Date.now())))]);
  }
}

export function call<T = unknown>(command: string, args: Record<string, unknown> = {}, project?: string): Promise<T> {
  const p = send<T>(command, args, project);
  if (MUTATING.has(command) && !NOT_WAITED.has(command)) {
    edits.add(p);
    void p.then(
      () => edits.delete(p),
      () => edits.delete(p),
    );
  }
  return p;
}

async function send<T>(command: string, args: Record<string, unknown>, project?: string): Promise<T> {
  const req: Record<string, unknown> = { command, args };
  if (project) req.project = project;
  if (MOVES.has(command)) {
    // (the core floors the frame and clamps it at 0)
    seekTarget = command === "viewer_seek" && args.scrub !== true ? Math.max(0, Math.floor(Number(args.frame) || 0)) : null;
  }
  const mutating = MUTATING.has(command);
  if (mutating) mutationListeners.forEach((f) => f("start"));
  let r: Reply;
  try {
    // Playhead commands go out one after another: two in flight could overtake each other
    // (Home then a frame step landing in the wrong order).
    if (command.startsWith("viewer_")) {
      const prev = viewerChain;
      let release!: () => void;
      viewerChain = new Promise<void>((ok) => (release = ok));
      await prev;
      try {
        r = await t().call(req);
      } finally {
        release();
      }
    } else {
      r = await t().call(req);
    }
  } catch (e) {
    if (mutating) mutationListeners.forEach((f) => f("error"));
    throw e;
  }
  if (mutating) mutationListeners.forEach((f) => f(r.ok ? "done" : "error"));
  if (!r.ok) throw new CallError(r.error);
  return r.output as T;
}

/** Subscribes to an event scope; batches arrive at most every 16 ms, each event typed as
 * the scope's generated event type. */
export function subscribe<S extends Scope>(scope: S, cb: (b: EventBatch<EventOf<S>>) => void): Promise<number> {
  return t().subscribe(scope, cb as (b: EventBatch) => void);
}

/** A `gm://data` body: fetched in the app, over the socket under the dev bridge (the
 * same scheme, parser and kinds); null when there is none or no core to ask. */
export async function gmData(url: string): Promise<ArrayBuffer | null> {
  let tr: Transport;
  try {
    tr = t();
  } catch {
    return null;
  }
  return tr.data(url);
}

/** A binary column from `gm://data/<project>/<kind>`. */
export async function dataColumn(project: string, kind: string, cut: string): Promise<{ kind: number; count: number; rev: bigint; body: DataView } | null> {
  const buf = await gmData(`gm://data/${project}/${kind}?cut=${cut}`);
  if (!buf || buf.byteLength < 16) return null;
  const v = new DataView(buf);
  return { kind: v.getUint16(2, true), count: v.getUint32(4, true), rev: v.getBigUint64(8, true), body: new DataView(buf, 16) };
}

/** The timeline's audio and word columns from `gm://data` (D.02), on the cut's
 * output clock: per frame min/max (`peaks`) and dB (`energy`), quiet regions as frame
 * pairs, and the words the cut plays (ordinal, source ms, output frame, part, text). */
export interface CutColumns {
  rev: bigint;
  peaks: Float32Array | null;
  energy: Float32Array | null;
  quiet: Uint32Array | null;
  words: { ordinal: number; startMs: number; endMs: number; frame: number; part: number; text: string }[] | null;
}

export type ColumnKind = "peaks" | "energy" | "quiet" | "words";

/** The columns of `kinds` (all four by default); a kind not asked for is null. */
export async function cutColumns(project: string, cut: string, kinds: readonly ColumnKind[] = ["peaks", "energy", "quiet", "words"]): Promise<CutColumns | null> {
  const [pk, en, qu, wo] = await Promise.all((["peaks", "energy", "quiet", "words"] as const).map((k) => (kinds.includes(k) ? dataColumn(project, k, cut) : Promise.resolve(null))));
  if (!pk && !en && !qu && !wo) return null;
  // the columns are little-endian, as every platform gmclip runs on: an aligned body is
  // one copy, an unaligned one is read value by value
  const f32 = (c: typeof pk, per: number) => {
    if (!c) return null;
    const n = Math.min(c.count * per, Math.floor(c.body.byteLength / 4));
    if (c.body.byteOffset % 4 === 0) return new Float32Array(c.body.buffer, c.body.byteOffset, n).slice();
    const a = new Float32Array(n);
    for (let i = 0; i < a.length; i++) a[i] = c.body.getFloat32(i * 4, true);
    return a;
  };
  let quiet: Uint32Array | null = null;
  if (qu) {
    quiet = new Uint32Array(qu.count * 2);
    for (let i = 0; i < quiet.length; i++) quiet[i] = qu.body.getUint32(i * 4, true);
  }
  let words: CutColumns["words"] = null;
  if (wo) {
    const n = wo.count;
    const text = new Uint8Array(wo.body.buffer, wo.body.byteOffset + n * 28, wo.body.byteLength - n * 28);
    const dec = new TextDecoder();
    words = [];
    for (let i = 0; i < n; i++) {
      const u = (k: number) => wo.body.getUint32(i * 28 + k * 4, true);
      words.push({ ordinal: u(0), startMs: u(1), endMs: u(2), frame: u(3), part: u(4), text: dec.decode(text.subarray(u(5), u(5) + u(6))) });
    }
  }
  const rev = (pk ?? en ?? qu ?? wo)?.rev ?? 0n;
  return { rev, peaks: f32(pk, 2), energy: f32(en, 1), quiet, words };
}

/** One filmstrip thumbnail: an output frame and its picture (null until the scrub proxy
 * covers it). */
export interface Thumb {
  frame: number;
  image: ImageBitmap | null;
}

/** Timeline thumbnails from `gm://data/<project>/filmstrip` (H.05; app only): every
 * `every` output frames, `w` pixels wide, read from the scrub proxy. */
export async function filmstrip(project: string, cut: string, every: number, w: number): Promise<{ rev: bigint; thumbs: Thumb[] } | null> {
  if (!inTauri()) return null;
  let buf: ArrayBuffer;
  try {
    const r = await fetch(gmFetchUrl(`gm://data/${project}/filmstrip?cut=${cut}&every=${Math.max(1, Math.round(every))}&w=${Math.round(w)}`));
    if (!r.ok) return null;
    buf = await r.arrayBuffer();
  } catch {
    return null;
  }
  if (buf.byteLength < 16) return null;
  const v = new DataView(buf);
  const count = v.getUint32(4, true);
  const rev = v.getBigUint64(8, true);
  const bytes = new Uint8Array(buf);
  const thumbs: Thumb[] = [];
  let at = 16;
  for (let i = 0; i < count && at + 8 <= buf.byteLength; i++) {
    const frame = v.getUint32(at, true);
    const tw = v.getUint16(at + 4, true);
    const th = v.getUint16(at + 6, true);
    at += 8;
    const n = tw * th * 3;
    if (!tw || !th || at + n > buf.byteLength) {
      thumbs.push({ frame, image: null });
      continue;
    }
    const rgba = new Uint8ClampedArray(tw * th * 4);
    for (let k = 0, j = at; k < tw * th; k++, j += 3) {
      rgba[k * 4] = bytes[j]!;
      rgba[k * 4 + 1] = bytes[j + 1]!;
      rgba[k * 4 + 2] = bytes[j + 2]!;
      rgba[k * 4 + 3] = 255;
    }
    at += n;
    thumbs.push({ frame, image: await createImageBitmap(new ImageData(rgba, tw, th)) });
  }
  return { rev, thumbs };
}
// lane L's mutating app commands (kept apart from the list above so lanes do not collide)
for (const c of ["library_moment", "chat_import", "chat_nudge", "chat_moment", "tap_mark", "cut_like", "cut_template_save", "cut_template_delete", "project_consolidate", "project_open_copy"]) MUTATING.add(c);
