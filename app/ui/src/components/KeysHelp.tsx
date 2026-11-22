// The keys list (?): a modal dialog that traps focus;
// Esc or ? closes it, a click on the backdrop closes it, focus returns to the opener.
// The twelve keys an edit uses most come first; the rest are behind "Show all", and the
// search field finds any by its key or its words (AJ 2026-11-22). Each row is one line
// (the whole description in its tooltip), in one key column across every group.
import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { GROUPS, type KeyRow } from "../keys";
import { keyLabel } from "../platform";

/** The essentials, by a row's first key (in this order). */
const ESSENTIAL = ["Space", "J", "L", "←", "S", "I", "D", "⌫", "⌘Z", "⌘⇧Z", "⌘E", "⌘K"];

function Keys(props: { r: KeyRow }) {
  return <For each={props.r.keys}>{(k) => (/ [a-z]/.test(k) ? <span class="key-gesture">{keyLabel(k)}</span> : <kbd>{keyLabel(k)}</kbd>)}</For>;
}

function Rows(props: { rows: KeyRow[] }) {
  return (
    <dl>
      <For each={props.rows}>
        {(r) => (
          <>
            <dt><Keys r={r} /></dt>
            <dd data-tip={keyLabel(r.what)}>{keyLabel(r.what)}</dd>
          </>
        )}
      </For>
    </dl>
  );
}

export function KeysHelp(props: { onClose: () => void }) {
  let close!: HTMLButtonElement;
  let search!: HTMLInputElement;
  const opener = document.activeElement as HTMLElement | null;
  const [all, setAll] = createSignal(false);
  const [q, setQ] = createSignal("");
  const essentials = createMemo(() => {
    const seen = new Set<string>();
    const out: KeyRow[] = [];
    for (const k of ESSENTIAL)
      for (const g of GROUPS)
        for (const r of g.rows)
          if (r.keys[0] === k && !seen.has(k)) {
            seen.add(k);
            out.push(r);
          }
    return out;
  });
  const matches = (r: KeyRow) => {
    const x = q().trim().toLowerCase();
    return !x || r.what.toLowerCase().includes(x) || r.keys.some((k) => keyLabel(k).toLowerCase() === x || k.toLowerCase() === x);
  };
  onMount(() => {
    close.focus();
    const key = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey) return;
      const inSearch = e.target === search;
      if (e.key === "Escape" || (e.key === "?" && !inSearch)) {
        e.preventDefault();
        e.stopPropagation();
        if (inSearch && q()) return void setQ("");
        props.onClose();
        return;
      }
      if (["ArrowUp", "ArrowDown", "PageUp", "PageDown"].includes(e.key)) return;
      // the search field types; Tab moves between it, Show all and Close
      if (inSearch && e.key !== "Tab") {
        e.stopPropagation();
        return;
      }
      if (e.key === "Tab") return;
      if (e.key === "Enter" || e.key === " ") return;
      e.preventDefault();
      e.stopPropagation();
      // a letter typed anywhere searches
      if (e.key.length === 1) {
        search.focus();
        setQ(q() + e.key);
      }
    };
    addEventListener("keydown", key, true);
    onCleanup(() => {
      removeEventListener("keydown", key, true);
      opener?.focus?.();
    });
  });
  return (
    <div class="keys-backdrop" onPointerDown={(e) => { if (e.target === e.currentTarget) props.onClose(); }}>
      <section class="keys-dialog" role="dialog" aria-modal="true" aria-label="Keyboard shortcuts">
        <header>
          <h2>Keyboard shortcuts</h2>
          <input ref={search} type="search" class="keys-search" aria-label="Find a key" placeholder="Find a key or what it does…" data-testid="keys-search" value={q()} onInput={(e) => setQ(e.currentTarget.value)} />
          <button type="button" class="ghost" ref={close} onClick={() => props.onClose()} aria-label="Close">✕</button>
        </header>
        <div class="keys-groups">
          <Show when={!q().trim()}>
            <div class="keys-group keys-essentials" data-testid="keys-essentials">
              <h3>The essentials</h3>
              <Rows rows={essentials()} />
            </div>
          </Show>
          <Show when={!q().trim()}>
            <button type="button" class="ghost small keys-all" data-testid="keys-all" aria-expanded={all()} onClick={() => setAll(!all())}>
              {all() ? "Show only the essentials" : "Show all keys"}
            </button>
          </Show>
          {/* every group stays in the page (read by find-in-page and tests); hidden until
              asked for, or filtered by the search */}
          <For each={GROUPS}>
            {(g) => (
              <div class="keys-group" hidden={q().trim() ? !g.rows.some(matches) : !all()}>
                <h3>{g.name}</h3>
                <Rows rows={g.rows.filter((r) => matches(r))} />
              </div>
            )}
          </For>
          <Show when={q().trim() && !GROUPS.some((g) => g.rows.some(matches))}>
            <p class="muted">No key does “{q().trim()}”.</p>
          </Show>
        </div>
      </section>
    </div>
  );
}
