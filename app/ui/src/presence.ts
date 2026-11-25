// Agents at work, drawn (decision 0059): one cursor per agent run, moved only by what that
// run committed. Every `committed` event on the project scope names its author (`run:<id>`),
// its command and the entities it changed; the cursor goes to the first of those the cut on
// screen can place, and each changed thing gets a ring for a moment. Nothing here is
// staged: no commit, no movement. A run that only reads keeps its cursor parked (its
// client's last call, from `activity`), and a cursor with no call for a while goes.
import { batch, createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";
import { actions } from "./actions";
import type { EventTriple } from "./gen/events";

/** A rectangle in the window (CSS px), and whether the thing itself is scrolled out of view
 * (the rectangle is then where it left the pane). */
export interface Place {
  x: number;
  y: number;
  w: number;
  h: number;
  off?: "left" | "right";
}

/** Finds an entity (`part:<id>`, `overlay:<id>`, …) on screen, or null when this pane does
 * not draw it. */
export type Locator = (entity: string) => Place | null;

export interface AgentCursor {
  /** The run (`run:<id>`). */
  run: string;
  project: string;
  /** The cut that was on screen when it first committed (History is asked by cut). */
  cut: string | null;
  /** The agent's own name for itself (a session label, a client), shown as text. */
  name: string;
  /** Its identity as History names it, once known. */
  identity: string | null;
  hue: number;
  /** What its last commit did, in a word or two. */
  label: string;
  /** The entity its cursor is on (the last one a pane could place). */
  target: string | null;
  /** Which end of it the commit moved (a trim's edge), else its middle. */
  side: "in" | "out" | "mid";
  /** Where the thing was when the commit took it away (a deleted part is nowhere now). */
  pin: Place | null;
  /** Commits seen from this run while the window watched. */
  changes: number;
  /** `performance.now()` of its last commit, and of its client's last call of any kind. */
  editedAt: number;
  calledAt: number;
}

export interface AgentMark {
  key: string;
  entity: string;
  run: string;
  hue: number;
  /** `create`, `set` or `delete`. */
  op: string;
  /** A deleted thing's place when the commit arrived (it cannot be found afterwards). */
  was: Place | null;
  until: number;
}

/** A cursor says what it did for this long, then shows its agent's name. */
export const LABEL_MS = 2600;
/** A ring on a changed thing lasts this long. */
export const MARK_MS = 1800;
/** The outline of a removed thing closes up in this long. */
export const GONE_MS = 800;
/** With no call from its agent for this long, a cursor goes. */
export const LINGER_MS = 20_000;
/** An agent is "working" for this long after its last commit (errors from a half-made
 * edit wait this out before they are shown). */
export const SETTLE_MS = 4000;
/** A cursor takes a new target at most this often (a burst commits every few ms). */
const RETARGET_MS = 140;
/** The most rings at once (a burst of a thousand ops keeps only the newest). */
const MAX_MARKS = 32;

export const [cursors, setCursors] = createStore<AgentCursor[]>([]);
export const [marks, setMarks] = createStore<AgentMark[]>([]);
/** Goes up with every agent commit and every cursor that leaves (a clock for memos). */
export const [presenceTick, setPresenceTick] = createSignal(0);

const locators = new Set<Locator>();
/** A pane that draws entities says where they are; returns its removal. */
export function registerLocator(l: Locator): () => void {
  locators.add(l);
  return () => locators.delete(l);
}

/** Where an entity is on screen, by the first pane that draws it. */
export function locate(entity: string): Place | null {
  for (const l of locators) {
    const p = l(entity);
    if (p) return p;
  }
  return null;
}

// six hues apart enough to tell (none the app's own accent or its danger red)
const HUES = [200, 280, 150, 35, 320, 95];
/** A run's hue: from its id, so it is the same each time, moved on to the next hue no
 * cursor on screen has (two agents on one cut are never one colour, up to six). */
export function hueOf(run: string): number {
  let h = 2166136261;
  for (let i = 0; i < run.length; i++) h = Math.imul(h ^ run.charCodeAt(i), 16777619);
  const start = (h >>> 0) % HUES.length;
  const taken = new Set(cursors.map((c) => c.hue));
  for (let k = 0; k < HUES.length; k++) {
    const hue = HUES[(start + k) % HUES.length]!;
    if (!taken.has(hue)) return hue;
  }
  return HUES[start]!;
}

/** `part_trim` → "Trim", `overlay_add` → "Add overlay", `words_strike` → "Strike words". */
export function verbOf(command: string): string {
  const [noun = "", ...rest] = command.split("_");
  const verb = rest.join(" ");
  const things: Record<string, string> = { part: "", cut: "cut", overlay: "overlay", caption: "caption", words: "words", layer: "layer", hook: "hook", gain: "level", layout: "layout", marker: "marker" };
  if (!verb) return cap(noun);
  const thing = things[noun] ?? noun;
  if (verb === "set" || verb === "edit") return thing ? `Edit ${thing}` : "Edit";
  return cap(thing ? `${verb} ${thing}` : verb);
}
const cap = (s: string) => (s ? s[0]!.toUpperCase() + s.slice(1) : s);

/** "agent:session-thread-a" / "local:session-thread-a" → "thread-a"; "agent:claude-code" → "claude-code". */
export function nameOf(identity: string): string {
  const tail = identity.includes(":") ? identity.slice(identity.indexOf(":") + 1) : identity;
  return tail.replace(/^session-/, "") || "Agent";
}

const pendingTarget = new Map<string, { entity: string; timer: ReturnType<typeof setTimeout> }>();
const lastRetarget = new Map<string, number>();
const naming = new Set<string>();

/** Who a run is: History's row for it (the run's own identity, never the event's text). */
async function nameRun(project: string, cut: string | null, run: string) {
  if (naming.has(run) || !cut || cursors.some((c) => c.run === run && c.identity !== null)) return;
  naming.add(run);
  try {
    const h = await actions.historyList(project, cut);
    const id = run.replace(/^run:/, "");
    const row = h.versions.find((v) => v.run === id && v.run_by);
    if (row?.run_by) {
      const label = row.run_label && row.run_label !== "implicit" ? row.run_label : null;
      setCursors((c) => c.run === run && c.identity === null, produce((c) => {
        c.identity = row.run_by;
        c.name = label ?? nameOf(row.run_by!);
      }));
    } else setTimeout(() => naming.delete(run), 700); // not written yet: asked again shortly
  } catch {
    setTimeout(() => naming.delete(run), 700);
  }
}

function retarget(run: string, entity: string) {
  const now = performance.now();
  const apply = () => {
    lastRetarget.set(run, performance.now());
    pendingTarget.delete(run);
    setCursors((c) => c.run === run, "target", entity);
  };
  const waited = now - (lastRetarget.get(run) ?? -Infinity);
  const p = pendingTarget.get(run);
  if (p) {
    p.entity = entity;
    clearTimeout(p.timer);
  }
  if (waited >= RETARGET_MS) return apply();
  pendingTarget.set(run, { entity, timer: setTimeout(() => {
    const q = pendingTarget.get(run);
    if (q) {
      lastRetarget.set(run, performance.now());
      pendingTarget.delete(run);
      setCursors((c) => c.run === run, "target", q.entity);
    }
  }, RETARGET_MS - waited) });
}

let sweeper: ReturnType<typeof setInterval> | undefined;
let lastPoll = 0;
/** Marks past their time and cursors past their linger go; while any cursor shows, the
 * agents' last calls are read every two seconds (a run that is only looking stays). */
function sweep() {
  const now = performance.now();
  if (marks.some((m) => m.until <= now)) setMarks((ms) => ms.filter((m) => m.until > now));
  const gone = cursors.filter((c) => now - Math.max(c.editedAt, c.calledAt) > LINGER_MS);
  if (gone.length) {
    setCursors((cs) => cs.filter((c) => !gone.includes(c)));
    for (const g of gone) {
      naming.delete(g.run);
      lastRetarget.delete(g.run);
    }
    setPresenceTick((n) => n + 1);
  }
  // a run not named yet (History had no row for it when its first commit arrived)
  for (const c of cursors) if (c.identity === null && now - c.editedAt > 400) void nameRun(c.project, c.cut, c.run);
  if (cursors.length && now - lastPoll > 2000) {
    lastPoll = now;
    void actions.activity().then((a) => {
      const at = performance.now();
      for (const cl of a.clients ?? []) {
        const who = nameOf(cl.identity);
        // `last_ms` is how long ago the client last called anything
        const ago = Date.now() - cl.last_ms;
        if (ago > LINGER_MS || ago < 0) continue;
        setCursors((c) => c.identity !== null && nameOf(c.identity) === who, "calledAt", (t) => Math.max(t, at - ago));
      }
    }).catch(() => undefined);
  }
  if (!cursors.length && !marks.length) {
    clearInterval(sweeper);
    sweeper = undefined;
  }
}

/** The project an agent is editing now: set by its commits, cleared [`SETTLE_MS`] after the
 * last one. While it is set, a cut that does not plan is a half-made edit, not an error. */
export const [agentEditing, setAgentEditing] = createSignal<string | null>(null);
let settleTimer: ReturnType<typeof setTimeout> | undefined;
const settled = new Set<() => void>();
/** Runs `f` each time the agents go quiet (what was held back while they worked is decided then). */
export function onSettled(f: () => void): () => void {
  settled.add(f);
  return () => settled.delete(f);
}

type Commit = { by: string; command: string; triples: EventTriple[] };

/** One batch of the project scope's events (the transport sends one per 16 ms): the agents'
 * commits in it. `cut`: the cut shown, for naming. A burst of a thousand commits costs one
 * pass: each run's count, its last commit for the cursor, the newest rings. */
export function noteCommits(project: string, cut: string | null, events: readonly Commit[]) {
  const mine = events.filter((e) => e.by.startsWith("run:"));
  if (!mine.length) return;
  const now = performance.now();
  const last = new Map<string, { e: Commit; n: number }>();
  for (const e of mine) last.set(e.by, { e, n: (last.get(e.by)?.n ?? 0) + 1 });
  batch(() => {
    // a run seen for the first time gets its cursor, and its hue, before anything is drawn
    for (const [run, { e }] of last) {
      if (cursors.some((c) => c.run === run)) continue;
      setCursors(cursors.length, { run, project, cut, name: "Agent", identity: null, hue: hueOf(run), label: verbOf(e.command), target: null, side: "mid", pin: null, changes: 0, editedAt: now, calledAt: now });
    }
    // rings: the newest commits' entities, one per entity (a transaction writes several
    // fields of one thing), newest last
    const fresh = new Map<string, AgentMark>();
    for (let i = mine.length - 1; i >= 0 && fresh.size < MAX_MARKS; i--) {
      const e = mine[i]!;
      const hue = cursors.find((c) => c.run === e.by)!.hue;
      for (const [entity, op] of entitiesOf(e.triples)) {
        const key = `${e.by}|${entity}`;
        if (fresh.has(key)) continue;
        fresh.set(key, { key, entity, run: e.by, hue, op, was: op === "delete" ? locate(entity) : null, until: now + (op === "delete" ? GONE_MS : MARK_MS) });
      }
    }
    for (const [run, { e, n }] of last) {
      const label = verbOf(e.command);
      // the cursor goes to the first changed thing a pane can place: what the commit made
      // or edited before what it removed (a removed thing is only where it was)
      const ents = [...entitiesOf(e.triples)].sort((a, b) => Number(a[1] === "delete") - Number(b[1] === "delete"));
      const hit = ents.find(([en]) => locate(en)) ?? ents[0];
      const field = hit ? e.triples.find((t) => t.entity === hit[0] && t.field)?.field : null;
      const side = field === "src_in" || field === "from" ? "in" : field === "src_out" || field === "to" ? "out" : "mid";
      const pin = hit && hit[1] === "delete" ? fresh.get(`${run}|${hit[0]}`)?.was ?? locate(hit[0]) : null;
      setCursors((c) => c.run === run, produce((c) => {
        c.label = label;
        c.changes += n;
        c.editedAt = now;
        c.calledAt = now;
        if (hit) {
          c.side = side;
          c.pin = pin;
        }
      }));
      if (hit) retarget(run, hit[0]);
      void nameRun(project, cut, run);
    }
    if (fresh.size) setMarks((ms) => [...ms.filter((m) => !fresh.has(m.key)), ...[...fresh.values()].reverse()].slice(-MAX_MARKS));
    setAgentEditing(project);
    setPresenceTick((n) => n + 1);
  });
  clearTimeout(settleTimer);
  settleTimer = setTimeout(() => {
    setAgentEditing(null);
    for (const f of settled) f();
  }, SETTLE_MS);
  sweeper ??= setInterval(sweep, 250);
}

/** Each entity a transaction touched, with what happened to it (`create` and `delete`
 * win over a field `set`). */
function entitiesOf(triples: readonly EventTriple[]): Map<string, string> {
  const seen = new Map<string, string>();
  for (const t of triples) {
    const was = seen.get(t.entity);
    if (!t.field) seen.set(t.entity, t.op);
    else if (!was) seen.set(t.entity, "set");
  }
  return seen;
}

/** A run named by the cut's own view (`agent_working`), before History is asked. */
export function knowRun(run: string, identity: string) {
  const id = run.startsWith("run:") ? run : `run:${run}`;
  setCursors((c) => c.run === id && c.identity === null, produce((c) => {
    c.identity = identity;
    c.name = nameOf(identity);
  }));
}

/** Forgets every cursor and ring (the cut on screen changed). */
export function clearPresence() {
  for (const p of pendingTarget.values()) clearTimeout(p.timer);
  pendingTarget.clear();
  lastRetarget.clear();
  naming.clear();
  setCursors([]);
  setMarks([]);
  clearTimeout(settleTimer);
  setAgentEditing(null);
  setPresenceTick((n) => n + 1);
}
