// Shared pieces of the app chrome: the top bar's slot (the cut view puts its header
// there, so the cut has one 44 px bar, not two), the save status words and the
// "exported — reveal it" chip.
import { createSignal, Show } from "solid-js";
import { Icon } from "../design/Icon";
import { actions } from "../actions";
import { fileManager } from "../platform";
import { attempt, justExported, saved, saveError, setJustExported } from "../store";

/** The top bar's middle, where a view mounts its own header (a Portal target). */
export const [topbarSlot, setTopbarSlot] = createSignal<HTMLElement>();

// R-ERR-2: a full disk keeps every edit in the app and retries; the chip says so
const diskFull = () => saved() === "failed" && /disk full/i.test(saveError() ?? "");
export const savedText = () => (saved() === "saved" ? "Saved" : saved() === "saving" ? "Saving…" : diskFull() ? "Not saved: disk full" : "Not saved");
export const savedTip = () =>
  diskFull()
    ? "Your Mac's disk is full. Every edit is kept here and saved as soon as there is space — retrying every second. Free up space (Settings › Storage) and keep working."
    : saveError() ?? (saved() === "saved" ? "Every change is saved as you go (⌘S makes a restore point)" : undefined);

export function ExportDoneChip() {
  return (
    <Show when={justExported()}>
      {(x) => (
        <span class="export-done" role="status" data-testid="export-done">
          <Icon name="circle-check" size={14} />
          <span data-tip={`Exported “${x().title}”`}>Exported “{x().title}”</span>
          <Show when={x().export}>
            {(id) => <button type="button" class="ghost small" data-testid="export-done-reveal" onClick={() => { void attempt(() => actions.reveal(x().project, id())); setJustExported(null); }}>Reveal in {fileManager}</button>}
          </Show>
          <button type="button" class="ghost small ibtn ibtn-sm" aria-label="Dismiss" onClick={() => setJustExported(null)}>
            <Icon name="x" size={13} />
          </button>
        </span>
      )}
    </Show>
  );
}
