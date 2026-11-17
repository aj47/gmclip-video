// "New cut like this" (lane L, L.05): this cut's look — framings, layouts and crops, caption
// style and seat, hook and pill styles — on another moment, without its parts. Optionally
// saved as a named template (content only) for any project.
import { createMemo, createResource, createSignal, For, onMount, Show } from "solid-js";
import { actions, cutLikeActions } from "../actions";
import { Button, Empty, IconButton } from "../design/components";
import { attempt, openCut, say } from "../store";
import { clock } from "../views/Search";

export function CutLikeSheet(props: { project: string; cut: string; title: string; onClose: () => void }) {
  // fixed for the sheet's life: the cut view under it changes when the new cut opens
  const project = props.project;
  const cut = props.cut;
  const close = props.onClose;
  const [moments] = createResource(async () => (await attempt(() => actions.moments(project)))?.moments ?? []);
  const [q, setQ] = createSignal("");
  const [pick, setPick] = createSignal<string | null>(null);
  const [save, setSave] = createSignal(false);
  const [name, setName] = createSignal(props.title.slice(0, 40));
  const [busy, setBusy] = createSignal(false);
  // the look comes from this cut, or from a saved template
  const [templates, { refetch }] = createResource(async () => (await attempt(cutLikeActions.templates))?.templates ?? []);
  const [from, setFrom] = createSignal<string>("");
  let input: HTMLInputElement | undefined;
  onMount(() => input?.focus());
  const shown = createMemo(() => {
    const t = q().trim().toLowerCase();
    return (moments() ?? [])
      .filter((m) => m.start_s !== null && (!t || `${m.excerpt} ${m.quote} ${m.text_hook ?? ""}`.toLowerCase().includes(t)))
      .slice(0, 200);
  });
  const make = async () => {
    const m = pick();
    if (!m) return;
    setBusy(true);
    const src = from() ? { template: from() } : { cut };
    const r = await attempt(() => cutLikeActions.make(project, src, m, !from() && save() ? name() : undefined));
    setBusy(false);
    if (!r) return;
    const renamed = Object.keys(r.styles_renamed ?? {}).length;
    say(`A ${r.seconds.toFixed(1)} s cut with this look${save() ? ` · saved as “${name().trim()}”` : ""}${renamed ? ` · ${renamed} style${renamed === 1 ? "" : "s"} added under the template's name` : ""}`);
    close();
    await openCut(project, r.cut);
  };
  return (
    <div class="keys-backdrop" onPointerDown={(e) => { if (e.target === e.currentTarget) close(); }}>
      <section class="keys-dialog cutlike" role="dialog" aria-modal="true" aria-label="New cut like this" data-testid="cutlike-sheet"
        onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); close(); } }}>
        <header>
          <h2>New cut like this</h2>
          <IconButton icon="x" label="Close" size="sm" onClick={() => close()} />
        </header>
        <Show when={(templates() ?? []).length > 0}>
          <label class="cutlike-from">
            <span class="muted small">Look from</span>
            <select aria-label="Look from" value={from()} onChange={(e) => setFrom(e.currentTarget.value)}>
              <option value="">This cut</option>
              <For each={templates() ?? []}>{(t) => <option value={t.name}>{t.name} ({t.framings.join(", ")})</option>}</For>
            </select>
            <Show when={from()}>
              <IconButton icon="x" label="Delete this saved look" size="sm" onClick={() => void attempt(() => cutLikeActions.remove(from())).then((r) => { if (r) { setFrom(""); void refetch(); } })} />
            </Show>
          </label>
        </Show>
        <input ref={input} type="search" aria-label="Find a moment" placeholder="Find a moment…" value={q()} onInput={(e) => setQ(e.currentTarget.value)} />
        <ul class="cutlike-list" role="listbox" aria-label="Moments">
          <For each={shown()} fallback={<li><Empty icon="sparkles" title="No moments with times yet" compact /></li>}>
            {(m) => (
              <li role="option" aria-selected={pick() === m.id} classList={{ on: pick() === m.id }} data-testid="cutlike-moment" onClick={() => setPick(m.id)}>
                <span class="mono muted">{clock(m.start_s ?? 0)}</span>
                <span class="cutlike-text">{m.text_hook || m.excerpt || m.quote.slice(0, 140)}</span>
              </li>
            )}
          </For>
        </ul>
        <div class="cutlike-foot">
          <Show when={!from()}>
            <label class="check">
              <input type="checkbox" data-testid="cutlike-save" checked={save()} onChange={(e) => setSave(e.currentTarget.checked)} /> Save this look as
            </label>
            <input type="text" aria-label="Template name" disabled={!save()} value={name()} maxLength={60} onInput={(e) => setName(e.currentTarget.value)} />
          </Show>
          <span class="spacer" />
          <Button variant="primary" icon="layers" data-testid="cutlike-make" disabled={!pick() || busy() || (!from() && save() && !name().trim())} onClick={() => void make()}>
            {busy() ? "Making…" : "Make the cut"}
          </Button>
        </div>
      </section>
    </div>
  );
}
