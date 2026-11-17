// A human gesture on a gizmo (R-GEST-1, R-CC-2): pointer down holds the entity in the
// core, so an agent's write to it meanwhile gets Conflict{by: human}; release runs the one
// command that commits it and ends the hold; Esc (or a lost pointer) ends the hold with
// nothing written. The preview while dragging stays in the page.
import { onCleanup } from "solid-js";
import { actions, type GestureKind } from "./actions";

export interface Held {
  /** Runs the commit with the gesture id (the command ends the hold in the same call). */
  commit<T>(fn: (gesture: number | null) => Promise<T>): Promise<T>;
  /** Lets go with nothing written. */
  cancel(): void;
}

export function hold(project: string, kind: GestureKind, id: string, part?: string): Held {
  const g = actions
    .timelineGestureBegin(project, kind, id, part)
    .then((r) => r.gesture)
    .catch(() => null);
  let done = false;
  return {
    async commit(fn) {
      done = true;
      return fn(await g);
    },
    cancel() {
      if (done) return;
      done = true;
      void g.then((x) => {
        if (x !== null) void actions.timelineGestureEnd(project, x).catch(() => undefined);
      });
    },
  };
}

/** Esc while a drag is open cancels it (and nothing else hears that Esc). Call in a
 * component; `active` says whether a drag is open. */
export function escCancels(active: () => boolean, cancel: () => void) {
  const key = (e: KeyboardEvent) => {
    if (e.key !== "Escape" || !active()) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    cancel();
  };
  window.addEventListener("keydown", key, true);
  onCleanup(() => window.removeEventListener("keydown", key, true));
}
