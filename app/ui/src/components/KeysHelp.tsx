// The keys list (?): a modal dialog that traps focus;
// Esc or ? closes it, a click on the backdrop closes it, focus returns to the opener.
import { For, onCleanup, onMount } from "solid-js";
import { GROUPS } from "../keys";
import { keyLabel } from "../platform";

export function KeysHelp(props: { onClose: () => void }) {
  let close!: HTMLButtonElement;
  const opener = document.activeElement as HTMLElement | null;
  onMount(() => {
    close.focus();
    const key = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey) return;
      if (e.key === "Escape" || e.key === "?") {
        e.preventDefault();
        e.stopPropagation();
        props.onClose();
        return;
      }
      if (["ArrowUp", "ArrowDown", "PageUp", "PageDown"].includes(e.key)) return;
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Tab") close.focus();
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
          <button type="button" class="ghost" ref={close} onClick={() => props.onClose()} aria-label="Close">✕</button>
        </header>
        <div class="keys-groups">
          <For each={GROUPS}>
            {(g) => (
              <div class="keys-group">
                <h3>{g.name}</h3>
                <dl>
                  <For each={g.rows}>
                    {(r) => (
                      <>
                        <dt><For each={r.keys}>{(k) => (/ [a-z]/.test(k) ? <span class="key-gesture">{keyLabel(k)}</span> : <kbd>{keyLabel(k)}</kbd>)}</For></dt>
                        <dd>{keyLabel(r.what)}</dd>
                      </>
                    )}
                  </For>
                </dl>
              </div>
            )}
          </For>
        </div>
      </section>
    </div>
  );
}
