// Every UI action, declared once (R-CMD-2). Each names exactly one command; components
// call these and nothing else, so a static check can map handler → command.
import { call, cutColumns, filmstrip, gmData, type ColumnKind } from "./ipc";
import type { CaptionGrouping, Commands, Struck } from "./gen/commands";
import type { JobView as GenJobView, ViewerState as GenViewerState } from "./gen/events";
export type { ColumnKind, CutColumns } from "./ipc";
export type { CaptionGrouping } from "./gen/commands";
/** Where the playhead is going (the last seek's frame until the viewer shows it). */
export { pendingSeek } from "./ipc";

/** A registry command's arguments and output, generated from its JSON Schema (A2.01,
 *  `cargo xt gen`): calls that name a core command check their arguments against it. */
type Args<K extends keyof Commands> = Commands[K]["args"];
type Out<K extends keyof Commands> = Commands[K]["output"];

/** One version in the history (A2.13). */
export interface HistoryVersion {
  id: string;
  kind: "restore_point" | "named" | "run";
  name: string | null;
  rev: number;
  ts: number;
  run: string | null;
  parts: number | null;
  seconds: number | null;
  /** Who edited after it: "you", "agent:<client>", "review link <id>". */
  then_by: string[];
  run_by: string | null;
  run_label: string | null;
}

export interface HistoryDiffPart {
  part: string;
  index: number;
  source: string | null;
  src_in_s: number;
  src_out_s: number;
  out_start_s: number;
  dur_s: number;
  thumb?: { w: number; h: number; rgb: string };
}

export interface HistoryDiff {
  from_parts: number | null;
  from_seconds: number | null;
  to_parts: number | null;
  to_seconds: number | null;
  from: HistoryDiffPart[];
  to: HistoryDiffPart[];
  added: HistoryDiffPart[];
  removed: HistoryDiffPart[];
  trimmed: { from: HistoryDiffPart; to: HistoryDiffPart }[];
  moved: string[];
  by: string[];
}

/** What undo/redo answered: the transaction it undid (the generated `HistoryResult`), or,
 *  for an earlier session's edit, `needs_confirm` and nothing changed (R-HIST-1). */
export type UndoResult = Out<"undo"> | { needs_confirm: true; target: string; command: string; ts: number };

export interface ProjectInfo {
  id: string;
  name: string;
  home: string;
  rev: number;
  cuts: { id: string; title: string; imported: boolean; parts: number; framed: boolean }[];
}

export interface LayerView {
  id: string;
  model: string | null;
  role: string;
  crop: [number, number, number, number];
  place: [number, number, number, number];
  z: number;
  visible: boolean;
  opacity: number;
  /** The crop as a fraction of the source picture (model-planned cuts). */
  crop_n: [number, number, number, number] | null;
  src_size: [number, number];
  /** The video its crop is of (a recording's camera layer: the camera file). */
  source?: string;
  /** A part's own addition (a detail inset), removable. */
  addition?: boolean;
  /** Rounded corners, shadow and border (decision 0046), as planned for this part. */
  frame?: PlannedFrame | null;
  /** A roto cut-out (R.01): soften and brush strokes in source pixels. */
  matte?: MatteView | null;
  /** Where crop and place come from (U.09): a split, the part, the linked layout, the
   * framing's defaults, or a layer the part added. */
  from?: LayerFrom | null;
}

/** A layer's frame as the RenderPlan carries it (decision 0046; colours `#RRGGBB`). */
export interface PlannedFrame {
  radius: number;
  shadow?: { blur: number; offset_y: number; opacity: number; rgb: string } | null;
  border?: { width: number; rgb: string } | null;
}

export type ValueSource = "split" | "part" | "layout" | "framing" | "own";
export interface LayerFrom {
  crop: ValueSource;
  place: ValueSource;
  owner: "layout" | "framing" | "own";
  layout: string | null;
  split_s: number | null;
}

export interface MatteView {
  kind: "roto";
  model: string;
  soften?: number;
  strokes?: { mode: "add" | "subtract"; radius: number; feather?: number; points: { x: number; y: number }[] }[];
}

/** One matted part's cut-out quality (R.02), worst first. */
export interface MatteRow {
  plan_part: string;
  layer: string;
  part: string | null;
  model_layer: string | null;
  status: "accurate" | "missing";
  quality: { frames: number; coverage: number; uncertain: number; flicker: number; empty_frames: number; score: number } | null;
  out: [number, number] | null;
}

export interface MatteBuild {
  state: "running" | "done" | "failed";
  done: number;
  total: number;
  error: string | null;
  /** Started on its own (a background build), not by the button. */
  auto?: boolean;
}

export interface MatteProgress {
  rev: number;
  parts: { plan_part: string; out: [number, number]; done: number; total: number }[];
  build: MatteBuild | null;
}

/** A brush stroke as `matte_brush` takes it (normalised source coordinates). */
export interface StrokeArg {
  mode: "add" | "subtract";
  radius: number;
  feather?: number;
  points: [number, number][];
}

export interface LayoutPresetRow {
  name: string;
  layers: number;
  insets: number;
  /** Saved by you (else read from the profile's stream layouts). */
  saved: boolean;
}

export interface CaptionLook {
  animation: "none" | "bounce" | "pop" | "fade";
  keyword_animation: "none" | "pop";
  keyword_colour?: string;
}

/** One caption style of the gallery: a built-in, the project's, or one of mine (profile). */
export interface CaptionStyleRow {
  name: string;
  label: string;
  origin: "builtin" | "project" | "mine";
  font: string;
  current: boolean;
  grouping?: string;
  look?: CaptionLook;
  /** The current frame with this style's caption, RGB bytes (base64). */
  thumb: { w: number; h: number; rgb: string } | null;
  why?: string;
}

export interface BrandColours {
  primary: string;
  accent: string;
  text: string;
  background: string;
}

/** What `brand_kit_save` takes (only the fields given change). */
export interface BrandKitFields {
  name?: string;
  handle?: string;
  heading_font?: string;
  body_font?: string;
  colours?: BrandColours;
  caption_preset?: string;
}

export interface BrandKit {
  name: string;
  handle: string;
  heading_font: string | null;
  body_font: string | null;
  colours: BrandColours;
  logo: { asset: string; w: number; h: number } | null;
  caption_preset: string | null;
  templates: Record<string, { kind: string; role?: string; dur_ms: number; counter?: unknown }>;
}

export interface BrandView {
  kit: BrandKit | null;
  mine: Partial<BrandKit> | null;
  profile: unknown;
  faces: string[];
  presets: { name: string; label: string }[];
}

/** An overlay on the cut (the inspector's overlay list). */
/** One registered command, as "Developer: all commands" lists it. */
export interface CatalogCommand {
  name: string;
  version: number;
  doc: string;
  scope: string;
  exec: "sync" | "job";
  undo: "undoable" | "read-only" | "destructive";
  /** Where the app's menus offer it, if anywhere. */
  ui: { label: string; menu: string } | null;
  /** Only a person may run it (never an agent). */
  human_only: boolean;
  example: Record<string, unknown> | null;
}

/** What a viewer gesture holds in the core (R-GEST-1): a lane item, a crop's layer, the
 * caption seat's framing or one part. */
export type GestureKind = "overlay" | "gain" | "layer" | "framing" | "part";

/** A plan text event's role (`viewer_text_drag` moves the events of one). */
export type TextRole = "caption" | "text_hook" | "section_pill" | "run_clock" | "note";

export interface ViewerGizmos {
  frame: number;
  canvas: { w: number; h: number };
  /** Text overlays on the frame: their ink box (canvas px), place (fractions) and their
   *  events' anchor (canvas px, what a drag's frame preview moves). */
  texts: { overlay: string; role: TextRole; label: string; box: [number, number, number, number]; place: { x: number; y: number; w: number }; at: [number, number] }[];
  /** The caption seat (canvas px), the part on screen and the cue's box when one shows. */
  caption: { y: number; box: [number, number, number, number] | null; part: string | null; editable: boolean } | null;
  /** Added videos and images on the frame: their box (canvas px) and place (fractions). */
  pictures?: { overlay: string; kind: "video" | "image"; box: [number, number, number, number]; place: { x: number; y: number; w: number } }[];
}

/** A background plate in the style library (decision 0029). */
export interface PlateRow {
  kind: "plate";
  name: string;
  item: { plate: { stops: string[] } };
  builtin?: boolean;
}

/** A cover layer the cut shows (decision 0033: it can be pinned to motion). */
export type CoverRow = { id: string; role: unknown; style: "blur" | "pixelate" | "solid" | null; tracked: number | null; out: [number, number] };

export interface OverlayRow {
  id: string;
  kind: "image" | "shape" | "text" | "video";
  /** A video overlay's media (decision 0028). */
  video?: { loop: boolean; asset: string | null } | null;
  /** Keys of the moving point it is pinned to (decision 0031). */
  tracked?: number | null;
  role?: string | null;
  band?: string | null;
  label?: string | null;
  shape: { kind: "box" | "arrow"; stroke: string; width: number; fill?: string; radius: number; from?: [number, number]; to?: [number, number] } | null;
  asset: string | null;
  opacity: number;
  geom: { x: number; y: number; w: number; h?: number };
  out: [number, number] | null;
  /** Drawn just under the picture layer with this z (decision 0042); null = above it. */
  under?: number | null;
  /** A video overlay with its person cut out (decision 0042). */
  matte?: boolean;
}

export interface CaptionMatchResult {
  name: string;
  frames: number;
  with_text: number;
  y: number;
  size: number;
  weight: string;
  fill: string;
  keyword: string | null;
  outline: number;
  pill: string | null;
  words_per_cue: number;
  cue_s: number | null;
  grouping: string;
  font: string;
  notes: string[];
  ms: number;
}

export interface CaptionGallery {
  frame: number;
  styles: CaptionStyleRow[];
}

export interface FramingInfo {
  active: string | null;
  aspect: "9:16" | "16:9" | "1:1" | null;
  list: { id: string; name: string; w: number; h: number; aspect: string }[];
  layouts: { id: string; name: string }[];
  part_layouts: Record<string, string | null>;
  /** The linked layout's id per part (null: the framing's default stack). */
  part_layout_ids?: Record<string, string | null>;
  presets: { id: string; name: string }[];
}

export interface PartView {
  n: number;
  id: string;
  plan_id: string;
  label: string;
  role: string | null;
  out: [number, number];
  src: [number, number];
  source: string;
  /** Its timeline group (decision 0037): grouped parts move together. */
  group?: string | null;
  layers: LayerView[];
  /** Follow-cursor moves on this part in the framing shown (0: off). */
  follow_moves?: number;
}

export interface CaptionView {
  id: string;
  out: [number, number];
  role: string;
  text: string;
}

/** Frames per second of a `{num, den}` rate (29.97… at an NTSC rate, decision 0020). */
export function rate(f: { num: number; den: number } | undefined | null, fallback = 30): number {
  return f && f.num > 0 && f.den > 0 ? f.num / f.den : fallback;
}

export interface CutView {
  project: string;
  cut: string;
  title: string;
  /** Decision 0011: the stream edit (the main cut, 0036), a long-form or a short. */
  kind?: "stream" | "long" | "short";
  rev: number;
  edited: boolean;
  fps: { num: number; den: number };
  canvas: { w: number; h: number };
  frames: number;
  /** Loudness the export applies, and what the last export measured. */
  loudness?: { mode: "static" | "as_recorded"; target_lufs?: number; true_peak?: number; last: { integrated: number | null; true_peak: number | null } | null };
  /** A paired hi-res picture's offset against the sound (s), when there is one. */
  picture_offset_s?: number | null;
  /** Upload presets the export offers (16:9 framings; 1440p when the source has ≥ 1440 lines). */
  upload_presets?: string[];
  /** R-CC-4: an agent run open on this cut now (its edits land live). */
  agent_working?: { run: string; identity: string } | null;
  /** Each offset stretch: where it starts (voice-file seconds, cut frame) and its offset. */
  picture_offsets?: { from_s: number; offset_s: number; from_frame: number | null }[] | null;
  parts: PartView[];
  captions: CaptionView[];
  words: { state: string; note: string };
  render_sig: string | null;
  export: { state: "never" | "fresh" | "stale"; at_ms?: number; id?: string; reason?: string; verify?: ExportVerify };
  framing: FramingInfo | null;
}

/** What the viewer shows and does (`viewer_state`, the `viewer` scope's `state` event):
 *  generated from the app's Rust type (A2.01, `gen/events.ts`). */
export type ViewerState = GenViewerState;

/** A source's scrub proxy (H.05). */
export interface ProxyJob {
  project: string;
  source: string;
  path: string;
  phase: "queued" | "building" | "done" | "failed" | "stopped";
  covered: number;
  frames: number;
  seconds: number;
  error: string | null;
}

/** One export job (`jobs_list`, the `jobs` scope's `job` event): generated (A2.01). */
export type JobView = GenJobView;

/** One of a cut's two shapes and its last export (`export_framings`, lane K5). */
export interface FramingExport {
  aspect: "9:16" | "16:9";
  id: string;
  name: string;
  w: number;
  h: number;
  export: { state: "never" | "fresh" | "stale"; at_ms?: number; id?: string; file?: string };
}
export interface FramingsView {
  cut: string;
  framings: FramingExport[];
  missing: ("9:16" | "16:9")[];
  supported: boolean;
  why_not: string | null;
}

export interface ExportEntry {
  id: string;
  cut: string;
  title: string;
  file: string;
  frames: number;
  seconds: number;
  render_sig: string | null;
  fps: number;
  at_ms: number;
  notes: string[];
}

export interface BoardTile {
  id: string;
  cut: string;
  title: string;
  file: string;
  at_ms: number;
  seconds: number;
  render_s: number;
  notes: string[];
  verdict: "keep" | "change" | "drop" | null;
  /** The export's review thread, oldest first: review links' notes (`note`, with a
   *  verdict) and comments, and your replies (`human`). Anything not `human` is the
   *  writer's own words: text only (R-UI-SEC-2). */
  thread: { kind: "note" | "comment"; id: string; author: string; human: boolean; verdict: "keep" | "change" | "drop" | null; at_ms: number; text: string }[];
  /** The export's id on review links (null: no export record, nothing to reply under). */
  thread_key: string | null;
  /** The comment a reply goes under (null: your reply starts the thread). */
  thread_root: string | null;
  exists: boolean;
  clippy: { hook: string | null; sent: boolean; why_not: string | null };
  /** The cut's last run's handoff summary: the agent's words (untrusted). */
  handoff: { run: string; text: string; choices: { index: number; label: string; fields: number }[] } | null;
}

export interface Activity {
  holder: boolean;
  paused?: boolean;
  clients?: { identity: string; last_ms: number; exe: string | null; client: string | null; mode: string; revoked: boolean }[];
  runs?: { run: string; project: string; cut: string }[];
  /** `run`: the run it was asked in (R-PROTO-6 pauses that run only); `exportable`:
   * "Approve and export" can run it (decision 0050). */
  approvals?: { id: string; identity: string; run: string; tool: string; effects: Record<string, unknown>; prose: string | null; ts_ms: number; run_paused: boolean; exportable: boolean }[];
  /** R-MCP-1c: first connections from a new program, not yet read. */
  first_seen?: { ts: number; identity: string; exe: string; revoked: boolean }[];
}

/** The last `verify{export}` of an export file (R-EXP-6, H.06). */
export interface ExportVerify {
  passed: boolean;
  at_ms: number;
  how: "export" | "reverify";
  summary: string;
  checks: { step: string; ok: boolean; detail: string }[];
  not_run: string[];
}

/** Home across the library (SPEC §12.6, A2.16): running work in any project, what needs
 *  you, what is done since you last looked, every project's newest exports. A closed
 *  project's rows carry `closed_home` (Home opens it first). */
export interface Home {
  running: (
    | { kind: "export"; project: string; project_name: string | null; title: string; job: string; done: number; frames: number; eta_s: number | null; state: string }
    | { kind: "transcript"; project: string; project_name: string | null; title: string | null; phase: string; done_s: number; total_s: number; eta_s: number | null; state: string }
    | { kind: "agent"; project: string; project_name: string | null; run: string; pass: string; harness_label: string | null; started_ms: number; state: string }
    | { kind: "batch"; project: string; project_name: string | null; batch: string; pass: string; done: number; total: number; state: string }
  )[];
  /** `note` on a review-link ask is the link's own words: untrusted text (R-UI-SEC-2).
   *  `grow`: a staged moment with no cut yet (`moment`, its quote as `title`). */
  needs_you: { project: string; project_name?: string | null; cut: string | null; title: string; why: string | null; action: string; moment?: string; score?: number; closed_home?: string | null; ask?: string; author?: string; note?: { untrusted: boolean; text: string } | null;
    /** `approval`: an agent's ask (decision 0050), `exportable` when "Approve and export" runs it. */
    approval?: string; identity?: string; exportable?: boolean; run_paused?: boolean }[];
  done: { project: string; cut: string; title: string; export: string; at_ms: number; closed_home?: string }[];
  /** The newest exports of every project in the library, newest first. */
  recent?: { project: string; project_name: string; cut: string; title: string; export: string; at_ms: number; seconds: number | null; closed_home: string | null }[];
  /** Every project home in the library folder, from the library index (A2.16). */
  library?: { path: string; id: string; name: string; open: boolean; exports: number; last_export: { title: string; at_ms: number } | null; moments?: { total: number; staged: number; to_grow: number } }[];
}

/** A video's density gate (D.04): moments per active-talk minute against the floor,
 * and the largest stretches of talk no moment covers. */
export interface Density {
  moments: number;
  active_min: number;
  per_min: number;
  one_per_min: number | null;
  target: number;
  short_by: number;
  floor: number;
  ok: boolean;
  stretches: { start_s: number; end_s: number; words: number }[];
}

export interface MomentRow {
  id: string;
  score: number;
  type: string;
  excerpt: string;
  exact: boolean;
  quote: string;
  why: string;
  text_hook: string | null;
  state: "new" | "staged" | "archived" | "used" | "posted";
  origin: string;
  origin_label: string;
  origin_ref: string | null;
  start_s: number | null;
  end_s: number | null;
  review_s: [number, number] | null;
  /** The video it is on (a source or same-recording group) and its file name. */
  owner?: string;
  video?: string | null;
}

export interface CutWord {
  i: number;
  text: string;
  shown: string | null;
  edited: boolean;
  part: number;
  frame: number | null;
  dur_ms: number;
  struck: boolean;
  keyword: boolean;
  /** The video (source or sync group id) and transcript revision the word is from: a
   * cut may play several videos, each word's ordinal is in its own video's transcript. */
  owner?: string;
  revision?: string;
}

export interface StoryView {
  brief: string;
  notes: string;
  parts: { n: number; id: string; role: string; in: string; out: string; dur_s: number }[];
  duration_s: number;
  band: "below" | "inside" | "above";
  band_range: [number, number];
  suggestions: { id: string; role: string; relevance: number; quote: string; state: string; in_s: number | null; out_s: number | null; in: string | null; in_cut: boolean; arc: number | null }[];
  arc_len: number;
}

export interface RecentVideo {
  path: string;
  name: string;
  project: string;
  cut: string;
  duration_s: number;
  opened_ms: number;
  missing: boolean;
}

/** A recent project for Home: its recent videos grouped. */
export interface RecentProject {
  project: string;
  name: string;
  videos: number;
  duration_s: number;
  opened_ms: number;
  /** The video its card shows. */
  thumb: string;
  /** A video of it that is still there (opening it reopens the project). */
  open: string | null;
  missing: boolean;
}

export interface CapLine {
  level: "ok" | "warn" | "no";
  text: string;
  fix: string | null;
}

/** The startup capability probe (R-PLAT-1, H.09). */
export interface StartupCaps {
  machine: { chip: string; apple_silicon: boolean; macos: string; windows?: string };
  probe: { h264_decode: boolean; h264_encode: boolean; vp9_decode: boolean; hevc_decode: boolean; hevc_encode: boolean; av1_decode: boolean };
  lines: CapLine[];
}

export interface ImportInspect {
  path: string;
  name: string;
  duration_s: number;
  video: CapLine;
  tracks: { index: number; n: number; codec: string; rate: number; channels: number; line: CapLine; speech: number | null; level_db: number | null; preview?: string }[];
  ask: boolean;
  suggested: number | null;
  preview_at_s: number;
  preview_rate: number;
  notes: string[];
  known: boolean;
  sheet: boolean;
  /** The ETA chain: the transcript at this Mac's speed (measured, else typical). */
  eta?: { transcript_s: number; speed: number; basis: "measured" | "typical" };
}

export interface Opened {
  project: string;
  cut: string;
  name: string;
  reopened: boolean;
  cancelled?: boolean;
}

/** Several files at once: each file's sheet, and the pairs whose lengths agree within
 * 10 % ("Are these the same recording?"). */
export interface ImportMany {
  files: ImportInspect[];
  pairs: [number, number][];
  failed: { path: string; error: string }[];
}

/** One file of a multi-file open or add, with the mic chosen for it. */
export interface AddFile {
  path: string;
  voice?: number | null;
}

export interface Added {
  project: string;
  cut: string | null;
  added: { path: string; name: string; stream: string; paired: boolean }[];
  skipped: { path: string; name: string; why: string }[];
  notes: string[];
  name?: string;
  reopened?: boolean;
}

export interface Progress {
  phase: "queued" | "transcribing" | "analysing" | "done" | "failed";
  done_s: number;
  total_s: number;
  error: string | null;
  asr_error: string | null;
  /** Seconds left (this run's speed once measured, else this Mac's last). */
  eta_s?: number | null;
}

/** The transcript pane's data: columnar, word `i` in `revision`; `frame[i]` −1 = not in the cut. */
export interface TranscriptData {
  state: "ok" | "transcribing" | "none";
  progress: Progress | null;
  revision?: string;
  fps?: number;
  text: string[];
  start_ms: number[];
  dur_ms: number[];
  frame: number[];
  lines: [number, number][];
  keywords: number[];
  /** Tuned words: ordinal → [start Δms, end Δms] from the ASR edges. */
  tuned?: Record<string, [number, number]>;
  /** Re-spelled words: ordinal → caption text ("" = dropped from captions). */
  edits?: Record<string, string>;
  /** Whose words these are (a source or a same-recording group) and the project's videos
   * (the source picker). */
  owner?: string | null;
  videos?: { owner: string; name: string; in_cut: boolean }[];
}

/** A topic band on the stream edit's transcript (decision 0011), by word ordinal. */
export interface TopicBand {
  id: string;
  /** The video whose transcript its word ordinals are in. */
  owner?: string;
  title: string;
  colour: string;
  state: "proposed" | "accepted" | "rejected";
  origin: "human" | "agent";
  conflict: boolean;
  replaces: string[];
  w0: number;
  w1: number;
  from_ms: number;
  to_ms: number;
  /** Where it plays in the cut (output frames). */
  out: [number, number][];
  /** The stream edit's, shown on another cut that plays its words (read-only there). */
  from_stream?: boolean;
}

export interface FluffRow {
  id: string;
  /** The video whose transcript its word ordinals are in. */
  owner?: string;
  reason: string;
  conflict: boolean;
  origin: "human" | "agent";
  w0: number;
  w1: number;
  from_ms: number;
  to_ms: number;
}

export interface CoverageRow {
  cut: string;
  /** The video whose transcript its word ordinals are in. */
  owner?: string;
  title: string;
  kind: "stream" | "long" | "short";
  w0: number;
  w1: number;
}

export interface StreamOverlay {
  stream: boolean;
  revision?: string;
  frames?: number;
  topics: TopicBand[];
  fluff: FluffRow[];
  coverage: CoverageRow[];
}

export interface MapFraming {
  id: string;
  name: string;
  aspect: string;
}

export interface MapCut {
  cut: string;
  title: string;
  kind: "stream" | "long" | "short";
  duration_ms: number;
  framings: MapFraming[];
  topics: string[];
  ranges: [number, number][];
  parent_edits: number;
  review: string | null;
  export?: { state: "never" | "fresh" | "stale"; at_ms?: number };
  /** The videos its parts play, in first-use order. */
  media?: MediaRef[];
}

export type MediaRef = { source: string } | { group: string };

/** One video of the project (the map's Sources section). */
export interface MapSource {
  id: string;
  name: string;
  path: string;
  duration_ms: number;
  width: number | null;
  height: number | null;
  fps: { num: number; den: number } | null;
  audio_tracks: number;
  group: string | null;
  group_role: "picture" | "audio" | "both" | null;
  media: MediaRef;
  stream: string | null;
  used_by: string[];
  moments: number;
  offline?: boolean;
  owner?: string;
  asr?: { phase: string; done_s: number; total_s: number; transcript: boolean; error: string | null; asr_error: string | null } | null;
  proxy?: string | null;
  /** Its stream layout's name ("Draw your layout once"), and the roles it boxes. */
  layout?: string | null;
  layout_roles?: string[];
}

export interface MapTopic {
  id: string;
  title: string;
  colour: string;
  origin: "human" | "agent";
  state: "proposed" | "accepted" | "rejected";
  conflict: boolean;
  replaces: string[];
  /** The video it is on (the main cut may hold several). */
  media: MediaRef;
  from_ms: number;
  to_ms: number;
  kept_ms: number;
}

export interface MapStream {
  cut: string;
  title: string;
  /** Its video when it holds one. */
  media?: MediaRef | null;
  /** Every video it holds, in the order added (decision 0036). */
  videos?: MediaRef[];
  total_ms: number;
  kept_ms: number;
  framings: MapFraming[];
  topics: MapTopic[];
  fluff_proposed: number;
  export?: { state: "never" | "fresh" | "stale"; at_ms?: number };
}

export interface ProjectMapData {
  /** The main cut: the project's stream edit, holding every video (decision 0036). */
  stream: MapStream | null;
  /** The same, as a list (one at most). */
  streams?: MapStream[];
  sources?: MapSource[];
  longs: MapCut[];
  shorts: MapCut[];
}

export interface ParentEdit {
  kind: "struck" | "restored";
  topic: string;
  topic_title: string;
  from_ms: number;
  dur_ms: number;
}

export interface ParentDiff {
  cut: string;
  parent: string;
  edits: ParentEdit[];
  struck_ms: number;
  restored_ms: number;
  topics_gone: string[];
}

export interface CueWord {
  rev: string;
  ordinal: number;
  text: string;
  break: "split" | "join" | null;
  star: boolean;
}

export interface ReviewStatus {
  serving: string | null;
  mode: "loopback" | { tailnet: { host: string; owner_login: string } };
  links: { id: string; created_ms: number; expires_ms: number; revoked: boolean; live: boolean; login: string | null; role?: "reviewer" | "editor" }[];
  remote_fix: { max_usd_per_run: number; max_runs_per_day: number } | null;
  remote_send?: { max_sends_per_day: number } | null;
}

export interface TermGate {
  rev: string;
  terms: number;
  findings: { ordinals: number[]; shown: string; written: string; kind: "variant" | "near"; frame: number | null }[];
}

export interface PairProposal {
  offset_s: number | null;
  stretches: { from_s: number; offset_s: number; confidence: number; picture: boolean; windows: number; frames: number | null }[];
  unmatched: { from_s: number; to_s: number }[];
  fps: number | null;
  took_s: number;
}

export interface ShortView {
  cues: { id: string; text: string; start: number; end: number; words: CueWord[] }[];
  hand_breaks: number;
  keyword_terms: string[];
  /** Word groups captions keep together (project-wide). */
  keep: string[];
  /** The captions' centre height on the frame (canvas px), as planned. */
  caption_y: number | null;
  captions: boolean;
  imported_captions: boolean;
  fixes: { from: string; to: string; n: number }[];
  overlays: { id: string; role: string | null; text: string; counter: { running: string; done: string } | null; start: number | null; end: number | null }[];
  measurements: { id: string; value: number; method: string }[];
  transcript: boolean;
  /** AGENTS.md (h53): the layout changes here (take `take`) while the hook is still up. */
  hook_layout?: { changes_at: number; take: number; hook_end: number } | null;
}

/** The timeline's editable lanes (U.08): the cut's own overlays, gain segments and the
 * measurements it shows, on the output clock. */
export interface TimelineLanes {
  /** False for an imported RenderPlan: its text is the plan's (read-only lanes). */
  overlays_editable: boolean;
  /** False for an imported RenderPlan: the gain lane shows its envelopes read-only. */
  gains_editable: boolean;
  overlays: {
    id: string;
    role: string | null;
    text: string;
    out: [number, number];
    counter: boolean;
    kind: "text" | "image" | "video" | "shape" | "counter";
    /** Drawn just under the picture layer with this z (decision 0042); null = above the picture. */
    under: number | null;
    /** A video overlay with its person cut out. */
    matte: boolean;
    /** Hidden from the layer rows (its opacity 0). */
    hidden?: boolean;
    /** What an image or video shows (its source or asset): the pieces of one cut overlay share it. */
    media?: string | null;
  }[];
  /** The picture's layers by z (decision 0042): where an image or video can sit. */
  picture_layers?: { z: number; role: string }[];
  gains: { id: string; gain_db: number; out: [number, number]; ramp_in_ms: number; ramp_out_ms: number }[];
  measurements: { id: string; value: number; unit: string; method: string; out: [number, number]; evidence: number }[];
}

export interface SweepRow {
  i: number;
  out: [number, number] | null;
  seconds: number;
  reason: string;
}

/** The sweeps (SPEC §5.5): pauses, fillers, and retakes (decision 0040). */
export type SweepKind = "pauses" | "fillers" | "retakes";

export interface SweepPreview {
  /** What `sweep_apply` applies, exactly (A2.06): Conflict if the cut moved since. */
  preview_id: string;
  kind: SweepKind;
  cuts: SweepRow[];
  skipped: number;
  removes_s: number;
  revision: string;
  as_of: number;
}

export interface AgentRun {
  id: string;
  project: string;
  cut: string;
  pass: "find_moments" | "full_sweep" | "grow_story" | "edit_pass" | "topics_fluff" | "fix_comments" | "thumbnail" | "publish_copy";
  harness: string;
  state: "starting" | "checking" | "running" | "done" | "failed";
  started_ms: number;
  ended_ms: number | null;
  error: string | null;
  tool_calls: number | null;
  exit_code: number | null;
  found: number | null;
  summary: string | null;
  /** The client build failed (or is not covered by) gmclip's isolation check (S7). */
  blocked?: { version: string; sha256: string; failed: { id: string; what: string; detail: string }[] };
  /** The client's name ("Claude Code", "pi", …). */
  harness_label?: string;
  /** G.04: what the running agent last looked at (from its own tool-call log). */
  looking_at?: AgentFocus | null;
  /** G.03: the app run this one continues, and whether it resumed the client's session. */
  continued_from?: string | null;
  resumed?: boolean;
  /** Finished runs: the client resumes its own session on "Continue" (else a fresh run seeded with the summary). */
  resumes?: boolean;
}

/** What a running agent is looking at: the latest see / listen / transcript_read. */
export interface AgentFocus {
  tool: "see" | "listen" | "transcript_read";
  label: string;
  target: string | null;
  frames_s: number[] | null;
  range_s: [number, number] | null;
  part: string | null;
  call: number;
}

/** One of the user's agents as gmclip found it (G.02). */
export interface AgentClient {
  harness: "claude-code" | "codex" | "pi" | "opencode";
  label: string;
  installed: boolean;
  found: string | null;
  version: string | null;
  set_by_owner: boolean;
  /** ready (tested), allowed (this exact build, by the check or by you), check (a
   *  one-time check / your allowance on first run), blocked (the check failed: "Run
   *  anyway" on the Owner sheet), missing. Every client, Codex included, takes this one
   *  path (decision 0012 amendment, 2026-10-16). */
  status: "ready" | "allowed" | "check" | "blocked" | "missing";
  reason: string | null;
  resumes: boolean;
  resume_note: string | null;
  how: string;
  login: string;
}

/** The person's view choices (app settings, `ui-prefs.json`; never profile fields). */
export interface UiPrefs {
  hover_scrub: boolean;
  safe_zones: boolean;
  map_open: boolean;
  render_stats: boolean;
  /** The timeline's ∿ waveform band under each part. */
  waveform?: boolean;
  coach_seen: string[];
  /** The cut view's panes (View menu): shown or hidden, kept across launches. */
  side_open?: boolean;
  timeline_open?: boolean;
  words_open?: boolean;
  lanes_open?: boolean;
  /** Focus: only the viewer and the timeline. */
  focus?: boolean;
  /** Pane sizes dragged on their splitters, in px (0 = the layout's own size). */
  side_w?: number;
  map_w?: number;
  tl_h?: number;
  /** The transcript: times beside each line, its text size and the reading view's. */
  tr_times?: boolean;
  tr_size?: number;
  read_size?: number;
  /** Appearance: colour theme and how tight the controls sit. */
  theme?: "dark" | "light" | "system";
  density?: "comfortable" | "compact";
  /** The panes a launch opens with: the last layout, every pane, or focus. */
  open_panes?: "last" | "all" | "focus";
  /** Workspace layouts the person saved (View › Layouts). */
  workspaces?: Workspace[];
  /** Settings › Recording (decision 0045): the Record button on Home. `recording_enable` sets it. */
  recording?: boolean;
  /** Where recordings go ("" = the library's sibling Recordings folder). */
  recordings_folder?: string;
  /** Settings › Developer (decision 0054): the team and in-app agent surfaces, every side
   *  panel in the bar, and the preview's measuring tools. Off by default. */
  developer?: boolean;
}

/** A named pane arrangement: the pane keys it sets (absent = left as they are). */
export interface Workspace {
  name: string;
  map_open?: boolean;
  side_open?: boolean;
  timeline_open?: boolean;
  words_open?: boolean;
  lanes_open?: boolean;
  focus?: boolean;
  reading?: boolean;
  side_w?: number;
  map_w?: number;
  tl_h?: number;
  /** A side tab by its name then (layout.ts `tabFrom` reads an older one). */
  side_tab?: string;
}

export interface AgentStatus {
  holder: boolean;
  note: string | null;
  connect: { command: string; args: string[]; claude_code: string; codex: string } | null;
  runs: AgentRun[];
  client: { found: boolean; path?: string; set_by_owner?: boolean; message?: string; harness?: string; label?: string };
  clients?: AgentClient[];
  /** The default agent (an Owner setting, never a profile field). */
  default?: AgentClient["harness"];
}

export interface BackupInfo {
  id: string;
  at_ms: number;
  reason: string;
  rev: number;
  bytes: number;
  name: string;
}

export interface StorageRow {
  id: string;
  label: string;
  bytes: number;
  freeable: boolean;
  what: string;
}

export interface SetupAnswers {
  kit: string;
  platforms: string[];
  finder: "agent" | "self";
  harness?: AgentClient["harness"] | null;
  recordings?: string | null;
}

export interface SetupPreview {
  kits: { id: string; label: string; blurb: string }[];
  platforms: { id: string; label: string; framing: string; preset: string; zone: string }[];
  agents: { clients: AgentClient[]; default: AgentClient["harness"] };
  recordings_default: string | null;
  answers: SetupAnswers | null;
  done: boolean;
  no_transcribe: boolean;
  models: ModelsStatus;
}

export interface ModelsStatus {
  installed: boolean;
  missing_files?: number;
  bytes?: number;
  mb?: number;
  running?: boolean;
  error?: string;
  partial_bytes?: number;
  licences?: { id: string; weights: string; code: string }[];
}

export interface SummaryLine {
  id: string;
  label: string;
  value: string;
}

export interface Checklist {
  show: boolean;
  dismissed: boolean;
  imported: boolean;
  done: boolean;
  steps: { id: string; label: string; how: string; done: boolean }[];
}

export const actions = {
  /** First run (lane S): the four questions, the summary, one sheet. */
  setupPreview: () => call<SetupPreview>("setup_preview"),
  /** Cheap: whether setup is done and its answers (no agent detection, no model check). */
  setupState: () => call<{ done: boolean; answers: SetupAnswers | null; no_transcribe: boolean }>("setup_state"),
  setupApply: (answers: SetupAnswers, dryRun = false) =>
    call<{ summary: SummaryLine[]; sheet?: string[]; profile: string; new_profile: boolean; applied?: string[] }>("setup_apply", { answers, dry_run: dryRun }),
  setupNoTranscribe: (on: boolean) => call<{ no_transcribe: boolean }>("setup_no_transcribe", { on }),
  checklistView: () => call<Checklist>("checklist_view"),
  checklistDismiss: (dismissed = true) => call<Checklist>("checklist_dismiss", { dismissed }),
  modelsPause: () => call<{ paused: boolean }>("models_pause"),
  /** Rolling backups (lane S): every project with copies, newest first. */
  backupsOverview: () =>
    call<{ projects: { project: string; name: string; open: boolean; home: string | null; backups: BackupInfo[] }[]; keep_recent: number; keep_daily: number; every_min: number }>("backups_overview"),
  backupNow: (project: string) => call<{ backup: BackupInfo | null }>("backup_now", {}, project),
  backupRestore: (project: string, backup: string) => call<{ restored: BackupInfo; before: BackupInfo | null; project: ProjectInfo }>("backup_restore", { project, backup }),
  storageUsage: () => call<{ rows: StorageRow[]; freeable_bytes: number; kept_open_bytes: number; app_data: string; cache: string }>("storage_usage"),
  storageFree: (kinds: string[] = []) => call<{ freed_bytes: number; kept: string[] }>("storage_free", { kinds }),
  modelsStatus: () => call<ModelsStatus>("models_status"),
  modelsDownload: () => call<{ started?: boolean; installed?: boolean }>("models_download"),
  agentClientSet: (path: string, harness?: AgentClient["harness"]) => call("agent_client_set", harness ? { path, harness } : { path }),
  agentClients: (refresh = false) => call<{ clients: AgentClient[]; default: AgentClient["harness"] }>("agent_clients", { refresh }),
  agentDefaultSet: (harness: AgentClient["harness"]) => call<{ default: AgentClient["harness"] }>("agent_default_set", { harness }),
  agentContinue: (run: string, note: string) => call<AgentRun>("agent_continue", { run, note }),
  ownerRequest: (command: string, args: Record<string, unknown>) => call("owner_request", { command, args }),
  /** A batch (SPEC §10.7.1): its budget ($), wall clock (h), per-cut limit (min) and
   * agent are the sheet's; the native sheet asks once (with the budget when > $5). */
  batchStart: (project: string, cuts: string[], pass: string, opts: { budget_usd: number; wall_clock_h: number; per_cut_min: number; harness?: AgentClient["harness"] }) =>
    call("batch_start", { cuts, pass, ...opts }, project),
  batchList: (project: string) =>
    call<{ batches: { id: string; pass: string; state: string; started_ms: number; budget_usd: number; wall_clock_s: number; per_cut_s: number; harness: string | null; preapproval: { covers: string[]; budget_usd: number; recorded_ms: number }; cuts: { cut: string; title: string; state: string; run: string | null; reason: string | null }[] }[] }>("batch_list", {}, project),
  /** A human handled an ask (`id`), or reviewed a paused run (`run`). */
  approvalResolve: (id: string | null, run: string | null) => call<{ resolved: number }>("approval_resolve", { ...(id ? { id } : {}), ...(run ? { run } : {}) }),
  /** "Approve and export" (decision 0050): the human runs the export an agent was refused. */
  approvalExport: (id: string) => call<{ approval: string; exported: { job?: string; jobs?: { job: string }[] } }>("approval_export", { id }),
  reviewStatus: () => call<ReviewStatus>("review_status"),
  reviewServe: (on: boolean) => call<ReviewStatus>("review_serve", { on }),
  reviewLinkNew: (login?: string) => call<{ id: string; url: string; expires_ms: number }>("review_link_new", login ? { login } : {}),
  reviewLinkRevoke: (id: string | null) => call<{ revoked: number }>("review_link_revoke", id ? { id } : {}),
  /** A cut's title (the core's `cut_set`; one undo step). */
  cutRename: (project: string, cut: string, title: string) => call<Out<"cut_set">>("cut_set", { cut, title } satisfies Args<"cut_set">, project),
  uiPrefsGet: () => call<UiPrefs>("ui_prefs_get"),
  uiPrefsSet: (patch: Partial<UiPrefs>) => call<UiPrefs>("ui_prefs_set", patch),
  ownerSettings: () =>
    call<{
      read_roots?: string[];
      write_roots?: string[];
      recordings?: string | null;
      revoked?: string[];
      note?: string;
      profile?: string | null;
      style?: { path: string; sha256: string; changed_on_disk?: boolean; unreadable?: string | null } | null;
      agent?: { found: boolean; path?: string; message?: string };
    }>("owner_settings"),
  /** Owner (R-PROF-5): pick the style rules file in the native panel; the native sheet pins it. */
  stylePick: () => call<{ cancelled?: boolean; path?: string; sha256?: string }>("style_pick"),
  /** Owner (R-PROF-3): the style file's changes on one native sheet ("Accept all"). */
  styleReview: () => call<{ pending: boolean; accepted?: boolean }>("style_review"),
  agentStatus: (project?: string) => call<AgentStatus>("agent_status", project ? { project } : {}),
  agentRun: (project: string, cut: string, pass: AgentRun["pass"], brief = "", media = false, harness?: AgentClient["harness"]) =>
    call<AgentRun>("agent_run", { cut, pass, brief, media, ...(harness ? { harness } : {}) }, project),
  saveState: (project: string) => call<{ rev: number; saved: number; error: string | null; state: "saved" | "saving" | "failed" }>("project_save_state", {}, project),
  restorePoint: (project: string) => call<{ restore_point: { id: string; rev: number }; error: string | null }>("project_restore_point", {}, project),
  timelineLanes: (project: string, cut: string) => call<TimelineLanes>("timeline_lanes", { cut }, project),
  /** Pointer down on an overlay or gain segment: the core holds it (R-GEST-1) until the
   * commit below (which ends it) or `timelineGestureEnd` (Esc). */
  timelineGestureBegin: (project: string, kind: GestureKind, id: string, part?: string) =>
    call<{ gesture: number }>("timeline_gesture_begin", { kind, id, ...(part ? { part } : {}) }, project),
  timelineGestureEnd: (project: string, gesture: number) => call("timeline_gesture_end", { gesture }, project),
  overlayTimeSet: (project: string, cut: string, overlay: string, from: number | null, to: number | null, gesture: number | null) =>
    call("overlay_time_set", { cut, overlay, ...(from !== null ? { from } : {}), ...(to !== null ? { to } : {}), ...(gesture !== null ? { gesture } : {}) }, project),
  /** Cuts an overlay at an output frame (on a part edge, each piece follows its part). */
  overlaySplitAt: (project: string, cut: string, overlay: string, frame: number) => call("overlay_split_at", { cut, overlay, frame }, project),
  /** Cuts an overlay at every part edge it spans: one piece per part, each moving with its part. */
  overlaySplitParts: (project: string, overlay: string) =>
    call<Out<"overlay_split_parts">>("overlay_split_parts", { overlay } satisfies Args<"overlay_split_parts">, project),
  gainSegmentEdit: (project: string, cut: string, segment: string, from: number | null, to: number | null, gainDb: number | null, gesture: number | null) =>
    call("gain_segment_edit", { cut, segment, ...(from !== null ? { from } : {}), ...(to !== null ? { to } : {}), ...(gainDb !== null ? { gain_db: gainDb } : {}), ...(gesture !== null ? { gesture } : {}) }, project),
  /** The audio lane as JSON, per part: the timeline's fallback where `gm://data` cannot be read. */
  timelineAudio: (project: string, cut: string) =>
    call<{ parts: { out: [number, number]; db: number[]; quiet: [number, number][]; onsets?: number[] }[]; analysed: boolean }>("timeline_audio", { cut }, project),
  /** Energy onsets near each part's edges (trims snap to them); the levels come from the columns. */
  timelineOnsets: (project: string, cut: string) =>
    call<{ parts: { out: [number, number]; onsets?: number[] }[]; analysed: boolean }>("timeline_audio", { cut, onsets_only: true }, project),
  /** The timeline's columns (`gm://data`: peaks, energy, quiet, words; D.02). */
  cutColumns: (project: string, cut: string, kinds?: readonly ColumnKind[]) => cutColumns(project, cut, kinds),
  sweepPreview: (project: string, cut: string, kind: string, minS: number, keepS: number) =>
    call<SweepPreview>("sweep_preview", { cut, kind, min_s: minS, keep_s: keepS }, project),
  sweepApply: (project: string, cut: string, previewId: string, take: number[]) =>
    call<{ applied: number }>("sweep_apply_app", { cut, preview_id: previewId, take }, project),
  holdToggle: (project: string, cut: string, frame: number) => call<{ held: boolean; until?: "sentence" | "part" }>("hold_toggle", { cut, frame }, project),
  reorderTo: (project: string, cut: string, part: string, index: number) => call("part_reorder_to", { cut, part, index }, project),
  /** Several parts to position `index` among the others, back to back (decision 0037). */
  partsMoveTo: (project: string, cut: string, parts: string[], index: number) => call("parts_move_to", { cut, parts, index }, project),
  short: (project: string, cut: string) => call<ShortView>("short_view", { cut }, project),
  captionsSet: (project: string, cut: string, on: boolean) => call("captions_set", { cut, on }, project),
  captionReplace: (project: string, cut: string, rev: string, hits: number[][], to: string) =>
    call<{ matches: number }>("caption_replace", { cut, rev, hits, to }, project),
  captionFix: (project: string, cut: string, from: string, to: string) => call<{ words: number }>("caption_fix", { cut, from, to }, project),
  cutChecks: (project: string, cut: string) =>
    call<{ findings: { rule: string; level: "info" | "warning" | "error"; message: string; fix?: { label: string; command?: string; action?: "export" } | null }[] }>("cut_checks", { cut }, project),
  /** A2.17: run finding `n`'s one-click fix (the app works it out again from the cut). */
  checkFix: (project: string, cut: string, n: number, rule: string) => call("check_fix", { cut, n, rule }, project),
  termGate: (project: string, cut: string) => call<TermGate>("term_gate", { cut }, project),
  termAdd: (project: string, written: string, heardAs: string, remove = false) => call("term_add", { written, heard_as: heardAs, remove }, project),
  captionY: (project: string, cut: string, y: number) => call("caption_y", { cut, y }, project),
  keepSet: (project: string, words: string, keep: boolean) => call<{ groups: number }>("caption_keep", { words, keep }, project),
  holdAt: (project: string, cut: string, part: string, atS: number) => call("hold_at", { cut, part, at_s: atS }, project),
  keywordsSet: (project: string, cut: string, terms: string) => call<{ terms: string[] }>("caption_keywords", { cut, terms }, project),
  cueBreak: (project: string, cut: string, w: { rev: string; ordinal: number }, brk: "split" | "join" | null) =>
    call("cue_break", { cut, rev: w.rev, ordinal: w.ordinal, ...(brk ? { break: brk } : {}) }, project),
  hookSet: (project: string, cut: string, text: string, until: number | null, durMs: number | null) =>
    call("hook_set", { cut, text, ...(until !== null ? { until } : {}), ...(durMs !== null ? { dur_ms: durMs } : {}) }, project),
  pillAdd: (project: string, cut: string, frame: number, text: string) => call("pill_add", { cut, frame, text }, project),
  runMeasure: (project: string, cut: string, from: number, to: number) => call<{ measurement: string; seconds: number }>("run_measure", { cut, from, to }, project),
  clockAdd: (project: string, cut: string, measurement: string, running: string, done: string, fromPart?: string, untilPart?: string) =>
    call("clock_add", { cut, measurement, running, done, ...(fromPart ? { from_part: fromPart } : {}), ...(untilPart ? { until_part: untilPart } : {}) }, project),
  streamView: (project: string, cut: string, atS: number) => call<{ t0_s: number; fps: number; frame: number; frames: number }>("stream_view", { cut, at_s: atS }, project),
  runMeasureSrc: (project: string, cut: string, fromS: number, toS: number) => call<{ measurement: string; seconds: number }>("run_measure_src", { cut, from_s: fromS, to_s: toS }, project),
  overlayDelete: (project: string, cut: string, overlay: string) => call("short_overlay_delete", { cut, overlay }, project),
  // The look (studio): caption style gallery and my style library.
  captionGallery: (project: string, cut: string, frame: number) => call<CaptionGallery>("caption_gallery", { cut, frame }, project),
  captionStyleApply: (project: string, cut: string, name: string) => call("caption_style_apply", { cut, name }, project),
  captionStyleSaveMine: (project: string, cut: string, name: string) => call("caption_style_save_mine", { cut, name }, project),
  captionStyleForgetMine: (project: string, name: string) => call("caption_style_forget_mine", { name }, project),
  /** A caption style measured from a short's burned-in captions (an approximation),
   * saved as a project style for the gallery. */
  captionStyleMatch: (project: string, cut: string, path: string) => call<CaptionMatchResult>("caption_style_match", { cut, path }, project),
  imageOverlayAdd: (project: string, cut: string, frame: number, name: string, data: string) =>
    call<{ overlay: string; asset: string; w: number; h: number }>("image_overlay_add", { cut, frame, name, data }, project),
  imageOverlayAddPath: (project: string, cut: string, frame: number, path: string) =>
    call<{ overlay: string }>("image_overlay_add", { cut, frame, path }, project),
  imageThumb: (project: string, asset: string) => call<{ w: number; h: number; rgb: string }>("image_thumb", { asset }, project),
  calloutAdd: (project: string, cut: string, frame: number, kind: "box" | "arrow") => call<{ overlay: string }>("callout_add", { cut, frame, kind }, project),
  overlaysView: (project: string, cut: string) => call<{ overlays: OverlayRow[]; picture_layers?: { z: number; role: string }[]; covers?: CoverRow[]; background: unknown; backdrop: { name: string; video: boolean; w?: number; h?: number } | null; caption_look: CaptionLook | null; caption_grouping: string | null }>("overlays_view", { cut }, project),
  captionLookSet: (project: string, framing: string, look: CaptionLook) => call<Out<"framing_set">>("framing_set", { framing, caption_look: look } satisfies Args<"framing_set">, project),
  captionGroupingSet: (project: string, framing: string, grouping: CaptionGrouping) => call<Out<"framing_set">>("framing_set", { framing, caption_grouping: grouping } satisfies Args<"framing_set">, project),
  /** A framing's background: a solid colour, the part's own picture blurred (`/1`), or a
   * stored backdrop (decision 0028). */
  backgroundSet: (project: string, framing: string, background: "blurred_source" | { solid: { colour: string } } | { backdrop: { asset: string; loop: boolean; fit: "cover" | "contain" } }) =>
    call<Out<"framing_set">>("framing_set", { framing, background } satisfies Args<"framing_set">, project),
  /** A video or still file as the cut's backdrop: under every part, running on across joins (decision 0028). */
  backdropSet: (project: string, cut: string, path: string, loop = true, fit: "cover" | "contain" = "cover") =>
    call<{ asset: string; framing: string }>("backdrop_set", { cut, path, loop, fit }, project),
  /** Background plates (decision 0029): the built-in ones and the project's, mine, the current one. */
  platesView: (project: string, cut: string) =>
    call<{ project: PlateRow[]; mine: PlateRow[]; current: string | null }>("plates_view", { cut }, project),
  /** Saves a plate (stops top to bottom) in the project and in my style library. */
  plateSave: (project: string, name: string, stops: string[]) => call<{ saved: string }>("plate_save", { name, stops }, project),
  /** Applies a plate to a framing (a built-in or the project's by name, or mine). */
  plateApply: (project: string, framing: string, name: string, mine: boolean) =>
    call("style_item_apply", { kind: "plate", name, mine, to: { framing } }, project),
  /** "Pin to motion" (decision 0031): follows the video under the overlay with Apple Vision and pins it there. */
  overlayTrack: (project: string, cut: string, overlay: string) =>
    call<{ keys: number; takes: number; seconds: number; lost: boolean; until_s: number }>("overlay_track", { cut, overlay }, project),
  /** "Pin to motion" on a cover (decision 0033): from the playhead, through every take of its video. */
  coverTrack: (project: string, cut: string, layer: string, frame: number) =>
    call<{ keys: number; takes: number; seconds: number; lost: boolean }>("cover_track", { cut, layer, frame }, project),
  /** Unpins a cover from its moving point. */
  coverUnpin: (project: string, layer: string) =>
    call("layer_track_set", { layer, track: null } satisfies Args<"layer_track_set">, project),
  /** "Blur here": a blur cover over the picture at a canvas point on a frame (decision 0033). */
  coverAdd: (project: string, cut: string, frame: number, x = 0.5, y = 0.5) =>
    call<{ layer: string }>("cover_add", { cut, frame, x, y }, project),
  /** Unpins an overlay from its moving point. */
  overlayUnpin: (project: string, overlay: string) =>
    call("overlay_track_set", { overlay, track: null } satisfies Args<"overlay_track_set">, project),
  /** A video file picture-in-picture at the playhead, silent (decision 0028). */
  videoOverlayAdd: (project: string, cut: string, frame: number, path: string) =>
    call<{ overlay: string; seconds: number }>("video_overlay_add", { cut, frame, path }, project),
  overlayGeom: (project: string, overlay: string, geom: Pick<Args<"overlay_set">, "rect" | "opacity" | "shape">) =>
    call<Out<"overlay_set">>("overlay_set", { overlay, ...geom } satisfies Args<"overlay_set">, project),
  overlayPlace: (project: string, overlay: string, place: { x: number; y: number; w: number }) => call<Out<"overlay_move">>("overlay_move", { overlay, place } satisfies Args<"overlay_move">, project),
  overlayRemove: (project: string, overlay: string) => call<Out<"overlay_delete">>("overlay_delete", { overlay } satisfies Args<"overlay_delete">, project),
  brandKit: (project: string) => call<BrandView>("brand_kit_view", {}, project),
  brandKitSave: (project: string, kit: BrandKitFields) => call<BrandView>("brand_kit_save", { ...kit }, project),
  brandKitUseMine: (project: string) => call<BrandView & { skipped: string[] }>("brand_kit_use_mine", {}, project),
  brandLogoSet: (project: string, name: string, data: string) => call<BrandView>("brand_logo_set", { name, data }, project),
  lowerThirdAdd: (project: string, cut: string, frame: number) => call<{ ids: string[] }>("lower_third_add_at", { cut, frame }, project),
  endCardAdd: (project: string, cut: string, loopLayout: boolean) => call<{ ids: string[] }>("end_card_add_app", { cut, loop_layout: loopLayout }, project),
  templateSave: (project: string, overlay: string, name: string) => call<BrandView>("overlay_template_save_app", { overlay, name }, project),
  templateInsert: (project: string, cut: string, name: string, frame: number) => call<{ id: string }>("overlay_template_insert_at", { cut, name, frame }, project),
  captionsFile: (project: string, cut: string, format: "srt" | "vtt" | "ass") =>
    call<{ path: string; cues: number; format: string }>("captions_file", { cut, format }, project),
  pairPropose: (voice: string, hires: string) =>
    call<PairProposal>("pair_propose", { voice, hires }),
  openVideo: (path: string, voice?: number | null, hires?: { path: string; offset_s: number; segments?: { from_s: number; offset_s: number }[] }) =>
    call<Opened>("video_open", { path, ...(voice != null ? { voice } : {}), ...(hires ? { hires } : {}) }),
  cutFromMoment: (project: string, moment: string) =>
    call<{ cut: string; seconds: number; stream_layout?: { matched: string | null; ambiguous?: string[] }; needs_layout?: string | null }>("cut_from_moment", { moment }, project),
  loudnessSet: (project: string, cut: string, lufs: number | null) => call("loudness_set", lufs === null ? { cut } : { cut, lufs }, project),
  soundView: (project: string, cut: string) =>
    call<{ gains: { id: string; gain_db: number; from: number | null; to: number | null }[]; hides: { id: string; from: number | null; to: number | null }[]; parts: { id: string; gain_db: number; speed?: number; ramp?: { to: number; src_start: number; src_end: number; curve: string } | null; under?: { member: string; member_db: number | null; part_db: number | null } }[]; no_captions?: { id: string; from: number | null; to: number | null }[]; sounds?: { id: string; name: string; gain_db: number; duck: boolean; from: number | null; to: number | null }[] }>("sound_view", { cut }, project),
  /** Music over the whole cut from an M4A or WAV file, −18 dB, ducked under the voice (decision 0051). */
  musicAdd: (project: string, cut: string, path: string) => call<Out<"audio_overlay_add">>("music_add", { cut, path }, project),
  /** Changes music or a sound effect over the cut: gain, fades, ducking (`no_duck` stops it). */
  soundSet: (project: string, overlay: string, patch: Omit<Args<"audio_overlay_set">, "overlay">) =>
    call<Out<"audio_overlay_set">>("audio_overlay_set", { overlay, ...patch } satisfies Args<"audio_overlay_set">, project),
  soundRemove: (project: string, overlay: string) =>
    call<Out<"audio_overlay_delete">>("audio_overlay_delete", { overlay } satisfies Args<"audio_overlay_delete">, project),
  noCaptions: (project: string, cut: string, from: number, to: number) => call("no_captions_add", { cut, from, to }, project),
  noCaptionsDelete: (project: string, cut: string, id: string) => call("no_captions_delete", { cut, id }, project),
  gainSpan: (project: string, cut: string, from: number, to: number, gainDb: number) => call("gain_span_add", { cut, from, to, gain_db: gainDb }, project),
  gainSpanDelete: (project: string, segment: string) => call("gain_span_delete", { segment }, project),
  partGain: (project: string, part: string, gainDb: number) => call("part_gain_set", { part, gain_db: gainDb }, project),
  /** A recording's system audio under a part's voice (decision 0051): dB, −60 silent, null = the recording's own level. */
  partUnder: (project: string, parts: string[], underDb: number | null) =>
    call<Out<"part_set_under">>("part_set_under", { parts, under_db: underDb } satisfies Args<"part_set_under">, project),
  /** The recording's own level for its system audio under the voice, wherever a part does not say (null: not heard). */
  syncUnder: (project: string, member: string, underDb: number | null) =>
    call<Out<"sync_under_set">>("sync_under_set", { member, under_db: underDb } satisfies Args<"sync_under_set">, project),
  /** Plays a part ¼× to 4× (decision 0030): sound stretched with its pitch kept from ½× to 2×, muted beyond. */
  partSpeed: (project: string, part: string, speed: number) =>
    call<Out<"part_set_speed">>("part_set_speed", { part, speed } satisfies Args<"part_set_speed">, project),
  /** Eases a part's speed to another over a stretch of it (decision 0033); null removes the ramp. */
  partRamp: (project: string, part: string, ramp: { to: number; start_ms: number; dur_ms: number; curve: "linear" | "ease_in" | "ease_out" | "ease_in_out" } | null) =>
    call<Out<"part_set_ramp">>("part_set_ramp", { part, ramp } satisfies Args<"part_set_ramp">, project),
  camHide: (project: string, cut: string, from: number, to: number) => call("cam_hide_add", { cut, from, to }, project),
  camHideDelete: (project: string, cut: string, hide: string) => call("cam_hide_delete", { cut, hide }, project),
  pictureOffset: (project: string, cut: string, offsetS: number, frame?: number, split?: boolean) =>
    call("picture_offset_set", { cut, offset_s: offsetS, ...(frame !== undefined ? { frame } : {}), ...(split ? { split } : {}) }, project),
  inspectVideo: (path: string) => call<ImportInspect>("video_inspect", { path }),
  // Several videos in one project (AJ 2026-10-15).
  inspectVideos: (paths: string[]) => call<ImportMany>("videos_inspect", { paths }),
  openVideos: (files: AddFile[], same: [number, number][], oneProject: boolean, name?: string) =>
    call<Added>("videos_open", { files, same, one_project: oneProject, ...(name ? { name } : {}) }),
  addVideos: (project: string, files: AddFile[], same: [number, number][]) => call<Added>("videos_add", { files, same }, project),
  pickVideos: () => call<{ picked: string[] }>("videos_pick"),
  removeVideo: (project: string, source: string) =>
    call<{ stream_edits: string[]; moments: number; group: string | null }>("video_remove", { source }, project),
  relinkVideo: (project: string, source: string, path?: string) =>
    call<{ relinked?: boolean; cancelled?: boolean }>("video_relink", path ? { source, path } : { source }, project),
  pickVideo: () => call<{ picked?: string; cancelled?: boolean }>("video_pick"),
  recentVideos: () => call<{ videos: RecentVideo[]; projects?: RecentProject[] }>("videos_recent"),
  /** One frame of a recent video (cached; from its scrub proxy when there is one). */
  videoThumb: (path: string) => call<{ w: number; h: number; rgb: string; from: "cache" | "proxy" | "file" } | null>("video_thumb", { path }),
  transcript: (project: string, cut: string, owner?: string | null) =>
    call<TranscriptData>("transcript_view", owner ? { cut, owner } : { cut }, project),
  info: () => call<{ version: string; library: string; exports: string; dev_bridge: boolean; autorun?: string }>("app_info"),
  appReady: () => call("app_ready"),
  frameSocket: () => call<{ port: number; token: string; generation?: number }>("app_frame_socket"),
  openProjects: () => call<ProjectInfo[]>("project_list_open"),
  newProject: (name: string) => call<ProjectInfo>("project_new", { name }),
  openHome: (path: string) => call<ProjectInfo>("project_open_home", { path }),
  importCut: (project: string, path: string) => call<{ cut: string; title: string }>("cut_import", { path }, project),
  cutView: (project: string, cut: string) => call<CutView>("cut_view", { cut }, project),
  trim: (project: string, cut: string, part: string, edge: "in" | "out", delta: number) => call("cut_trim", { cut, part, edge, delta }, project),
  /** Ripple-deletes output frames `[from, to)` (the I/O range) as one undo step. */
  rangeDelete: (project: string, cut: string, from: number, to: number) => call<{ removed: number; frames: number }>("cut_range_delete", { cut, from, to }, project),
  split: (project: string, cut: string, frame: number) => call("cut_split", { cut, frame }, project),
  deletePart: (project: string, part: string) => call<Out<"part_delete">>("part_delete", { part } satisfies Args<"part_delete">, project),
  /** Follow the cursor on a part's screen recording (on) or remove it (off): one undo step. */
  followCursor: (project: string, cut: string, part: string, on: boolean, maxZoom: number) =>
    call<{ on: boolean; samples: number; found: number; moves: number; max_zoom: number; ms: number; method?: "input_log" | "pixels"; note?: string | null }>("follow_cursor", { cut, part, on, max_zoom: maxZoom }, project),
  /** Link a part to a cut layout in a framing, or unlink it (`layout` null): one undo step. */
  layoutSet: (project: string, part: string, framing: string, layout: string | null) => call<Out<"layout_set">>("layout_set", { part, framing, layout } satisfies Args<"layout_set">, project),
  /** Merge a part with the next when they play on in the same source (one undo step). */
  partMerge: (project: string, a: string, b: string) => call<Out<"part_merge">>("part_merge", { a, b } satisfies Args<"part_merge">, project),
  partsGroup: (project: string, cut: string, parts: string[]) => call<Out<"parts_group">>("parts_group", { cut, parts } satisfies Args<"parts_group">, project),
  partsUngroup: (project: string, cut: string, parts: string[]) => call<Out<"parts_ungroup">>("parts_ungroup", { cut, parts } satisfies Args<"parts_ungroup">, project),
  /** The human's undo; the first one after a reopen answers `needs_confirm` until
   * `confirm` (R-HIST-1). */
  undo: (project: string, confirm = false) => call<UndoResult>("undo", (confirm ? { confirm } : {}) satisfies Args<"undo">, project),
  historyList: (project: string, cut: string) =>
    call<{ total: number; versions: HistoryVersion[]; now_parts: number | null; now_seconds: number | null }>("history_list", { cut }, project),
  /** How many versions the project has ("↶ N earlier versions"). */
  historyCount: (project: string) => call<{ total: number }>("history_count", {}, project),
  /** A cut at a version against now: parts added, removed, trimmed, moved. */
  historyDiff: (project: string, cut: string, snapshot: string) => call<HistoryDiff>("history_diff", { cut, snapshot }, project),
  historyRestore: (project: string, cut: string, snapshot: string) => call<{ ms?: number }>("history_restore", { cut, snapshot }, project),
  /** "Undo this agent" (decision 0050): every field one agent run wrote, back as it was
   * (one undo step). `Conflict` when later edits touched the same things, with its
   * choices; pass the one picked as `option`. */
  runUndo: (project: string, run: string, option?: Args<"run_undo">["option"]) =>
    call<Out<"run_undo">>("run_undo", { run, ...(option ? { option } : {}) } satisfies Args<"run_undo">, project),
  snapshotNamed: (project: string, name: string) => call<{ id: string }>("snapshot_named", { name }, project),
  redo: (project: string, confirm = false) => call<UndoResult>("redo", (confirm ? { confirm } : {}) satisfies Args<"redo">, project),
  /** `group`: the layers grouped with this one, placed the same (one undo). */
  placePx: (project: string, cut: string, rev: number, part: string, layer: string, x: number, y: number, w: number, group?: { part: string; layer: string }[]) =>
    call("layer_place_px", { cut, rev, part, layer, x, y, w, ...(group ? { group } : {}) }, project),
  cropPx: (project: string, cut: string, rev: number, part: string, layer: string, x: number, y: number, w: number, h: number, group?: { part: string; layer: string }[]) =>
    call("layer_crop_px", { cut, rev, part, layer, x, y, w, h, ...(group ? { group } : {}) }, project),
  layerVisible: (project: string, layer: string, visible: boolean) => call<Out<"layer_set">>("layer_set", { layer, visible } satisfies Args<"layer_set">, project),
  /** A layer's place in the stack (the timeline's layer rows). */
  layerZ: (project: string, layer: string, z: number) => call<Out<"layer_set">>("layer_set", { layer, z } satisfies Args<"layer_set">, project),
  /** An overlay shown (1) or hidden (0) from the timeline's layer rows. */
  overlayOpacity: (project: string, overlay: string, opacity: number) => call<Out<"overlay_set">>("overlay_set", { overlay, opacity } satisfies Args<"overlay_set">, project),
  /** `source` with `part`: the crop editor's picture, that part's own source whole (with
   * `layer`: that layer's own video, a recording's camera file). */
  viewerLoad: (project: string, cut: string, opts: { source?: boolean; keep?: boolean; part?: string; layer?: string } = {}) => call("viewer_load", { cut, ...opts }, project),
  outputSet: (project: string, cut: string, aspect: "9:16" | "16:9") => call("cut_output_set", { cut, aspect }, project),
  layoutPreset: (project: string, cut: string, parts: string[], preset: string) => call("layout_preset", { cut, parts, preset }, project),
  layoutApplyAll: (project: string, cut: string, part: string) => call("layout_apply_all", { cut, part }, project),
  hookLayoutKeep: (project: string, cut: string) => call<{ split_at: number | null; parts: number; note?: string }>("hook_layout_keep", { cut }, project),
  insetAdd: (project: string, cut: string, part: string, crop?: [number, number, number, number], place?: [number, number, number]) =>
    call<{ layer: string }>("inset_add", { cut, part, ...(crop ? { crop } : {}), ...(place ? { place } : {}) }, project),
  insetDelete: (project: string, cut: string, layer: string) => call("inset_delete", { cut, layer }, project),
  layoutCopyNext: (project: string, cut: string, part: string, count: number) => call<{ parts: number }>("layout_copy_next", { cut, part, count }, project),
  /** This part's layout and crops on every other part from the same source video. */
  layoutCopySameSource: (project: string, cut: string, part: string) => call<{ parts: number }>("layout_copy_same_source", { cut, part }, project),
  layoutPresets: (project: string, cut: string) => call<{ profile: string | null; presets: LayoutPresetRow[] }>("layout_presets", { cut }, project),
  layoutPresetSave: (project: string, cut: string, part: string, name: string) => call("layout_preset_save", { cut, part, name }, project),
  layoutPresetApply: (project: string, cut: string, name: string, parts: string[]) => call<{ parts: number }>("layout_preset_apply", { cut, name, parts }, project),
  layoutPresetDelete: (project: string, cut: string, name: string) => call("layout_preset_delete", { cut, name }, project),
  layoutPresetRename: (project: string, name: string, to: string) => call("layout_preset_rename", { name, to }, project),
  cropNorm: (project: string, cut: string, part: string, layer: string, r: [number, number, number, number], gesture: number | null = null, group?: { part: string; layer: string }[]) =>
    call("layer_crop_norm", { cut, part, layer, x: r[0], y: r[1], w: r[2], h: r[3], ...(gesture !== null ? { gesture } : {}), ...(group ? { group } : {}) }, project),
  /** "Developer: all commands" (read only): every registered command with its doc and example. */
  commandsCatalog: () => call<{ commands: CatalogCommand[] }>("commands_catalog"),
  /** What can be dragged on a frame besides the layers: its text overlays and the caption seat (U.12). */
  viewerGizmos: (project: string, cut: string, frame: number) => call<ViewerGizmos>("viewer_gizmos", { cut, frame }, project),
  /** U.12: the paused frame draws the text events of `role` anchored at `at` (every
   *  caption when `at` is null) moved by `(dx, dy)` canvas px: a drag's live preview,
   *  rendered by the shared compositor and never logged. */
  viewerTextDrag: (role: TextRole, at: [number, number] | null, dx: number, dy: number) => call("viewer_text_drag", { role, at, dx, dy }),
  /** The drag ended: `committed` keeps the move drawn until the re-planned cut arrives. */
  viewerTextDragEnd: (committed: boolean) => call("viewer_text_drag_end", { committed }),
  /** Releases a text overlay drag: one overlay_move in the framing shown, ending `gesture`. */
  overlayDrag: (project: string, cut: string, overlay: string, x: number, y: number, gesture: number | null) =>
    call("overlay_drag", { cut, overlay, x, y, ...(gesture !== null ? { gesture } : {}) }, project),
  /** Releases a caption-seat drag: the framing's seat (or one part's), ending `gesture`. */
  captionSeat: (project: string, cut: string, y: number, part: string | null, gesture: number | null) =>
    call("caption_seat", { cut, y, ...(part ? { part } : {}), ...(gesture !== null ? { gesture } : {}) }, project),
  viewerSeek: (frame: number) => call("viewer_seek", { frame }),
  /** The trim monitor (decision 0053): the viewer shows, paused, the frame on `part`'s
   *  `edge` as a trim by `delta` frames would leave it (planned, never committed). */
  viewerTrimPeek: (project: string, cut: string, part: string, edge: "in" | "out", delta: number) =>
    call<{ frame: number }>("viewer_trim_peek", { cut, part, edge, delta }, project),
  /** A part's level (dB per frame) on its own source from `before` frames before its in
   *  point to `after` past its out (`first`: −before): what a trim drag reveals. */
  partEnergy: (project: string, cut: string, part: string, before: number, after: number) =>
    call<{ first: number; db: number[] } | null>("timeline_part_energy", { cut, part, before, after }, project),
  /** The edge drag ended: the viewer shows the cut again, at the frame before the drag. */
  viewerTrimPeekEnd: () => call("viewer_trim_peek_end"),
  /** A scrub step (H.05): the scrub proxy until the scrub rests, then the exact frame. */
  viewerScrubTo: (frame: number) => call("viewer_seek", { frame, scrub: true }),
  /** Timeline thumbnails from the scrub proxy (`gm://data/<project>/filmstrip`, app only). */
  filmstrip: (project: string, cut: string, every: number, w: number) => filmstrip(project, cut, every, w),
  proxyStatus: (project?: string) => call<{ jobs: ProxyJob[] }>("proxy_status", project ? { project } : {}),
  /** Plays output samples [from, to) and stops there. */
  viewerPlayRange: (fromSample: number, toSample: number, looping = false) =>
    call("viewer_play_range", { from_sample: Math.max(0, Math.floor(fromSample)), to_sample: Math.max(0, Math.floor(toSample)), looping }),
  /** Plays source ms [from, to) of the cut (P: one word). */
  viewerPlaySrc: (fromMs: number, toMs: number) => call("viewer_play_src", { from_ms: fromMs, to_ms: toMs }),
  viewerToggle: () => call("viewer_toggle"),
  viewerPlay: () => call("viewer_play"),
  viewerPause: () => call("viewer_pause"),
  viewerStep: (n: number) => call("viewer_step", { n }),
  viewerFull: (on: boolean) => call("viewer_full", { on }),
  viewerStress: (on: boolean) => call("viewer_stress", { on }),
  viewerRate: (rate: number) => call("viewer_rate", { rate }),
  viewerScrub: (on: boolean) => call("viewer_scrub", { on }),
  viewerSelect: (part: string | null, layer: string | null) => call("viewer_select", part && layer ? { part, layer } : {}),
  /** The layers grouped on the timeline: a drag of one on the preview places them all. */
  viewerGroup: (group: { part: string; layer: string }[]) => call("viewer_group", { group }),
  viewerState: () => call<ViewerState>("viewer_state"),
  /** An export of the cut; `preset` (`youtube-1080p`, `youtube-1440p`) plans the 16:9
   * framing at that upload preset instead of its own output (SPEC §6.5.1). */
  exportStart: (project: string, cut: string, draftMattes = false, preset?: string) =>
    call<{ job: string }>("export_start", { cut, ...(draftMattes ? { draft_mattes: true } : {}), ...(preset ? { preset } : {}) }, project),
  // Cut-outs and the activity crop (R.01–R.03)
  /** Decision 0042: an image or video overlay just under the picture layer with z `under` (null: above the picture). */
  overlayStackSet: (project: string, overlay: string, under: number | null) =>
    call<Out<"overlay_stack_set">>("overlay_stack_set", { overlay, under } satisfies Args<"overlay_stack_set">, project),
  /** Decision 0042: cut the person out of a video overlay, or not. */
  overlayMatteSet: (project: string, overlay: string, on: boolean) =>
    call<Out<"matte_set">>("matte_set", { overlay, matte: on ? { roto: {} } : "none" } satisfies Args<"matte_set">, project),
  matteSet: (project: string, layer: string, on: boolean, soften?: number, part?: string, framing?: string) =>
    call<Out<"matte_set">>("matte_set", { layer, matte: on ? { roto: soften === undefined ? {} : { soften } } : "none", ...(part && framing ? { part, framing } : {}) } satisfies Args<"matte_set">, project),
  matteBrush: (project: string, layer: string, part: string, framing: string, strokes: StrokeArg[], clear = false) =>
    call<Out<"matte_brush">>("matte_brush", { layer, part, framing, strokes, clear } satisfies Args<"matte_brush">, project),
  matteStatus: (project: string, cut: string) => call<{ parts: MatteRow[]; build: MatteBuild | null }>("matte_status", { cut }, project),
  matteBuild: (project: string, cut: string) => call<{ build: MatteBuild }>("matte_build", { cut }, project),
  /** The timeline's cut-out ticks (R.01): per matted plan part, output frames with an
   * accurate mask track so far; tracks build in the background on their own. */
  matteProgress: (project: string, cut: string) => call<MatteProgress>("matte_progress", { cut }, project),
  followAction: (project: string, cut: string, part?: string) =>
    call<{ parts: unknown[]; skipped: { part: string; why: string }[]; splits: number; committed?: number | null }>("follow_action", part ? { cut, part } : { cut }, project),
  viewerMatteView: (on: boolean) => call("viewer_matte_view", { on }),
  exportCancel: (job: string) => call("export_cancel", { job }),
  /** End card to the long-form (lane K5): what the card would point at, or why not. */
  endCardLongformView: (project: string, cut: string) =>
    call<{ available: boolean; why?: string; longform?: string; title?: string; longform_title?: string; at_s?: number; has_card: boolean }>("end_card_longform_view", { cut }, project),
  /** The card over the short's last 3 s: one undo step. */
  endCardLongformAdd: (project: string, cut: string) => call<{ ids: string[]; title: string; picture: boolean }>("end_card_longform_add_app", { cut }, project),
  /** The card off: one undo step. */
  endCardLongformRemove: (project: string, cut: string) => call<{ removed: number }>("end_card_longform_remove", { cut }, project),
  /** The cut's 9:16 and 16:9 framings with each one's export, and the shapes it lacks. */
  exportFramings: (project: string, cut: string) => call<FramingsView>("export_framings", { cut }, project),
  /** The missing shape, made from the layout defaults (the cut keeps showing its own). */
  framingMake: (project: string, cut: string, aspect: "9:16" | "16:9") => call<{ framing: string; made: boolean }>("framing_make", { cut, aspect }, project),
  /** Both framings exported: two jobs, 9:16 first (`make_missing` makes a missing one first). */
  exportBoth: (project: string, cut: string, makeMissing: boolean) =>
    call<{ jobs: { aspect: string; framing: string; name: string; job: string }[]; made: string[] }>("export_both", { cut, make_missing: makeMissing }, project),
  /** H.09: what VideoToolbox offers on this Mac (asked once at launch). */
  capsStartup: () => call<StartupCaps>("caps_startup"),
  /** H.06: re-verify an export file (R-EXP-6); the result is recorded on the export. */
  exportVerify: (project: string, exportId: string) => call<ExportVerify>("export_verify", { export: exportId }, project),
  jobs: () => call<JobView[]>("jobs_list"),
  exports: (project: string) => call<ExportEntry[]>("exports_list", {}, project),
  reveal: (project: string, exportId: string) => call("export_reveal", { export: exportId }, project),
  /** The macOS share sheet (AirDrop, Messages, Mail, Save to Photos) at a window point. */
  share: (project: string, exportId: string, x: number, y: number) => call("export_share", { export: exportId, x, y }, project),
  /** Drags the exported file out of the window (to the Finder, Mail, a chat). */
  dragOut: (project: string, exportId: string) => call("export_drag", { export: exportId }, project),
  board: (project: string) => call<{ tiles: BoardTile[]; clippy_paired: boolean }>("board_view", { project }),
  /** Undo one judgement call of the cut's last run (one transaction). */
  runChoiceRestore: (project: string, cut: string, choice: number) =>
    call<Out<"run_choice_restore">>("run_choice_restore", { cut, choice } satisfies Args<"run_choice_restore">, project),
  clippyPair: () => call<{ paired: boolean }>("clippy_pair"),
  activity: () => call<Activity>("activity"),
  firstSeenAck: (ts: number) => call<{ acked: number }>("first_seen_ack", { ts }),
  agentsPause: (on: boolean) => call<{ paused: boolean }>("agents_pause", { on }),
  agentClientAllow: (sha256: string) => call<{ result: string }>("agent_client_allow", { sha256 }),
  agentRunStop: (run: string) => call("agent_run_stop", { run }),
  home: () => call<Home>("home"),
  homeSeen: () => call("home_seen"),
  s2aReport: (report: unknown) => call("app_s2a_report", { report }),
  cutWords: (project: string, cut: string) =>
    call<{ state: string; revision?: string; words: CutWord[]; note?: string; respell?: boolean; respell_why?: string }>("cut_words", { cut }, project),
  strikeWords: (project: string, cut: string, from: number, to: number, owner?: string | null) =>
    call<{ output?: Struck }>("words_strike_ord", owner ? { cut, from, to, owner } : { cut, from, to }, project),
  restoreWords: (project: string, cut: string, from: number, to: number, owner?: string | null) =>
    call("words_restore_ord", owner ? { cut, from, to, owner } : { cut, from, to }, project),
  respellWord: (project: string, cut: string, word: number, text: string | null, owner?: string | null) =>
    call("word_respell", owner ? { cut, word, text, owner } : { cut, word, text }, project),
  edgeReset: (project: string, cut: string, word: number, owner?: string | null) =>
    call("word_edge_reset", owner ? { cut, word, owner } : { cut, word }, project),
  addAsPart: (project: string, cut: string, from: number, to: number, owner?: string | null) =>
    call("words_add_part", owner ? { cut, from, to, owner } : { cut, from, to }, project),
  stretchLast: (project: string, cut: string, to: number, owner?: string | null) =>
    call("words_stretch_last", owner ? { cut, to, owner } : { cut, to }, project),
  nudgeWord: (project: string, cut: string, word: number, edge: "start" | "end", deltaMs: number, owner?: string | null) =>
    call("word_nudge_ord", owner ? { cut, word, edge, delta_ms: deltaMs, owner } : { cut, word, edge, delta_ms: deltaMs }, project),
  respell: (project: string, cut: string, revision: string, ordinal: number, text: string | null) =>
    call<Out<"caption_edit">>("caption_edit", { cut, word: { asr_revision: revision, ordinal }, edit: text === null ? null : text === "" ? "drop" : { text } } satisfies Args<"caption_edit">, project),
  keyword: (project: string, cut: string, revision: string, ordinal: number) =>
    call<Out<"caption_keyword_toggle">>("caption_keyword_toggle", { cut, word: { asr_revision: revision, ordinal } } satisfies Args<"caption_keyword_toggle">, project),
  story: (project: string, cut: string) => call<StoryView>("story_view", { cut }, project),
  briefSet: (project: string, cut: string, brief: string) => call<Out<"story_brief_set">>("story_brief_set", { cut, brief } satisfies Args<"story_brief_set">, project),
  /** "Use the proposed cut" (all of the arc), or ＋ one suggestion: one undo step. */
  storyApply: (project: string, cut: string, only?: string[]) =>
    call<Out<"story_apply">>("story_apply", (only ? { cut, only } : { cut }) satisfies Args<"story_apply">, project),
  suggestionState: (project: string, suggestion: string, state: "suggested" | "rejected") =>
    call<Out<"story_suggestion_state">>("story_suggestion_state", { suggestion, state } satisfies Args<"story_suggestion_state">, project),
  partTime: (project: string, cut: string, part: string, edge: "in" | "out", text: string) => call("part_time_set", { cut, part, edge, text }, project),
  partNudge: (project: string, cut: string, part: string, edge: "in" | "out", deltaMs: number) =>
    call("part_nudge_ms", { cut, part, edge, delta_ms: deltaMs }, project),
  partMove: (project: string, cut: string, part: string, dir: "up" | "down") => call("part_move", { cut, part, dir }, project),
  chronological: (project: string, cut: string) => call("cut_chronological", { cut }, project),
  moments: (project: string) => call<{ moments: MomentRow[]; transcript: boolean; videos?: { owner: string; name: string; density?: Density | null }[] }>("moments_list", {}, project),
  // Decision 0011: the stream edit, topics, long-forms, framings, the project map.
  streamOverlay: (project: string, cut: string) => call<StreamOverlay>("stream_overlay", { cut }, project),
  projectMap: (project: string) => call<ProjectMapData>("project_map_view", {}, project),
  streamEditEnsure: (project: string, adopt?: string, media?: MediaRef) =>
    call<Out<"stream_edit_ensure">>("stream_edit_ensure", (adopt ? { adopt } : media ? { media } : {}) satisfies Args<"stream_edit_ensure">, project),
  topicLabel: (project: string, cut: string, from: number, to: number, title: string, owner?: string) =>
    call<{ topic: string; trimmed: string[]; removed: string[]; split_off: string[] }>("topic_label_ord", owner ? { cut, from, to, title, owner } : { cut, from, to, title }, project),
  topicEdge: (project: string, cut: string, topic: string, edge: "start" | "end", word: number) =>
    call("topic_edge_ord", { cut, topic, edge, word }, project),
  topicSplit: (project: string, cut: string, topic: string, at: number) => call("topic_split_ord", { cut, topic, at }, project),
  topicState: (project: string, topic: string, state: "accepted" | "rejected") => call<Out<"topic_state">>("topic_state", { topic, state } satisfies Args<"topic_state">, project),
  topicDelete: (project: string, topic: string) => call<Out<"topic_delete">>("topic_delete", { topic } satisfies Args<"topic_delete">, project),
  fluffState: (project: string, proposals: string[], state: "accepted" | "rejected") =>
    call<Out<"fluff_state">>("fluff_state", { proposals, state } satisfies Args<"fluff_state">, project),
  longformFromTopics: (project: string, topics: string[], order: "source" | "given", title: string) =>
    call<Out<"longform_from_topics">>("longform_from_topics", { topics, order, ...(title.trim() ? { title } : {}) } satisfies Args<"longform_from_topics">, project),
  parentDiff: (project: string, cut: string) => call<ParentDiff>("cut_parent_diff", { cut } satisfies Args<"cut_parent_diff">, project),
  pullFromParent: (project: string, cut: string) => call<Out<"cut_pull_from_parent">>("cut_pull_from_parent", { cut } satisfies Args<"cut_pull_from_parent">, project),
  framingAdd: (project: string, cut: string, name: string, aspect: "9:16" | "16:9" | "1:1" | "4:5") =>
    call<Out<"framing_add">>("framing_add", { cut, name, aspect } satisfies Args<"framing_add">, project),
  framingPick: (project: string, cut: string, framing: string) => call("framing_pick", { cut, framing }, project),
  // Decision 0015 (lane V): comments, suggested edits, review-link asks, golden cuts, thumbnails.
  comments: (project: string, cut: string) => call<CommentsView>("comments_view", { cut }, project),
  commentAdd: (project: string, cut: string, text: string, frame: number | null, box: FrameBox | null) =>
    call<{ output: { id: string } }>("comment_add_at", { cut, text, ...(frame !== null ? { frame } : {}), ...(box ? { box } : {}) }, project),
  commentReply: (project: string, cut: string, replyTo: string, text: string) => call("comment_add_at", { cut, text, reply_to: replyTo }, project),
  commentResolve: (project: string, comment: string, resolved: boolean) => call<Out<"comment_resolve">>("comment_resolve", { comment, resolved } satisfies Args<"comment_resolve">, project),
  commentDelete: (project: string, comment: string) => call<Out<"comment_delete">>("comment_delete", { comment } satisfies Args<"comment_delete">, project),
  commentsFix: (project: string, cut: string, comments: string[] | null) => call<AgentRun>("comments_fix", { cut, ...(comments ? { comments } : {}) }, project),
  proposalsDecide: (project: string, proposals: string[], state: "accepted" | "rejected") =>
    call<Out<"edit_proposal_state">>("edit_proposal_state", { proposals, state } satisfies Args<"edit_proposal_state">, project),
  reviewAsk: (ask: string, start: boolean) => call("review_ask", { ask, start }),
  reviewLinkRole: (role: "reviewer" | "editor") => call<{ id: string; url: string; expires_ms: number; role: string }>("review_link_role", { role }),
  golden: (project: string, cut: string) => call<GoldenView>("golden_view", { cut }, project),
  goldenMark: (project: string, cut: string, on: boolean) => call<{ golden: boolean; count: number }>("golden_mark", { cut, on }, project),
  goldenCompare: (project: string, cut: string) => call<GoldenCompare>("golden_compare", { cut }, project),
  thumbs: (project: string, cut: string) => call<ThumbsView>("thumbs_view", { cut }, project),
  thumbAdd: (project: string, cut: string, frame: number, title: string, pick: boolean) => call("thumb_add_at", { cut, frame, title, pick }, project),
  thumbCover: (project: string, cut: string, thumbnail: string, frame: number) => call("thumb_cover", { cut, thumbnail, frame }, project),
  thumbSet: (project: string, thumbnail: string, patch: { pick?: boolean; title?: string }) => call<Out<"thumbnail_set">>("thumbnail_set", { thumbnail, ...patch } satisfies Args<"thumbnail_set">, project),
  thumbRender: (project: string, thumbnail: string) =>
    call<{ path: string; width: number; height: number; preview_png: string; frame: number }>("thumb_render", { thumbnail }, project),
  thumbAgent: (project: string, cut: string, frame: number | null) => call<AgentRun>("thumb_agent", { cut, ...(frame !== null ? { frame } : {}) }, project),
};

// ---- Library (lane L): search everything said, meaning, chat logs, tap marks, templates,
// consolidate. Its own object so lanes do not collide in `actions`.

/** One library search hit (L.01): word-edged, on its project's media clock. */
export interface LibraryHit {
  project: string;
  project_name: string;
  home: string;
  open: boolean;
  media: { source?: string; group?: string };
  media_name: string;
  revision: string;
  w0: number;
  w1: number;
  start_s: number;
  end_s: number;
  text: string;
  before: string;
  after: string;
  topic: { title: string; state: string } | null;
  score: number;
  by: "words" | "meaning" | "both";
}

export interface LibrarySearchResult {
  query: string;
  hits: LibraryHit[];
  /** Topics whose titles match (K.04), with the long-form that plays each. */
  topics?: LibraryTopic[];
  /** The meaning index's state (null for words-only searches). */
  meaning: { state: string; windows?: number; pending?: number; model?: string; error?: string } | null;
  index: { projects: number; looked_at: number; transcripts_indexed: number; lines_indexed: number; ms: number };
  query_ms: number;
}

const hitArgs = (h: LibraryHit) => ({ project: h.project, home: h.home, media: h.media, start_s: h.start_s, end_s: h.end_s });

export const libraryActions = {
  search: (query: string, mode: "words" | "meaning" | "hybrid", limit = 30, only?: string) =>
    call<LibrarySearchResult>("library_search", only ? { query, mode, limit, only } : { query, mode, limit }),
  /** ▶ the hit's span in the viewer (its project opened when closed). */
  play: (h: LibraryHit) => call<{ project: string; start_s: number; end_s: number }>("library_play", hitArgs(h)),
  /** The cut that shows the hit, and its frame there. */
  open: (h: LibraryHit) => call<{ project: string; cut: string; frame: number | null }>("library_open", hitArgs(h)),
  /** ＋ Make moment: a word-edged moment by you (origin manual). */
  moment: (h: LibraryHit, note: string) =>
    call<{ project: string; moment: { id: string; start_s: number; end_s: number; words: number; quote: string } }>("library_moment", { ...hitArgs(h), note }),
};


/** Tap marking (L.04): the in and out frames shown, in a cut or a moment's review. */
export const tapActions = {
  mark: (project: string, at: { cut: string } | { moment: string }, inFrame: number, outFrame: number) =>
    call<{ moment: { id: string; start_s: number; end_s: number; words: number; quote: string }; tapped: [number, number] }>(
      "tap_mark",
      { ...at, in_frame: inFrame, out_frame: outFrame },
      project,
    ),
};

/** A saved look (L.05): content only. */
export interface CutTemplateRow {
  name: string;
  framings: string[];
  hook: boolean;
  pills: number;
  captions: boolean;
}

export const cutLikeActions = {
  /** A new cut on `moment` with the look of `cut` (or of a saved template). */
  make: (project: string, from: { cut: string } | { template: string }, moment: string, saveAs?: string) =>
    call<{ cut: string; framings: string[]; seconds: number; styles_renamed: Record<string, string>; pills: number; hook: boolean }>(
      "cut_like",
      { ...from, moment, ...(saveAs?.trim() ? { save_as: saveAs.trim() } : {}) },
      project,
    ),
  save: (project: string, cut: string, name: string) => call<{ name: string }>("cut_template_save", { cut, name }, project),
  templates: () => call<{ templates: CutTemplateRow[] }>("cut_templates"),
  remove: (name: string) => call<{ deleted: string }>("cut_template_delete", { name }),
};

export const consolidateActions = {
  open: (path: string) => call<{ id: string }>("project_open_copy", { path }),
};
// ---- decision 0015 (lane V) ---------------------------------------------------------

/** A box on the frame, fractions of the output canvas. */
export interface FrameBox {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface CommentThread {
  id: string;
  /** Output frame on the cut as it plays now; null when it is cut out (or not pinned). */
  frame: number | null;
  t: number | null;
  pinned: boolean;
  box: FrameBox | null;
  author: string;
  human: boolean;
  text: string;
  resolved: boolean;
  at_ms: number;
  export_ref: string | null;
  replies: { id: string; author: string; human: boolean; text: string; at_ms: number }[];
}

export interface Suggestion {
  id: string;
  kind: "trim" | "strike" | "caption_move";
  summary: string;
  frame: number | null;
  author: string;
  note: string;
  at_ms: number;
  state: "proposed" | "accepted" | "rejected";
  comment: string | null;
}

export interface CommentsView {
  threads: CommentThread[];
  suggestions: Suggestion[];
  open: number;
  fps: number;
  agent_runs: { run: string; comments: string[] }[];
}

export interface GoldenView {
  golden: boolean;
  goldens: { project: string; project_name: string; cut: string; title: string; marked_ms: number; line: string }[];
}

export interface GoldenCompare {
  mine_line: string;
  goldens: { project_name: string; cut: string; title: string; line: string }[];
  rows: { key: string; label: string; mine: number | null; band: [number, number] | null; status: "in" | "below" | "above" | "missing" | "n/a"; goldens: (number | null)[] }[];
  outside: string[];
  loops_expected: boolean;
  canvas_matches: boolean;
}

export interface ThumbStyle {
  font: string;
  size: number;
  fill: string;
  outline: string;
  accent: string;
  uppercase: boolean;
}

export interface ThumbVersion {
  id: string;
  title: string;
  hook_options: string[];
  style: ThumbStyle;
  layout: { title_box: FrameBox; align: "left" | "centre" | "right" };
  origin: "human" | "agent";
  author: string;
  human: boolean;
  picked: boolean;
  notes: string;
  at_ms: number;
  frame: number | null;
  rendered: boolean;
}

export interface ThumbsView {
  versions: ThumbVersion[];
  text_hook: string | null;
  brand: boolean;
  style: ThumbStyle;
  layout: { title_box: FrameBox; align: "left" | "centre" | "right" };
  canvas: [number, number] | null;
}

// ---- Packaging (decision 0024): three openings, one short; hook lines on measured
// numbers; title/description drafts per platform; a long-form's chapters. Its own object
// so lanes do not collide in `actions`.

/** One version of a short with what its exports carry (`openings_view`). */
export type OpeningVersion = Out<"openings_list">["versions"][number] & {
  exports: number;
  last_export: string | null;
  retention: { export: string; platform: string; source: string; at_3s: number; at_opening_end: number; at_end: number; duration_s: number } | null;
};
export type OpeningsView = Omit<Out<"openings_list">, "versions"> & { versions: OpeningVersion[] };
export type HookLine = Out<"hook_text_suggest">["suggestions"][number];
export type CopyDraftRow = Out<"copy_drafts_list">["drafts"][number];
export type CopyPlatformName = CopyDraftRow["platform"];

export const packagingActions = {
  openings: (project: string, cut: string) => call<OpeningsView>("openings_view", { cut }, project),
  openingFromMoment: (project: string, cut: string, moment: string) =>
    call<Out<"opening_add">>("opening_add", { cut, from: { moment } } satisfies Args<"opening_add">, project),
  openingFromSuggestion: (project: string, cut: string, suggestion: string) =>
    call<Out<"opening_add">>("opening_add", { cut, from: { suggestion } } satisfies Args<"opening_add">, project),
  /** Words selected in the transcript (ordinals, inclusive). */
  openingFromWords: (project: string, cut: string, from: number, to: number) => call<{ id: string }>("opening_add_words", { cut, from, to }, project),
  openingSet: (project: string, opening: string, patch: { label?: string; hook_text?: string }) =>
    call<Out<"opening_set">>("opening_set", { opening, ...patch } satisfies Args<"opening_set">, project),
  openingDelete: (project: string, opening: string) => call<Out<"opening_delete">>("opening_delete", { opening } satisfies Args<"opening_delete">, project),
  openingChoose: (project: string, opening: string) => call<Out<"opening_choose">>("opening_choose", { opening } satisfies Args<"opening_choose">, project),
  /** A version in the viewer (none: the cut itself). */
  openingPreview: (project: string, cut: string, opening: string | null) =>
    call<{ frames: number; fps: number }>("opening_preview", { cut, ...(opening ? { opening } : {}) }, project),
  openingExport: (project: string, cut: string, opening: string) => call<{ job: string }>("opening_export", { cut, opening }, project),
  hookLines: (project: string, cut: string) => call<Out<"hook_text_suggest">>("hook_text_suggest", { cut } satisfies Args<"hook_text_suggest">, project),
  /** A suggested line as the text hook, with where its numbers come from (R-AI-4). */
  hookUse: (project: string, cut: string, line: HookLine, until: number | null, durMs: number | null) =>
    call("hook_set", { cut, text: line.text, provenance: line.provenance, ...(until !== null ? { until } : {}), ...(durMs !== null ? { dur_ms: durMs } : {}) }, project),
  drafts: (project: string, cut: string) => call<Out<"copy_drafts_list">>("copy_drafts_list", { cut } satisfies Args<"copy_drafts_list">, project),
  draftDecide: (project: string, draft: string, patch: { state?: "accepted" | "rejected" | "proposed"; title?: string; description?: string }) =>
    call<Out<"copy_draft_state">>("copy_draft_state", { draft, ...patch } satisfies Args<"copy_draft_state">, project),
  draftWithAgent: (project: string, cut: string) => call<AgentRun>("agent_run", { cut, pass: "publish_copy", brief: "", media: false }, project),
  chapters: (project: string, cut: string) => call<Out<"longform_chapters">>("longform_chapters", { cut } satisfies Args<"longform_chapters">, project),
};

// ---- Ideas 2026-10-16 (library and layout, lane K4). Its own object, like the library's.

/** A topic as a library search result (K.04). */
export interface LibraryTopic {
  project: string;
  project_name: string;
  home: string;
  open: boolean;
  media: { source?: string; group?: string };
  media_name: string;
  topic: string;
  title: string;
  state: "accepted" | "proposed";
  start_s: number;
  end_s: number;
  long: { cut: string; title: string; at_s: number } | null;
  by: "words" | "meaning";
}

/** One box of a stream layout, normalised to the picture. */
export interface LayoutBox {
  role: string;
  rect: { x: number; y: number; w: number; h: number };
}

/** "Draw your layout once" for one video (K.01). */
export interface LayoutDrawView {
  source: string;
  file: string;
  canvas: { w: number; h: number };
  fps: number;
  duration_s: number;
  name: string;
  has_layout: boolean;
  regions: LayoutBox[];
  roles: { role: string; label: string; private: boolean }[];
  presets: { name: string; regions: LayoutBox[] }[];
  siblings: { source: string; name: string; layout: string | null }[];
}

export interface RgbFrame {
  w: number;
  h: number;
  rgb: string;
  at_s?: number;
}

export interface ComparedChapter {
  topic: string;
  title: string;
  colour: string;
  at_ms: number;
  dur_ms: number;
  parts: number;
  frame: RgbFrame | null;
}

export interface ComparedLong {
  cut: string;
  title: string;
  brief: string;
  duration_ms: number;
  parts: number;
  chapters: ComparedChapter[];
}

export interface LongformComparison {
  a: ComparedLong;
  b: ComparedLong;
  same_topics: boolean;
  same_order: boolean;
  moved: string[];
  only_a: string[];
  only_b: string[];
}

/** A B-roll suggestion: a library hit plus how one click adds it (K.05). */
export type BrollSuggestion = LibraryHit & { add_as: "part" | "image"; also?: "clip"; note: string };

export const ideaActions = {
  /** K.01: the video's picture, its layout (if any), presets of its size, its siblings. */
  layoutDrawView: (project: string, source: string) => call<LayoutDrawView>("layout_draw_view", { source }, project),
  /** A frame to draw on (`at_s` absent: a tenth in). */
  layoutDrawFrame: (project: string, source: string, at_s?: number) =>
    call<RgbFrame>("layout_draw_frame", at_s === undefined ? { source } : { source, at_s }, project),
  layoutDrawSave: (project: string, source: string, name: string, regions: LayoutBox[], also: string[], preset: boolean) =>
    call<{ name: string; videos: number; preset: boolean; private: number; camera: boolean }>("layout_draw_save", { source, name, regions, also, preset }, project),
  /** K.02: caption words `from..=to` (1–4) spelt `written` everywhere (and in all projects). */
  termFixEverywhere: (project: string, cut: string, from: number, to: number, written: string, profile: boolean, owner?: string) =>
    call<{ found: number; variant_added: boolean; heard: string; written: string; profile: boolean }>("term_fix_everywhere", owner ? { cut, from, to, written, profile, owner } : { cut, from, to, written, profile }, project),
  terms: (project: string) => call<{ terms: { written: string; heard_as: string[]; project: boolean; profile: boolean }[] }>("terms_view", {}, project),
  /** K.03: a sibling long-form, in the same order or the topics' new order. */
  longformFork: (project: string, cut: string, topics?: string[], title?: string) =>
    call<Out<"longform_fork">>("longform_fork", { cut, ...(topics ? { topics } : {}), ...(title ? { title } : {}) } satisfies Args<"longform_fork">, project),
  longformCompare: (project: string, a: string, b: string) => call<LongformComparison>("longform_compare_view", { a, b }, project),
  longformPick: (project: string, keep: string, drop: string) => call<Out<"longform_pick">>("longform_pick", { keep, drop } satisfies Args<"longform_pick">, project),
  /** K.04: open a topic result where it is used. */
  topicOpen: (t: LibraryTopic) =>
    call<{ project: string; cut: string; frame: number | null; in: "long" | "stream"; title?: string }>("library_topic_open", { project: t.project, home: t.home, topic: t.topic }),
  /** K.05: moments from other recordings that talk about the selected words. */
  brollSuggest: (project: string, cut: string, from: number, to: number) =>
    call<{ suggestions: BrollSuggestion[]; meaning: { state: string; pending?: number; error?: string } | null; words: number }>("broll_suggest", { cut, from, to }, project),
  /** `asClip`: the moment's own video, silent, picture-in-picture over the words (decision 0028). */
  brollAdd: (project: string, cut: string, from: number, to: number, hit: BrollSuggestion, asClip = false) =>
    call<{ added: "part" | "image" | "clip"; seconds: number }>("broll_add", { cut, from, to, hit: { project: hit.project, home: hit.home, media: hit.media, start_s: hit.start_s, end_s: hit.end_s }, ...(asClip ? { as: "clip" } : {}) }, project),
};

// ---- Edit polish (Ideas 2026-10-16, lane K2; decision 0025) ------------------------------

/** A caption word the speaker stressed (louder or slower than his running baseline here). */
export interface StressedWord {
  key: string;
  text: string;
  frame: number;
  at: string;
  label: string;
  z_level: number;
  z_slow: number;
  db_over: number;
  pace_ratio: number;
  keyword: boolean;
}
export interface PunchSuggestion {
  key: string;
  text: string;
  frame: number;
  at: string;
  label: string;
  added: string | null;
  why_not: string | null;
}
export interface CalloutSuggestion {
  key: string;
  kind: "label" | "arrow";
  what: string;
  text: string;
  frame: number;
  at: string;
  label: string;
  added: boolean;
  seconds: number;
}
export interface PolishView {
  framing: string;
  auto_applied: number;
  emphasis: { state: "ok" | "no_transcript" | "few_words" | "no_sound"; words: number; budget: number; auto: boolean; proposals: StressedWord[]; measured: string };
  punch: { on: boolean; count: number; existing: string[]; suggestions: PunchSuggestion[]; rule: string };
  callouts: CalloutSuggestion[];
  loop: { on: boolean | null; state: "ok" | "empty"; matches?: boolean; differences?: string[]; fixes?: { key: "layout" | "tail"; label: string }[]; measured?: string };
}
export interface HoldSuggestion {
  key: string;
  frame: number;
  change_frame: number;
  at: string;
  until: string;
  until_word: string;
  fraction: number;
  z: number;
  label: string;
}
export type PolishKind = "emphasis" | "punch" | "callout" | "loop";

export const polishActions = {
  /** Stressed words, punch-ins, callouts and the loop for a cut (stressed words are starred on their own when the caption style asks). */
  view: (project: string, cut: string) => call<PolishView>("polish_view", { cut }, project),
  /** One click on a suggestion (worked out again by the app), one undo step. */
  apply: (project: string, cut: string, kind: PolishKind, key: string) =>
    call<{ kind: PolishKind; key: string; output: Record<string, unknown> }>("polish_apply", { cut, kind, key }, project),
  /** Where the screen jumps to something else mid-sentence (reads frames). */
  holdsFind: (project: string, cut: string) =>
    call<{ samples: number; steps: number; holds: HoldSuggestion[]; skipped: unknown[]; measured: string }>("holds_find", { cut }, project),
};

// ---------------------------------------------------------------------------------
// Agent sessions over ACP (decision 0035): live sessions of the user's own agents
// ---------------------------------------------------------------------------------

export type { Entry as SessionEntry, SessionView, Target as SessionTarget } from "./gen/events";
type SessionViewT = import("./gen/events").SessionView;
type SessionEntryT = import("./gen/events").Entry;

/** One agent and how it speaks ACP on this machine. */
export interface SessionAgent {
  harness: AgentClient["harness"];
  label: string;
  available: boolean;
  via: string | null;
  problem: string | null;
  /** The adapter can switch the agent's own tools and settings off (gmclip access enforced). */
  contained: boolean;
  /** The agent can take gmclip's MCP tools. */
  gmclip_tools: boolean;
  /** The models it offered in its last session here (an agent lists them only once open). */
  models: { id: string; name: string }[];
}

/** gmclip's tools only; the agent's own tools too, each asking first; or without asking. */
export type SessionAccess = "gmclip" | "full" | "bypass";

/** What a session works on: nothing (the library), a project, one video or one cut. */
export interface SessionTargetArgs {
  project?: string;
  source?: string;
  cut?: string;
}

export const sessionActions = {
  agents: () => call<{ agents: SessionAgent[]; default: AgentClient["harness"] }>("session_agents"),
  list: () => call<{ sessions: SessionViewT[]; live: number }>("session_list"),
  get: (session: string) => call<{ session: SessionViewT; entries: SessionEntryT[] }>("session_get", { session }),
  /** A human click (R-AGT-3): starts the agent on the target with the first message. */
  start: (target: SessionTargetArgs, harness: AgentClient["harness"], access: SessionAccess, message: string, media = false, folder?: string, model?: string) =>
    call<{ session: SessionViewT }>("session_start", { target, harness, access, message, media, ...(folder ? { folder } : {}), ...(model ? { model } : {}) }),
  prompt: (session: string, message: string) => call("session_prompt", { session, message }),
  cancel: (session: string) => call("session_cancel", { session }),
  stop: (session: string) => call<{ session: SessionViewT }>("session_stop", { session }),
  answer: (session: string, permission: string, option: string) => call("session_answer", { session, permission, option }),
  mode: (session: string, mode: string) => call("session_mode", { session, mode }),
  model: (session: string, model: string) => call("session_model", { session, model }),
  /** Opens an ended session again; `message`, if any, is its next turn. */
  resume: (session: string, message?: string) => call<{ session: SessionViewT }>("session_resume", { session, message }),
  forget: (session: string) => call("session_forget", { session }),
  /** Undoes every edit the session made (a session on one cut). */
  undo: (session: string) => call<{ undone: number }>("session_undo", { session }),
};

// ---------------------------------------------------------------------------------
// Recording (decision 0045, SPEC §5.18): Settings › Recording, the Record sheet, the
// recording bar, opening a session folder, and a recorded screen's cursor
// ---------------------------------------------------------------------------------

export type RecPermKind = "screen" | "camera" | "microphone" | "accessibility" | "input";
export interface RecPermission {
  state: "granted" | "denied" | "not_asked";
  /** The System Settings pane (an `x-apple.systempreferences:` URL). */
  settings: string;
}
/** The recording running now: its folder, time, markers, what is missing, and whether
 * ⌃⌥S asked it to stop; `trouble` when a stream stopped. */
export interface RecRunning {
  dir: string;
  started: boolean;
  elapsed_s: number;
  markers: number;
  missing: string[];
  stop_asked: boolean;
  trouble: string | null;
  /** Encoder overload; includes video and audio samples. */
  dropped?: number;
}
export interface RecStatus {
  enabled: boolean;
  /** Off macOS: no recording at all. */
  unsupported?: boolean;
  permissions?: Record<RecPermKind, RecPermission>;
  /** Running inside gmclip.app; a bare binary's camera and mic are asked for its launcher. */
  bundled?: boolean;
  recording: RecRunning | null;
  /** Where recordings go unless the sheet says otherwise. */
  folder: string;
  /** The same with the home folder as ~ (to show). */
  folder_short?: string;
}
export interface RecSources {
  displays: { id: number; name: string; main: boolean; width: number; height: number }[];
  windows: { id: number; app: string; title: string }[];
  cameras: { id: string; name: string }[];
  mics: { id: string; name: string }[];
  /** Live encoders running (OBS and the like): the recording is streaming-safe. */
  live?: string[];
}
export interface RecOptions {
  displays?: number[];
  window?: number;
  face?: boolean;
  camera?: string;
  mic?: boolean;
  mic_id?: string;
  system_audio?: boolean;
  fps?: 30 | 60;
  folder?: string;
  name?: string;
}
/** A stopped or opened session: `videos_open`'s result plus what the session lacks. */
export interface RecOpened {
  project: string;
  cut: string | null;
  added: Added["added"];
  name?: string;
  reopened?: boolean;
  notes: string[];
  session: string;
  cancelled?: undefined;
}

export const recordingActions = {
  status: () => call<RecStatus>("recording_status"),
  /** The recordings folder in Finder (made first if missing). */
  reveal: () => call<{ folder: string }>("recording_reveal"),
  /** On asks the OS for all five permissions in order (camera and mic sheets wait). */
  enable: (on: boolean) => call<RecStatus>("recording_enable", { on }),
  /** The Record sheet's camera preview on (with `camera`, a unique id) or off; its
   * pictures are `gm://camera/frame?after=<seq>`. */
  preview: (on: boolean, camera?: string) => call<{ on: boolean }>("recording_preview", { on, ...(camera ? { camera } : {}) }),
  /** The newest preview picture after `seq`: `{w u32, h u32, seq u32}` (LE) then RGBA. */
  previewFrame: (seq: number) => gmData(`gm://camera/frame?after=${seq}`),
  /** Asks for one permission (Settings asks one at a time, in order); `settings` opens its
   * System Settings pane instead (one refused, or asked before). */
  ask: (kind: RecPermKind, settings = false) => call<RecStatus>("recording_ask", { kind, settings }),
  sources: () => call<RecSources>("recording_sources"),
  /** Starts every stream warm; the countdown runs meanwhile. */
  prepare: (o: RecOptions) => call<{ dir: string; missing: string[] }>("recording_prepare", { ...o }),
  go: () => call<{ dir: string }>("recording_go"),
  marker: () => call<{ markers: number }>("recording_marker"),
  /** Closes the files and opens the session as a project (before go: cancelled). */
  stop: () => call<RecOpened | { cancelled: true }>("recording_stop"),
  cancel: () => call<{ cancelled: boolean }>("recording_cancel"),
  open: (dir: string) => call<RecOpened>("recording_open", { dir }),
  /** The project's videos, with each recorded screen's cursor by source id. */
  sourceList: (project: string) => call<Out<"source_list">>("source_list", {} satisfies Args<"source_list">, project),
  /** How a recorded screen's cursor is drawn (one undo step). */
  cursorSet: (project: string, a: Args<"source_cursor_set">) => call<Out<"source_cursor_set">>("source_cursor_set", a satisfies Args<"source_cursor_set">, project),
};

// ---------------------------------------------------------------------------------
// Screen Studio controls (decision 0046): zoom and layout sections on lanes above the
// parts, a layer's frame and padding, the camera bubble's built-in layouts, and the
// one-click look
// ---------------------------------------------------------------------------------

export type { SectionKind, SectionView, ZoomSpec, ZoomTarget, LayerFrame, BubbleShape, BuiltinLayoutRow, CutMarker } from "./gen/commands";

export const studioActions = {
  /** The cut's sections in cut ms (unplaced ones last, start/end null). */
  sectionList: (project: string, cut: string) => call<Out<"section_list">>("section_list", { cut } satisfies Args<"section_list">, project),
  /** The recording markers the cut plays, in cut ms (decision 0052). */
  markerList: (project: string, cut: string) => call<Out<"marker_list">>("marker_list", { cut } satisfies Args<"marker_list">, project),
  /** One undo step; framing absent applies it to every framing. */
  sectionAdd: (project: string, a: Args<"section_add">) => call<Out<"section_add">>("section_add", a satisfies Args<"section_add">, project),
  /** An edge alone moves that edge; `kind` replaces the whole kind. */
  sectionSet: (project: string, a: Args<"section_set">) => call<Out<"section_set">>("section_set", a satisfies Args<"section_set">, project),
  sectionRemove: (project: string, section: string) => call<Out<"section_remove">>("section_remove", { section } satisfies Args<"section_remove">, project),
  /** Zoom sections from the input log, replacing the earlier automatic ones (the app runs
   * the holder's planner: `zoom_auto` is holder-answered). */
  zoomAuto: (project: string, cut: string, maxZoom?: number) =>
    call<{ sections: number; parts_read: number; parts_skipped: number }>("zoom_auto_app", { cut, ...(maxZoom ? { max_zoom: maxZoom } : {}) }, project),
  layoutBuiltinList: (project: string, cut: string) => call<Out<"layout_builtin_list">>("layout_builtin_list", { cut } satisfies Args<"layout_builtin_list">, project),
  /** A built-in layout made in the framing (and linked to `parts`, if any): one undo step. */
  layoutBuiltinAdd: (project: string, a: Args<"layout_builtin_add">) => call<Out<"layout_builtin_add">>("layout_builtin_add", a satisfies Args<"layout_builtin_add">, project),
  /** Rounded corners, shadow and border on a layer (null clears). */
  layerFrameSet: (project: string, a: Args<"layer_frame_set">) => call<Out<"layer_frame_set">>("layer_frame_set", a satisfies Args<"layer_frame_set">, project),
  /** Where a layer sits (fractions of the canvas; its height follows the crop). */
  placeSet: (project: string, a: Args<"place_set">) => call<Out<"place_set">>("place_set", a satisfies Args<"place_set">, project),
  /** Plate, padding, corners, shadow, bubble, cursor feel and auto zoom in one go; `before`
   * is the restore point taken first. */
  screenStudioApply: (project: string, cut: string, plate?: string) =>
    call<{ applied: boolean; before: string; notes: string[] }>("screen_studio_apply", { cut, ...(plate ? { plate } : {}) }, project),
  /** "Undo look": the whole project back to the restore point taken before it (the look
   * also changed the sources' cursors). */
  restoreTo: (project: string, snapshot: string) => call<Out<"restore_to">>("restore_to", { snapshot } satisfies Args<"restore_to">, project),
};
