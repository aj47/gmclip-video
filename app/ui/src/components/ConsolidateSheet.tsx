// Opening a consolidated project (lane L, L.06): a project folder that travels — the
// database and only the stretches of the recordings its cuts use (± handles), copied
// without re-encoding — opened on another Mac. Making one is project_consolidate (an
// agent's, or gmclip's CLI; Home has no Consolidate… since decision 0054).
import { createSignal } from "solid-js";
import { consolidateActions } from "../actions";
import { Button, IconButton } from "../design/components";
import { attempt, refreshHome, refreshProjects, say, watchProject } from "../store";

export function ConsolidateSheet(props: { onClose: () => void }) {
  const close = props.onClose;
  const [path, setPath] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const openCopy = async () => {
    setBusy(true);
    const r = await attempt(() => consolidateActions.open(path().trim()));
    setBusy(false);
    if (r) {
      await watchProject(r.id);
      await refreshProjects();
      void refreshHome();
      say("Opened the consolidated project");
      close();
    }
  };
  return (
    <div class="keys-backdrop" onPointerDown={(e) => { if (e.target === e.currentTarget) close(); }}>
      <section class="keys-dialog consolidate" role="dialog" aria-modal="true" aria-label="Open a consolidated project" data-testid="consolidate-sheet"
        onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); close(); } }}>
        <header>
          <h2>Open a consolidated project</h2>
          <IconButton icon="x" label="Close" size="sm" onClick={() => close()} />
        </header>
        <form class="consolidate-form" onSubmit={(e) => { e.preventDefault(); if (path().trim()) void openCopy(); }}>
          <input type="text" aria-label="Consolidated project folder" placeholder="Path to a …gmclip folder" value={path()} onInput={(e) => setPath(e.currentTarget.value)} />
          <Button type="submit" variant="primary" icon="folder-open" disabled={!path().trim() || busy()} data-testid="consolidate-open">{busy() ? "Opening…" : "Open"}</Button>
        </form>
      </section>
    </div>
  );
}
