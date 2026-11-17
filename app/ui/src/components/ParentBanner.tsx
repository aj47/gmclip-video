// "Stream edit changed — N edits" (decision 0011): a long-form shows what the stream edit
// did inside its topics since it last took its changes; Pull changes applies them as one
// undoable step. Nothing propagates by itself (AJ: a long-form always asks).
import { createResource, createSignal, For, Show } from "solid-js";
import { actions } from "../actions";
import { attempt, cutView, say } from "../store";

function clock(ms: number): string {
  const t = Math.round(ms / 1000);
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const s = String(t % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

export function ParentBanner() {
  const [open, setOpen] = createSignal(false);
  const [diff, { refetch }] = createResource(
    () => {
      const v = cutView();
      return v && v.kind === "long" ? { p: v.project, c: v.cut, rev: v.rev } : undefined;
    },
    async (k) => (await actions.parentDiff(k.p, k.c).catch(() => null)) ?? null,
  );
  const pull = async () => {
    const v = cutView();
    if (!v) return;
    const r = await attempt(() => actions.pullFromParent(v.project, v.cut));
    if (r) {
      say(`Pulled ${r.applied} stream-edit edit${r.applied === 1 ? "" : "s"} · ⌘Z undoes`);
      setOpen(false);
      void refetch();
    }
  };
  return (
    <Show when={diff() && diff()!.edits.length > 0 ? diff() : null}>
      {(d) => (
        <div class="parent-banner" role="status" data-testid="parent-banner">
          <span>
            Stream edit changed — <b>{d().edits.length} edit{d().edits.length === 1 ? "" : "s"}</b>
            <span class="muted small"> ({clock(d().struck_ms)} struck, {clock(d().restored_ms)} restored)</span>
          </span>
          <button type="button" class="ghost small" data-testid="parent-diff" aria-expanded={open()} onClick={() => setOpen(!open())}>{open() ? "Hide" : "Show"} diff</button>
          <button type="button" class="small primary" data-testid="parent-pull" onClick={() => void pull()}>Pull changes</button>
          <Show when={open()}>
            <ul class="parent-diff" aria-label="Stream edit changes">
              <For each={d().edits}>
                {(e) => (
                  <li class={`small ${e.kind}`}>
                    {e.kind === "struck" ? "− struck" : "+ restored"} {clock(e.dur_ms)} at {clock(e.from_ms)} in “{e.topic_title || "a removed topic"}”
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </div>
      )}
    </Show>
  );
}
