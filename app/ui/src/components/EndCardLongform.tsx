// End card to the long-form (lane K5): a short whose moment comes from a stream with a
// long-form is offered, in the Captions tab, "Full video:" with the long-form's title, a frame
// of it and an arrow over its last 3 s — built from the brand kit and the overlay kinds that
// exist. A suggestion: nothing is added until you click, and one ⌘Z (or Remove) takes it off.
import { createResource, Show } from "solid-js";
import { actions, rate } from "../actions";
import { attempt, cutView, say, sayUndo } from "../store";
import { Button } from "../design/components";

/** Adds the card to the cut on screen (the Captions tab's button and ⌘K). */
export async function addLongformEndCard(): Promise<boolean> {
  const v = cutView();
  if (!v) return false;
  const r = await attempt(() => actions.endCardLongformAdd(v.project, v.cut));
  if (!r) return false;
  sayUndo(`End card added: “Full video: ${r.title}”${r.picture ? "" : " (no frame: the recording's file is not here)"}`, v.project);
  return true;
}

export function EndCardLongform() {
  const [v, { refetch }] = createResource(
    () => {
      const c = cutView();
      return c ? { p: c.project, c: c.cut, rev: c.rev } : undefined;
    },
    async (k) => (await actions.endCardLongformView(k.p, k.c).catch(() => null)) ?? null,
  );
  const add = async () => {
    if (await addLongformEndCard()) void refetch();
  };
  const remove = async () => {
    const c = cutView();
    if (!c) return;
    if (await attempt(() => actions.endCardLongformRemove(c.project, c.cut))) {
      sayUndo("End card removed", c.project);
      void refetch();
    }
  };
  const seek = () => {
    const c = cutView();
    if (c) void attempt(() => actions.viewerSeek(Math.max(0, c.frames - Math.round(3 * rate(c.fps)))));
  };
  return (
    <div class="lf-card" data-testid="endcard-lf" data-available={v()?.available ? "1" : "0"} data-has={v()?.has_card ? "1" : "0"}>
      <h3>End card to the long-form</h3>
      <Show when={v()} fallback={<p class="muted small" role="status">{v.loading ? "Looking for a long-form of this moment…" : "Cannot tell whether this moment is in a long-form."}</p>}>
        {(x) => (
          <Show when={x().available} fallback={<p class="muted small" data-testid="endcard-lf-why">{x().why}</p>}>
            <Show
              when={!x().has_card}
              fallback={
                <>
                  <p class="small" data-testid="endcard-lf-on">On the last 3 s: “Full video: {x().title}”.</p>
                  <div class="row lf-card-actions">
                    <Button size="sm" variant="ghost" icon="play" onClick={seek}>See it</Button>
                    <Button size="sm" variant="ghost" icon="x" data-testid="endcard-lf-remove" onClick={() => void remove()}>Remove</Button>
                  </div>
                </>
              }
            >
              <p class="muted small">
                This moment is in your long-form <b>{x().longform_title}</b>. Suggested: point viewers at the full video over the last 3 s.
              </p>
              <div class="lf-card-preview" aria-hidden="true">
                <span class="lfp-label">Full video:</span>
                <span class="lfp-title">{x().title}</span>
                <span class="lfp-frame" />
                <span class="lfp-arrow">↓</span>
              </div>
              <Button size="sm" variant="secondary" icon="plus" data-testid="endcard-lf-add" tooltip="Brand kit colours and face; ⌘Z takes it off" onClick={() => void add()}>
                Add end card
              </Button>
            </Show>
          </Show>
        )}
      </Show>
    </div>
  );
}
