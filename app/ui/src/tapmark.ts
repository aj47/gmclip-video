// Tap marking while watching (lane L, L.04): `,` drops an in and `.` an out at the frame on
// screen, in the cut view, at any speed; the pair becomes a moment by
// you, its edges snapped to the nearest sentence start and end (SPEC §5.2's human times).
import { createSignal } from "solid-js";
import { tapActions } from "./actions";
import { attempt, say, viewer } from "./store";

type Target = { cut: string };

/** The pending in: the frame and what it was dropped on. */
export const [tapIn, setTapIn] = createSignal<{ frame: number; key: string } | null>(null);

function mmss(s: number): string {
  const t = Math.floor(s);
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, "0")}`;
}

export function tapMark(kind: "in" | "out", project: string, target: Target, onMarked?: () => void) {
  const f = viewer()?.frame;
  if (f === undefined) return;
  const key = `${project}:${JSON.stringify(target)}`;
  if (kind === "in") {
    setTapIn({ frame: f, key });
    say("In marked · . marks the out");
    return;
  }
  const a = tapIn();
  if (!a || a.key !== key) {
    say("Press , for the in first", "error");
    return;
  }
  if (f <= a.frame) {
    say("The out comes after the in", "error");
    return;
  }
  setTapIn(null);
  void attempt(() => tapActions.mark(project, target, a.frame, f)).then((r) => {
    if (!r) return;
    say(`Moment marked ${mmss(r.moment.start_s)}–${mmss(r.moment.end_s)} · ${r.moment.words} words, on sentence edges`);
    onMarked?.();
  });
}
