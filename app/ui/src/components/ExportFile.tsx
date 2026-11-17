// The cut's newest export in the cut header (decision 0054: the review board folded into
// the cut). The export popover hands over the file — Reveal in Finder, Share… (the macOS
// share sheet) and a drag out of the window — and the status pill shows the agent's
// handoff note (its own words, unchecked) with one Restore per recorded choice. Both read
// the board's tile for this cut's newest export (board_view).
import { For, Show } from "solid-js";
import { actions, type BoardTile } from "../actions";
import { Button } from "../design/components";
import { baseName, fileManager, isMac, isWindows } from "../platform";
import { attempt, say } from "../store";

/** The newest export of `cut` among the project's exports (newest first), or null. */
export async function lastExport(project: string, cut: string): Promise<BoardTile | null> {
  return (await attempt(() => actions.board(project)))?.tiles.find((t) => t.cut === cut) ?? null;
}

/** The export's file: its name, Reveal, Share… and drag it out. */
export function ExportFile(props: { project: string; tile: BoardTile }) {
  const t = () => props.tile;
  const drags = () => t().exists && (isMac || isWindows);
  return (
    <div
      class="export-file"
      data-testid="export-file"
      // the file itself leaves through the system's drag (AppKit; OLE on Windows)
      draggable={drags()}
      data-tip={drags() ? `Drag to ${isMac ? "the Finder" : "File Explorer"}, Mail or a chat to hand over the file` : undefined}
      onDragStart={(e) => {
        e.preventDefault();
        void attempt(() => actions.dragOut(props.project, t().id));
      }}
    >
      <span class="hint export-file-name" title={t().file}>{t().exists ? baseName(t().file) : "The file is gone (moved or deleted)."}</span>
      <span class="export-file-acts">
        <Button size="sm" variant="ghost" icon="folder-open" data-testid="export-reveal" disabled={!t().exists} onClick={() => void attempt(() => actions.reveal(props.project, t().id))}>
          Reveal in {fileManager}
        </Button>
        <Button size="sm" variant="ghost" icon="share" data-testid="export-share" disabled={!t().exists} tooltip="AirDrop, Messages, Mail, Save to Photos…" onClick={(e) => void attempt(() => actions.share(props.project, t().id, e.clientX, e.clientY))}>
          Share…
        </Button>
      </span>
    </div>
  );
}

/** The agent's handoff on the cut's last run: its words as text, and a Restore per choice
 * (one undo step each). */
export function HandoffNote(props: { project: string; tile: BoardTile }) {
  const restore = async (choice: number, label: string) => {
    const r = await attempt(() => actions.runChoiceRestore(props.project, props.tile.cut, choice));
    if (r) say(r.restored ? `Restored: “${label.slice(0, 60)}”` : "Already as recorded");
  };
  return (
    <Show when={props.tile.handoff}>
      {(h) => (
        <section class="untrusted from-agent handoff" data-testid="handoff" aria-label="From the agent (its own words, unchecked)">
          <p class="small muted">From the agent (its own words, unchecked):</p>
          <p class="small handoff-text">{h().text}</p>
          <For each={h().choices}>
            {(c) => (
              <p class="row small handoff-choice" data-choice={c.index}>
                <span>{c.label}</span>
                <span class="spacer" />
                <button type="button" class="ghost small" data-testid="choice-restore" onClick={() => void restore(c.index, c.label)}>
                  Restore
                </button>
              </p>
            )}
          </For>
        </section>
      )}
    </Show>
  );
}
