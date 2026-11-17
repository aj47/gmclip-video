// Reliability (SPEC §14, §12.5 R-ERR-2; lane S): "Restore an earlier copy" from Home, and
// Settings › Storage with "Free up N GB" (rebuildable caches only, never project data).
import { thisMachine } from "../platform";
import { createResource, createSignal, For, Show } from "solid-js";
import { actions, type BackupInfo } from "../actions";
import { Button, Empty } from "../design/components";
import { attempt, refreshHome, refreshProjects, say, watchProject } from "../store";
import "./firstrun.css";

export function bytes(n: number): string {
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)} GB`;
  if (n >= 1e6) return `${Math.round(n / 1e6)} MB`;
  if (n >= 1e3) return `${Math.round(n / 1e3)} KB`;
  return `${n} B`;
}

function when(ms: number): string {
  const d = new Date(ms);
  const today = new Date();
  const same = d.toDateString() === today.toDateString();
  const t = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  return same ? `today ${t}` : `${d.toLocaleDateString([], { month: "short", day: "numeric" })} ${t}`;
}

const REASON: Record<string, string> = {
  quit: "when you quit",
  timer: "while editing",
  manual: "made by you",
  "before-restore": "before a restore",
};

/** Home › Restore an earlier copy: each project's copies; restoring keeps the current state as a copy first. */
export function RestoreSheet(props: { onClose: () => void }) {
  const [ov, { refetch }] = createResource(async () => (await attempt(actions.backupsOverview)) ?? null);
  const [asking, setAsking] = createSignal<{ project: string; name: string; b: BackupInfo } | null>(null);
  const [busy, setBusy] = createSignal(false);
  const restore = async () => {
    const a = asking();
    if (!a) return;
    setBusy(true);
    const r = await attempt(() => actions.backupRestore(a.project, a.b.id));
    setBusy(false);
    setAsking(null);
    if (r) {
      say(`Restored “${a.name}” to its copy from ${when(a.b.at_ms)} — the state before is kept as a copy`);
      await watchProject(r.project.id);
      await refreshProjects();
      refreshHome();
      void refetch();
    }
  };
  return (
    <div class="keys-backdrop" onPointerDown={(e) => { if (e.target === e.currentTarget) props.onClose(); }}>
      <section class="keys-dialog restore-sheet" role="dialog" aria-modal="true" aria-label="Restore an earlier copy" data-testid="restore-sheet">
        <header>
          <h2 data-tip="gmclip keeps a copy of each project when you quit and every 30 minutes while you edit: the newest 5, and one a day for a week.">Restore an earlier copy</h2>
          <button type="button" class="ghost" onClick={() => props.onClose()} aria-label="Close">✕</button>
        </header>
        <Show when={ov()} fallback={<p class="muted">Looking for copies…</p>}>
          {(o) => (
            <>
              <For each={o().projects} fallback={<Empty compact icon="history" title="No copies yet.">They start once you edit a project and quit or keep editing for {o().every_min} minutes.</Empty>}>
                {(p) => (
                  <div class="restore-project" data-testid="restore-project">
                    <h3>{p.name}{p.open ? <span class="tag">open</span> : null}</h3>
                    <ul class="restore-list">
                      <For each={p.backups}>
                        {(b) => (
                          <li data-testid="restore-copy" data-reason={b.reason}>
                            <span>{when(b.at_ms)}</span>
                            <span class="muted small">{REASON[b.reason] ?? b.reason} · {bytes(b.bytes)}</span>
                            <Button size="sm" variant="ghost" icon="restore" data-testid="restore-pick" disabled={busy() || !p.home} tooltip={p.home ? "Put this copy back (the current state is kept as a copy first)" : `The project's folder is not on ${thisMachine}`} onClick={() => setAsking({ project: p.project, name: p.name, b })}>
                              Restore…
                            </Button>
                          </li>
                        )}
                      </For>
                    </ul>
                  </div>
                )}
              </For>
            </>
          )}
        </Show>
        <Show when={asking()}>
          {(a) => (
            <div class="restore-confirm" role="alertdialog" aria-label="Confirm restore" data-testid="restore-confirm">
              <p>
                Put “{a().name}” back to {when(a().b.at_ms)}? {""}
                <span class="muted">The project closes and reopens; what it is now is kept as a copy, so you can undo this from here.</span>
              </p>
              <div class="row">
                <Button variant="primary" icon="restore" data-testid="restore-go" disabled={busy()} onClick={() => void restore()}>{busy() ? "Restoring…" : "Restore"}</Button>
                <Button variant="ghost" onClick={() => setAsking(null)}>Cancel</Button>
              </div>
            </div>
          )}
        </Show>
      </section>
    </div>
  );
}

/** Settings › Storage: what gmclip keeps here, and "Free up" for the rebuildable caches. */
export function StoragePanel() {
  const [u, { refetch }] = createResource(async () => (await attempt(actions.storageUsage)) ?? null);
  const [busy, setBusy] = createSignal(false);
  const free = async () => {
    setBusy(true);
    const r = await attempt(() => actions.storageFree([]));
    setBusy(false);
    if (r) say(r.freed_bytes ? `Freed ${bytes(r.freed_bytes)}${r.kept.length ? " (caches of open projects kept)" : ""}` : "Nothing to free");
    void refetch();
  };
  return (
    <section class="settings-section" aria-label="Storage" data-testid="storage">
      <h3>Storage</h3>
      <Show when={u()} fallback={<p class="muted small">Measuring…</p>}>
        {(x) => (
          <>
            <table class="storage-table">
              <tbody>
                <For each={x().rows}>
                  {(r) => (
                    <tr data-testid="storage-row" data-id={r.id}>
                      <th scope="row" data-tip={r.what}>{r.label}</th>
                      <td class="num">{bytes(r.bytes)}</td>
                      <td class="muted small">{r.freeable ? "can be freed" : "kept"}</td>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
            <div class="row">
              <Button icon="minus" data-testid="storage-free" disabled={busy() || x().freeable_bytes === 0} tooltip="Removes scrub proxies, moment clips, cut-out masks, search vectors and thumbnails; each is made again when needed. Never your projects, exports or backups." onClick={() => void free()}>
                {x().freeable_bytes ? `Free up ${bytes(x().freeable_bytes)}` : "Nothing to free"}
              </Button>
              <Show when={x().kept_open_bytes > 0}>
                <span class="muted small">{bytes(x().kept_open_bytes)} in use by open projects stays.</span>
              </Show>
            </div>
          </>
        )}
      </Show>
    </section>
  );
}
