// Recording (decision 0045, SPEC §5.18): the Record sheet's flow and the recording bar,
// shown over every view. `recording_prepare` starts the streams warm while a 3-2-1
// countdown runs (Esc cancels), `recording_go` starts the files, `recording_status` is
// read every 500 ms; Stop on the bar or ⌃⌥S (read as `stop_asked`) closes the files and
// the session opens as a project, as Home opens `videos_open`'s result.
import { createSignal } from "solid-js";
import { recordingActions, type RecOpened, type RecOptions, type RecRunning } from "./actions";
import { attempt, openCut, refreshProjects, say, watchProject } from "./store";

/** The Record sheet is open. */
export const [recordSheet, setRecordSheet] = createSignal(false);
/** 3, 2, 1 before the files start (null: no countdown). */
export const [countdown, setCountdown] = createSignal<number | null>(null);
/** What the prepared recording goes without (a refused permission, no camera). */
export const [recMissing, setRecMissing] = createSignal<string[]>([]);
/** The recording running now, as the last poll read it. */
export const [rec, setRec] = createSignal<RecRunning | null>(null);
/** Stop was asked and the session is being closed and opened. */
export const [recStopping, setRecStopping] = createSignal(false);
/** Keep the session folder available when closing or importing fails. */
const RECOVERY_KEY = "gmclip-recording-recovery";
function savedRecovery(): string | null {
  try { return localStorage.getItem(RECOVERY_KEY); } catch { return null; }
}
const [recovery, setRecovery] = createSignal<string | null>(savedRecovery());
export const recRecovery = recovery;
export function setRecRecovery(dir: string | null) {
  setRecovery(dir);
  try {
    if (dir) localStorage.setItem(RECOVERY_KEY, dir);
    else localStorage.removeItem(RECOVERY_KEY);
  } catch { /* Keep the recovery path visible even when storage is unavailable. */ }
}

let tick: ReturnType<typeof setTimeout> | undefined;
let poll: ReturnType<typeof setInterval> | undefined;

function stopPolling() {
  clearInterval(poll);
  poll = undefined;
}

async function read() {
  let s;
  try {
    s = await recordingActions.status();
  } catch {
    return;
  }
  const r = s.recording;
  // stopped elsewhere (the CLI, a cancel): the bar goes
  if (!r || !r.started) {
    if (!recStopping()) {
      stopPolling();
      setRec(null);
    }
    return;
  }
  // A busy capture callback may skip a statistics poll; retain observed overload.
  setRec({ ...r, dropped: Math.max(r.dropped ?? 0, rec()?.dropped ?? 0) });
  if (r.stop_asked && !recStopping()) void stopRecording();
}

function startPolling() {
  stopPolling();
  void read();
  poll = setInterval(() => void read(), 500);
}

/** Prepares the streams and starts the countdown; false when it could not (the error said). */
export async function startRecording(o: RecOptions): Promise<boolean> {
  const r = await attempt(() => recordingActions.prepare(o));
  if (!r) return false;
  setRecMissing(r.missing);
  setCountdown(3);
  const step = () => {
    const n = countdown();
    if (n === null) return;
    if (n > 1) {
      setCountdown(n - 1);
      tick = setTimeout(step, 1000);
    } else {
      setCountdown(null);
      void go();
    }
  };
  tick = setTimeout(step, 1000);
  return true;
}

async function go() {
  const r = await attempt(recordingActions.go);
  if (!r) {
    await attempt(recordingActions.cancel);
    return;
  }
  startPolling();
}

/** Esc during the countdown: the prepared recording and its folder go. */
export async function cancelCountdown() {
  if (countdown() === null) return;
  clearTimeout(tick);
  setCountdown(null);
  setRecMissing([]);
  await attempt(recordingActions.cancel);
  say("Recording cancelled");
}

export async function addMarker() {
  const r = await attempt(recordingActions.marker);
  const now = rec();
  if (r && now) setRec({ ...now, markers: r.markers });
}

export async function stopRecording() {
  if (recStopping()) return;
  const dir = rec()?.dir;
  setRecStopping(true);
  stopPolling();
  const r = await attempt(recordingActions.stop);
  setRecStopping(false);
  setRec(null);
  setRecMissing([]);
  if (!r) {
    if (dir) setRecRecovery(dir);
    return;
  }
  if (r.cancelled) return;
  if (recRecovery() === dir) setRecRecovery(null);
  await openSession(r);
}

/** A session folder made earlier (a drop, Home's field): opened as a project. `quiet`
 * (a dropped folder that may not be one): false without a word when it is not. */
export async function openRecording(dir: string, quiet = false): Promise<boolean> {
  const r = quiet ? await recordingActions.open(dir).catch(() => undefined) : await attempt(() => recordingActions.open(dir));
  if (r) {
    if (recRecovery() === dir) setRecRecovery(null);
    await openSession(r);
  }
  return !!r;
}

async function openSession(r: RecOpened) {
  if (r.notes.length) say(r.notes.join(" · "));
  await attempt(() => watchProject(r.project));
  await refreshProjects();
  if (r.cut) await openCut(r.project, r.cut);
}

/** At launch (or a reload) a recording still running gets its bar back. */
export async function resumeRecording() {
  try {
    const s = await recordingActions.status();
    if (s.recording?.started) {
      setRec(s.recording);
      startPolling();
    }
  } catch {
    // an older core: no recording
  }
}
