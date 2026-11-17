// ⌘K (U.14; SPEC §5.15): the app's actions by name — the UI's own buttons, ranked by match
// and how recently each was used (kept on this Mac); synonyms find the same action. It runs
// what a click would; it never lists Owner or agent-only commands (it holds no commands).
// "Developer: all commands" switches it to the core's catalogue, read only: every
// registered command with what it does, its scope and undo class and an example; Enter
// copies the call (for `gmclip` or an agent) and runs nothing.
import { createMemo, createResource, createSignal, For, onMount, Show } from "solid-js";
import { keyLabel } from "../platform";
import { actions as core, type CatalogCommand } from "../actions";
import { say } from "../store";

export interface PaletteAction {
  id: string;
  label: string;
  /** Other words that find it. */
  also?: string;
  keys?: string;
  /** The section it is listed under ("More panels"); its name finds it too. */
  section?: string;
  when?: () => boolean;
  run: () => void;
}

const RECENT = "gmclip.palette.recent";

function recent(): string[] {
  try {
    return JSON.parse(localStorage.getItem(RECENT) ?? "[]") as string[];
  } catch {
    return [];
  }
}

function remember(id: string) {
  try {
    localStorage.setItem(RECENT, JSON.stringify([id, ...recent().filter((x) => x !== id)].slice(0, 20)));
  } catch {
    // private storage: ranking just forgets
  }
}

/** Every word of the query starts a word of the label or synonyms (or is inside it). */
function score(a: PaletteAction, q: string): number {
  const hay = `${a.label} ${a.also ?? ""} ${a.section ?? ""}`.toLowerCase();
  const words = q.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return 1;
  let s = 0;
  for (const w of words) {
    if (new RegExp(`\\b${w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`).test(hay)) s += 2;
    else if (hay.includes(w)) s += 1;
    else return 0;
  }
  // the words in the name itself beat the same words among its synonyms: "History" is the
  // History dialog before the actions that mention history
  const label = a.label.toLowerCase();
  const query = words.join(" ");
  if (label === query) s += 4;
  else if (label.startsWith(query)) s += 2;
  else if (words.every((w) => label.includes(w))) s += 1;
  return s;
}

/** The catalogue row matches every word of the query (name, doc, scope or menu label). */
function devScore(c: CatalogCommand, q: string): number {
  return score({ id: c.name, label: c.name.replace(/_/g, " "), also: `${c.name} ${c.doc} ${c.scope} ${c.ui?.label ?? ""}`, run: () => undefined }, q);
}

export function Palette(props: { actions: PaletteAction[]; onClose: () => void }) {
  const [q, setQ] = createSignal("");
  const [at, setAt] = createSignal(0);
  const [dev, setDev] = createSignal(false);
  let input: HTMLInputElement | undefined;
  onMount(() => input?.focus());
  const [catalog] = createResource(dev, async (on) => (on ? (await core.commandsCatalog().catch(() => null))?.commands ?? [] : []));
  const devEntry: PaletteAction = {
    id: "developer-all-commands",
    label: "Developer: all commands",
    also: "registry api mcp cli agent schema reference",
    run: () => {
      setDev(true);
      setQ("");
      setAt(0);
      input?.focus();
    },
  };
  const devShown = createMemo(() =>
    (catalog() ?? [])
      .map((c) => ({ c, s: devScore(c, q()) }))
      .filter((x) => x.s > 0)
      .sort((x, y) => y.s - x.s || x.c.name.localeCompare(y.c.name))
      .map((x) => x.c)
      .slice(0, 50),
  );
  const copy = (c: CatalogCommand | undefined) => {
    if (!c) return;
    const text = JSON.stringify({ command: c.name, args: c.example ?? {} });
    void navigator.clipboard?.writeText(text).then(
      () => say(`Copied ${c.name}'s example call`),
      () => say(text),
    );
  };
  const shown = createMemo(() => {
    const r = recent();
    const rank = (a: PaletteAction) => {
      const i = r.indexOf(a.id);
      return i < 0 ? 0 : 20 - i;
    };
    return [...props.actions, devEntry]
      .filter((a) => !a.when || a.when())
      .map((a) => ({ a, s: score(a, q()) }))
      .filter((x) => x.s > 0)
      .sort((x, y) => y.s - x.s || rank(y.a) - rank(x.a) || x.a.label.localeCompare(y.a.label))
      .map((x) => x.a)
      .slice(0, 12);
  });
  const run = (a: PaletteAction | undefined) => {
    if (!a) return;
    remember(a.id);
    if (a === devEntry) return a.run();
    props.onClose();
    a.run();
  };
  const count = () => (dev() ? devShown().length : shown().length);
  return (
    <div class="keys-backdrop" onPointerDown={(e) => { if (e.target === e.currentTarget) props.onClose(); }}>
      <section class="keys-dialog palette" classList={{ "palette-dev": dev() }} role="dialog" aria-modal="true" aria-label="Command palette">
        <Show when={dev()}>
          <p class="palette-mode" data-testid="palette-dev">
            <b>Developer: all commands</b> — what agents and <code>gmclip</code> can call (read only). Enter copies an example call. Esc goes back.
          </p>
        </Show>
        <input
          ref={input}
          type="text"
          aria-label={dev() ? "Find a command" : "Find an action"}
          data-testid="palette-input"
          placeholder={dev() ? "Find a command by name or what it does…" : "Find an action…"}
          value={q()}
          onInput={(e) => { setQ(e.currentTarget.value); setAt(0); }}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") { e.preventDefault(); setAt(Math.min(at() + 1, count() - 1)); }
            else if (e.key === "ArrowUp") { e.preventDefault(); setAt(Math.max(at() - 1, 0)); }
            else if (e.key === "Enter") { e.preventDefault(); if (dev()) copy(devShown()[at()]); else run(shown()[at()]); }
            else if (e.key === "Escape") {
              e.preventDefault();
              if (dev()) { setDev(false); setQ(""); setAt(0); } else props.onClose();
            }
          }}
        />
        <Show when={dev()}>
          <ul class="palette-list palette-cmds" role="listbox" aria-label="Commands">
            <For each={devShown()} fallback={<li class="muted small">{catalog.loading ? "Loading the commands…" : "No command by that name."}</li>}>
              {(c, i) => (
                <li role="option" aria-selected={i() === at()} classList={{ on: i() === at() }} data-testid="palette-cmd" data-name={c.name}
                  onPointerDown={(e) => { e.preventDefault(); setAt(i()); copy(c); }}>
                  <span class="pc-name">{c.name}</span>
                  <span class="pc-doc">{c.doc}</span>
                  <span class="pc-tags">
                    {c.scope} · {c.undo}{c.exec === "job" ? " · job" : ""}{c.human_only ? " · you only" : ""}{c.ui ? ` · ${c.ui.menu} › ${c.ui.label}` : ""}
                  </span>
                </li>
              )}
            </For>
          </ul>
        </Show>
        <ul class="palette-list" role="listbox" aria-label="Actions" hidden={dev()}>
          <For each={shown()} fallback={<li class="muted small">Nothing by that name.</li>}>
            {(a, i) => (
              <li role="option" aria-selected={i() === at()} classList={{ on: i() === at() }} data-testid="palette-item" onPointerDown={(e) => { e.preventDefault(); run(a); }}>
                <span>{a.label}</span>
                {a.section ? <span class="palette-section" data-testid="palette-section">{a.section}</span> : null}
                {a.keys ? <kbd>{keyLabel(a.keys)}</kbd> : null}
              </li>
            )}
          </For>
        </ul>
      </section>
    </div>
  );
}
