// Agent sessions (decision 0035): the list, the open session's transcript, and the
// launch sheet's target, fed by `session_*` calls and the `agents` scope. The core owns
// every session; this only mirrors it (entries by index, streamed text appended).
import { createSignal } from "solid-js";
import { createStore, produce, reconcile } from "solid-js/store";
import { sessionActions, type SessionAccess, type SessionEntry, type SessionTargetArgs, type SessionView } from "./actions";
import { attempt, route, say, subscribe } from "./store";

const [listStore, setListStore] = createStore<{ v: SessionView[] }>({ v: [] });
/** Every session, newest activity first. */
export const sessions = () => listStore.v;
/** Sessions whose agent is running. */
export const liveSessions = () => listStore.v.filter((s) => isLive(s.state)).length;
/** Sessions waiting for the human (a permission question). */
export const waitingSessions = () => listStore.v.filter((s) => s.state === "waiting").length;

export const isLive = (state: SessionView["state"]) => state === "starting" || state === "working" || state === "waiting" || state === "idle";
/** An ended session that once opened (it has a folder): asking picks it up again, if its
 * agent can (the core asks the agent; `resumable` predates resume and may say false). */
export const canPickUp = (s: SessionView) => !isLive(s.state) && !!s.cwd;

/** The session open in the Agents view. */
export const [openSession, setOpenSession] = createSignal<string | null>(null);
const [entryStore, setEntryStore] = createStore<{ id: string | null; v: SessionEntry[] }>({ id: null, v: [] });
/** The open session's transcript. */
export const entries = () => entryStore.v;

/** What the launch sheet opens on (null: closed). */
export const [launchFor, setLaunchFor] = createSignal<(SessionTargetArgs & { label?: string; harness?: string }) | null>(null);

function upsert(s: SessionView) {
  setListStore("v", (v) => {
    const rest = v.filter((x) => x.id !== s.id);
    return [s, ...rest].sort((a, b) => b.last_ms - a.last_ms);
  });
}

export async function refreshSessions() {
  const r = await attempt(sessionActions.list);
  if (r) setListStore("v", reconcile(r.sessions, { key: "id" }));
}

/** Loads the transcript of `id` (and makes it the open one). */
export async function loadSession(id: string) {
  setOpenSession(id);
  const r = await attempt(() => sessionActions.get(id));
  if (!r || openSession() !== id) return;
  upsert(r.session);
  setEntryStore({ id, v: r.entries });
}

let subscribed = false;
/** Subscribes to the `agents` scope once (the first time anything shows sessions). */
export async function watchSessions() {
  if (subscribed) return;
  subscribed = true;
  await subscribe("agents", (b) => {
    for (const e of b.events) {
      switch (e.kind) {
        case "resync": {
          void refreshSessions();
          const id = openSession();
          if (id) void loadSession(id);
          break;
        }
        case "session": {
          const prev = listStore.v.find((x) => x.id === e.session.id);
          upsert(e.session);
          if (prev && prev.state !== e.session.state) notify(e.session, prev.state);
          break;
        }
        case "entry":
          if (entryStore.id === e.session)
            setEntryStore(
              "v",
              produce((v) => {
                if (e.index < v.length) v[e.index] = e.entry;
                else v.push(e.entry);
              }),
            );
          break;
        case "append":
          if (entryStore.id === e.session)
            setEntryStore(
              "v",
              e.index,
              produce((x) => {
                if (x && "text" in x) x.text += e.text;
              }),
            );
          break;
        case "removed":
          setListStore("v", (v) => v.filter((x) => x.id !== e.session));
          if (openSession() === e.session) {
            setOpenSession(null);
            setEntryStore({ id: null, v: [] });
          }
          break;
      }
    }
  });
  await refreshSessions();
}

/** A toast when a session needs the human or ends, unless it is the one on screen. */
function notify(s: SessionView, was: SessionView["state"]) {
  const here = openSession() === s.id && route().view === "agents";
  if (s.state === "waiting" && !here) say(`${s.harness_label} asks before running a tool: see Agents`);
  else if (s.state === "failed") say(`${s.harness_label}: ${s.error ?? "the session failed"}`, "error");
  else if (s.state === "idle" && was === "working" && !here) say(`${s.harness_label} finished: ${s.title}`);
}

/** Starts a session and opens it; returns its id. */
export async function startSession(...args: Parameters<typeof sessionActions.start>): Promise<string | null> {
  await watchSessions();
  const r = await attempt(() => sessionActions.start(...args));
  if (!r) return null;
  upsert(r.session);
  await loadSession(r.session.id);
  return r.session.id;
}

/** The access last picked to start a session with. */
export const lastAccess = (): SessionAccess => {
  const a = localStorage.getItem("gmclip.agents.access");
  return a === "full" || a === "bypass" ? a : "gmclip";
};
export const setLastAccess = (a: SessionAccess) => localStorage.setItem("gmclip.agents.access", a);

/** The model last picked for each agent ("" = the agent's own default). */
export const lastModel = (harness: string) => localStorage.getItem(`gmclip.agents.model.${harness}`) ?? "";
export const setLastModel = (harness: string, model: string) => localStorage.setItem(`gmclip.agents.model.${harness}`, model);
