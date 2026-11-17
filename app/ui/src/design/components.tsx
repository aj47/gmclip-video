// gmclip's component set (design system). Small, unopinionated about logic: each one is a
// styled native element with the right role, focus ring and hover/press states, so the
// lanes' feature code keeps its own handlers and only swaps the element.
//
//   Button          text (+ icon) button: primary · secondary (default) · ghost · danger
//   IconButton      square icon-only button; `label` is its accessible name and tooltip
//   Toggle          IconButton with aria-pressed (an on/off tool)
//   Switch          a labelled on/off switch for settings rows
//   SegmentedControl  one-of-n, buttons with aria-pressed (16:9 · 9:16 · …)
//   Tabs            icon + label tabs that never overflow: icon-only when tight, then a
//                   "more" menu for what still does not fit
//   Menu            a button that opens a menu (items, checkboxes, separators, shortcuts)
//   Tooltip         `tip(label, keys)` spreads data-tip/data-kbd; one delegated tooltip
//                   layer (installTooltips) shows them — no per-element listeners
//   Chip            a small status chip: neutral · accent · success · warning · danger
//   Panel           a titled section with hairline header and optional actions
//   Toolbar         role=toolbar row with `ToolbarGroup` / `ToolbarSep`
//   Kbd             a key cap
//   Empty           one sentence + one action (SPEC §12.5)
//   Banner          an inline error / warning / info bar with actions
//   Sheet           a modal dialog over a dimmed window: title, close button, Esc, focus
//                   kept inside and given back to the opener; scrolls inside a small window
import { createEffect, createSignal, For, on, onCleanup, onMount, Show, splitProps, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { keyLabel } from "../platform";
import { Icon } from "./Icon";

type BtnBase = Omit<JSX.ButtonHTMLAttributes<HTMLButtonElement>, "type">;

/** data-tip / data-kbd for the shared tooltip layer. */
export function tip(label: string | undefined, keys?: string): { "data-tip"?: string; "data-kbd"?: string } {
  if (!label) return {};
  return keys ? { "data-tip": label, "data-kbd": keys } : { "data-tip": label };
}

export function Button(
  props: BtnBase & {
    variant?: "primary" | "secondary" | "ghost" | "danger";
    size?: "sm" | "md" | "lg";
    icon?: string;
    iconAfter?: string;
    tooltip?: string;
    keys?: string;
    type?: "button" | "submit";
  },
) {
  const [own, rest] = splitProps(props, ["variant", "size", "icon", "iconAfter", "tooltip", "keys", "class", "children", "type"]);
  return (
    <button
      type={own.type ?? "button"}
      class={`btn btn-${own.variant ?? "secondary"} btn-${own.size ?? "md"}${own.class ? ` ${own.class}` : ""}`}
      {...tip(own.tooltip, own.keys)}
      {...rest}
    >
      <Show when={own.icon}>{(i) => <Icon name={i()} size={own.size === "lg" ? 18 : 15} />}</Show>
      <Show when={own.children}>
        <span class="btn-label">{own.children}</span>
      </Show>
      <Show when={own.iconAfter}>{(i) => <Icon name={i()} size={14} />}</Show>
    </button>
  );
}

export function IconButton(
  props: BtnBase & {
    icon: string;
    label: string;
    keys?: string;
    size?: "sm" | "md" | "lg";
    variant?: "ghost" | "secondary" | "primary";
    pressed?: boolean;
    /** No tooltip (the label still names it). */
    quiet?: boolean;
    /** Tooltip text when it should say more than the name. */
    hint?: string;
  },
) {
  const [own, rest] = splitProps(props, ["icon", "label", "keys", "size", "variant", "pressed", "quiet", "class", "hint"]);
  const px = () => (own.size === "lg" ? 20 : own.size === "sm" ? 14 : 16);
  return (
    <button
      type="button"
      class={`ibtn ibtn-${own.size ?? "md"} btn-${own.variant ?? "ghost"}${own.class ? ` ${own.class}` : ""}`}
      aria-label={own.label}
      aria-pressed={own.pressed === undefined ? undefined : own.pressed}
      {...(own.quiet ? {} : tip(own.hint ?? own.label, own.keys))}
      {...rest}
    >
      <Icon name={own.icon} size={px()} />
    </button>
  );
}

/** An on/off tool: an icon button whose pressed state is the setting. */
export function Toggle(props: Omit<BtnBase, "onChange"> & { icon: string; label: string; hint?: string; keys?: string; on: boolean; onChange: (on: boolean) => void; size?: "sm" | "md" | "lg" }) {
  const [own, rest] = splitProps(props, ["icon", "label", "hint", "keys", "on", "onChange", "size", "class"]);
  return (
    <IconButton
      icon={own.icon}
      label={own.label}
      hint={own.hint}
      keys={own.keys}
      size={own.size}
      pressed={own.on}
      class={`toggle${own.class ? ` ${own.class}` : ""}`}
      onClick={() => own.onChange(!own.on)}
      {...rest}
    />
  );
}

export function Switch(props: { label: JSX.Element; on: boolean; onChange: (on: boolean) => void; hint?: string; testid?: string; disabled?: boolean }) {
  return (
    <label class="switch-row" {...tip(props.hint)}>
      <span class="switch-label">{props.label}</span>
      <input
        type="checkbox"
        role="switch"
        class="switch"
        checked={props.on}
        disabled={props.disabled}
        data-testid={props.testid}
        data-on={props.on ? "1" : "0"}
        onChange={(e) => props.onChange(e.currentTarget.checked)}
      />
    </label>
  );
}

export interface SegOption<T extends string> {
  value: T;
  label: string;
  icon?: string;
  hint?: string;
}

export function SegmentedControl<T extends string>(props: { options: SegOption<T>[]; value: T | undefined; onChange: (v: T) => void; label: string; size?: "sm" | "md"; testid?: string }) {
  return (
    <span class={`seg seg-${props.size ?? "md"}`} role="group" aria-label={props.label} data-testid={props.testid}>
      <For each={props.options}>
        {(o) => (
          <button type="button" aria-label={o.label} aria-pressed={props.value === o.value} {...tip(o.hint)} onClick={() => props.onChange(o.value)}>
            <Show when={o.icon}>{(i) => <Icon name={i()} size={13} />}</Show>
            <span>{o.label}</span>
          </button>
        )}
      </For>
    </span>
  );
}

export interface TabItem<T extends string> {
  id: T;
  label: string;
  icon: string;
  badge?: () => string | number | null | undefined;
}

/** Tabs that never overflow: full labels when they fit, else icons (the selected tab keeps
 * its label), else the ones past the edge go into a "more" menu. Each tab's accessible
 * name is its label in every mode. */
export function Tabs<T extends string>(props: { items: TabItem<T>[]; value: T; onChange: (v: T) => void; label: string; testid?: string }) {
  let bar!: HTMLDivElement;
  const [mode, setMode] = createSignal<"full" | "compact">("full");
  const [hidden, setHidden] = createSignal<T[]>([]);
  let fullWidth = 0;
  /** Compact tab widths (px) by id, remembered while a tab sits in "more". */
  const widths = new Map<string, number>();
  const measure = () => {
    if (!bar) return;
    const avail = bar.clientWidth;
    if (mode() === "full") {
      if (bar.scrollWidth > avail + 1) {
        fullWidth = bar.scrollWidth;
        setMode("compact");
        queueMicrotask(measure);
      }
      return;
    }
    if (fullWidth && avail >= fullWidth) {
      setHidden([]);
      setMode("full");
      return;
    }
    for (const t of bar.querySelectorAll<HTMLElement>("[role=tab]")) widths.set(t.dataset.tab!, t.offsetWidth + 2);
    const w = (id: T) => widths.get(id) ?? 36;
    let total = props.items.reduce((n, t) => n + w(t.id), 0);
    const out: T[] = [];
    if (total > avail) {
      total += 36; // the "more" button
      for (const t of [...props.items].reverse()) {
        if (total <= avail) break;
        if (t.id === props.value) continue;
        out.unshift(t.id);
        total -= w(t.id);
      }
    }
    const prev = hidden();
    if (out.length !== prev.length || out.some((x, i) => x !== prev[i])) setHidden(out);
  };
  onMount(() => {
    const ro = new ResizeObserver(() => measure());
    ro.observe(bar);
    onCleanup(() => ro.disconnect());
    measure();
  });
  // a tab picked from "more" shows in the bar; the next measure moves another out
  createEffect(on(() => props.value, () => queueMicrotask(measure), { defer: true }));
  const shown = () => props.items.filter((t) => !hidden().includes(t.id) || t.id === props.value);
  const more = () => props.items.filter((t) => hidden().includes(t.id) && t.id !== props.value);
  return (
    <div class="tabs" data-mode={mode()} data-testid={props.testid}>
      <div class="tabs-list" role="tablist" aria-label={props.label} ref={bar}>
        <For each={shown()}>
          {(t) => (
            <button
              type="button"
              role="tab"
              class="tab"
              data-tab={t.id}
              aria-label={t.label}
              aria-selected={props.value === t.id}
              tabindex={0}
              {...(mode() === "compact" && props.value !== t.id ? tip(t.label) : {})}
              onClick={() => props.onChange(t.id)}
            >
              <Icon name={t.icon} size={15} />
              <span class="tab-label">{t.label}</span>
              <Show when={t.badge?.()}>{(b) => <span class="tab-badge">{b()}</span>}</Show>
            </button>
          )}
        </For>
      </div>
      <Show when={more().length}>
        <Menu
          label="More panels"
          icon="more"
          items={more().map((t) => ({ label: t.label, icon: t.icon, onSelect: () => props.onChange(t.id) }))}
        />
      </Show>
    </div>
  );
}

export interface MenuItem {
  label: string;
  icon?: string;
  keys?: string;
  checked?: boolean;
  disabled?: boolean;
  danger?: boolean;
  testid?: string;
  onSelect?: () => void;
  separator?: boolean;
}

/** A button that opens a menu below it (Esc / outside click closes; arrows move). */
export function Menu(props: { label: string; icon?: string; text?: string; items: MenuItem[] | (() => MenuItem[]); align?: "start" | "end"; variant?: "ghost" | "secondary"; testid?: string; keys?: string }) {
  const [open, setOpen] = createSignal(false);
  let root!: HTMLDivElement;
  let btn!: HTMLButtonElement;
  const items = () => (typeof props.items === "function" ? props.items() : props.items);
  const close = (focus = true) => {
    setOpen(false);
    if (focus) btn?.focus();
  };
  const outside = (e: PointerEvent) => {
    if (open() && !root.contains(e.target as Node)) close(false);
  };
  onMount(() => {
    addEventListener("pointerdown", outside, true);
    onCleanup(() => removeEventListener("pointerdown", outside, true));
  });
  const focusItem = (d: number) => {
    const els = [...root.querySelectorAll<HTMLElement>("[role^=menuitem]:not([aria-disabled=true])")];
    const at = els.indexOf(document.activeElement as HTMLElement);
    els[(at + d + els.length) % els.length]?.focus();
  };
  const onKey = (e: KeyboardEvent) => {
    if (!open()) return;
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      close();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      focusItem(1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      focusItem(-1);
    }
  };
  return (
    <div class="menu-root" ref={root} onKeyDown={onKey}>
      <button
        type="button"
        ref={btn}
        class={props.text ? `btn btn-${props.variant ?? "ghost"} btn-md menu-trigger` : `ibtn ibtn-md btn-${props.variant ?? "ghost"} menu-trigger`}
        aria-haspopup="menu"
        aria-expanded={open()}
        aria-label={props.text ? undefined : props.label}
        data-testid={props.testid}
        {...(props.text ? {} : tip(props.label, props.keys))}
        onClick={() => {
          setOpen(!open());
          if (open()) queueMicrotask(() => focusItem(1));
        }}
      >
        <Show when={props.icon}>{(i) => <Icon name={i()} size={props.text ? 15 : 16} />}</Show>
        <Show when={props.text}>
          <span class="btn-label">{props.text}</span>
          <Icon name="chevron-down" size={13} class="menu-chevron" />
        </Show>
      </button>
      <Show when={open()}>
        <div class={`menu menu-${props.align ?? "end"}`} role="menu" aria-label={props.label}>
          <For each={items()}>
            {(it) =>
              it.separator ? (
                <div class="menu-sep" role="separator" />
              ) : (
                <button
                  type="button"
                  class={`menu-item${it.danger ? " danger" : ""}`}
                  role={it.checked === undefined ? "menuitem" : "menuitemcheckbox"}
                  aria-checked={it.checked === undefined ? undefined : it.checked}
                  aria-disabled={it.disabled ? "true" : undefined}
                  data-testid={it.testid}
                  tabindex="-1"
                  onClick={() => {
                    if (it.disabled) return;
                    close();
                    it.onSelect?.();
                  }}
                >
                  <span class="menu-check">{it.checked ? <Icon name="check" size={13} /> : null}</span>
                  <Show when={it.icon}>{(i) => <Icon name={i()} size={15} />}</Show>
                  <span class="menu-text">{it.label}</span>
                  <Show when={it.keys}>{(k) => <span class="menu-keys">{keyLabel(k())}</span>}</Show>
                </button>
              )
            }
          </For>
        </div>
      </Show>
    </div>
  );
}

export function Chip(props: { tone?: "neutral" | "accent" | "success" | "warning" | "danger"; icon?: string; dot?: boolean; children: JSX.Element; tooltip?: string; testid?: string; class?: string; role?: "status" | "alert" }) {
  return (
    <span class={`chip chip-${props.tone ?? "neutral"}${props.class ? ` ${props.class}` : ""}`} data-testid={props.testid} role={props.role} {...tip(props.tooltip)}>
      <Show when={props.dot}>
        <span class="chip-dot" aria-hidden="true" />
      </Show>
      <Show when={props.icon}>{(i) => <Icon name={i()} size={12} />}</Show>
      <span>{props.children}</span>
    </span>
  );
}

export function Panel(props: { title?: JSX.Element; actions?: JSX.Element; children: JSX.Element; class?: string; label?: string; testid?: string }) {
  return (
    <section class={`panel${props.class ? ` ${props.class}` : ""}`} aria-label={props.label} data-testid={props.testid}>
      <Show when={props.title || props.actions}>
        <header class="panel-head">
          <h2 class="panel-title">{props.title}</h2>
          <span class="panel-actions">{props.actions}</span>
        </header>
      </Show>
      <div class="panel-body">{props.children}</div>
    </section>
  );
}

export function Toolbar(props: { label: string; children: JSX.Element; class?: string; testid?: string }) {
  return (
    <div class={`toolbar${props.class ? ` ${props.class}` : ""}`} role="toolbar" aria-label={props.label} data-testid={props.testid}>
      {props.children}
    </div>
  );
}
export function ToolbarGroup(props: { children: JSX.Element; class?: string }) {
  return <div class={`toolbar-group${props.class ? ` ${props.class}` : ""}`}>{props.children}</div>;
}
export function ToolbarSep() {
  return <span class="toolbar-sep" role="separator" aria-orientation="vertical" />;
}

export function Kbd(props: { children: JSX.Element }) {
  return <kbd class="kbd">{props.children}</kbd>;
}

export function Empty(props: { icon?: string; title: JSX.Element; children?: JSX.Element; action?: JSX.Element; testid?: string; compact?: boolean }) {
  return (
    <div class={`empty-state${props.compact ? " compact" : ""}`} data-testid={props.testid}>
      <Show when={props.icon}>{(i) => <span class="empty-icon"><Icon name={i()} size={props.compact ? 18 : 22} /></span>}</Show>
      <p class="empty-title">{props.title}</p>
      <Show when={props.children}>
        <p class="empty-text">{props.children}</p>
      </Show>
      <Show when={props.action}>{props.action}</Show>
    </div>
  );
}

export function Banner(props: { tone?: "danger" | "warning" | "info"; children: JSX.Element; actions?: JSX.Element; onDismiss?: () => void; testid?: string; role?: "status" | "alert" }) {
  const icon = () => (props.tone === "danger" ? "circle-alert" : props.tone === "warning" ? "triangle-alert" : "circle-check");
  return (
    <div class={`banner banner-${props.tone ?? "info"}`} role={props.role ?? (props.tone === "danger" ? "alert" : "status")} data-testid={props.testid}>
      <Icon name={icon()} size={15} />
      <span class="banner-text">{props.children}</span>
      <Show when={props.actions}>
        <span class="banner-actions">{props.actions}</span>
      </Show>
      <Show when={props.onDismiss}>
        <IconButton icon="x" label="Dismiss" size="sm" quiet onClick={() => props.onDismiss?.()} />
      </Show>
    </div>
  );
}

const FOCUSABLE = 'button:not([disabled]), [href], input:not([disabled]):not([type="hidden"]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"]), summary';

/** A modal sheet (every sheet the app opens looks and behaves the same): a title with a
 *  close button, an optional line under it and actions beside it; Esc or a click on the
 *  dim backdrop closes it; Tab stays inside; focus goes back where it came from; a plain
 *  key typed in it never reaches the cut's own keys. It scrolls inside the window down to
 *  480 × 360. */
export function Sheet(props: {
  label: string;
  title?: JSX.Element;
  sub?: JSX.Element;
  actions?: JSX.Element;
  onClose: () => void;
  children: JSX.Element;
  class?: string;
  testid?: string;
  /** Wider sheets (side by side columns). */
  size?: "md" | "lg";
}) {
  let box!: HTMLElement;
  const opener = document.activeElement as HTMLElement | null;
  onMount(() => {
    // the sheet itself takes the focus, unless a field in it already asked for it
    queueMicrotask(() => {
      if (box && !box.contains(document.activeElement)) box.focus({ preventScroll: true });
    });
    // Esc with the focus outside it (a click on the backdrop's blur, a control removed)
    const stray = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || box.contains(e.target as Node)) return;
      const sheets = document.querySelectorAll(".sheet");
      if (sheets[sheets.length - 1] !== box) return;
      e.preventDefault();
      props.onClose();
    };
    addEventListener("keydown", stray);
    // the focus never settles outside the top sheet (Tab past an edge the list missed, a
    // click on the page behind the backdrop's blur)
    const guard = (e: FocusEvent) => {
      if (box.contains(e.target as Node)) return;
      const sheets = document.querySelectorAll(".sheet");
      if (sheets[sheets.length - 1] !== box) return;
      const els = tabbables();
      (els[0] ?? box).focus({ preventScroll: true });
    };
    document.addEventListener("focusin", guard);
    onCleanup(() => {
      removeEventListener("keydown", stray);
      document.removeEventListener("focusin", guard);
    });
  });
  onCleanup(() => {
    if (opener?.isConnected) opener.focus({ preventScroll: true });
  });
  const tabbables = () =>
    [...box.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((x) => x.tabIndex >= 0 && x.getClientRects().length > 0 && !x.closest("[inert]"));
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      props.onClose();
      return;
    }
    if (e.key === "Tab") {
      const els = tabbables();
      if (!els.length) return;
      const first = els[0]!;
      const last = els[els.length - 1]!;
      if (e.shiftKey && (document.activeElement === first || document.activeElement === box)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
    // the cut's keys (s, d, space…) are for the cut, not for a sheet over it
    if (!e.metaKey && !e.ctrlKey) e.stopPropagation();
  };
  return (
    <Portal>
      <div class="sheet-back" role="presentation" onPointerDown={(e) => { if (e.target === e.currentTarget) props.onClose(); }}>
        <section
          ref={box}
          class={`sheet sheet-${props.size ?? "md"}${props.class ? ` ${props.class}` : ""}`}
          role="dialog"
          aria-modal="true"
          aria-label={props.label}
          data-testid={props.testid}
          tabindex="-1"
          onKeyDown={onKey}
          onFocusOut={() => {
            // a focused control re-rendered away (a list refreshing under it): the sheet
            // keeps the focus instead of dropping it on the page behind
            setTimeout(() => {
              if (box.isConnected && (document.activeElement === document.body || !document.activeElement)) box.focus({ preventScroll: true });
            }, 0);
          }}
        >
          <header class="sheet-head">
            <h2>{props.title ?? props.label}</h2>
            <Show when={props.actions}>
              <span class="sheet-actions">{props.actions}</span>
            </Show>
            <IconButton icon="x" label="Close" keys="Esc" size="sm" class="sheet-close" onClick={() => props.onClose()} />
          </header>
          <Show when={props.sub}>
            <p class="sheet-sub muted small">{props.sub}</p>
          </Show>
          {props.children}
        </section>
      </div>
    </Portal>
  );
}

/** The one tooltip layer: elements with `data-tip` (and optional `data-kbd`) show a
 * styled tooltip after a short hover, or at once on keyboard focus. Delegated from the
 * window, so nothing per element and no DOM churn while the pointer moves. */
export function installTooltips(): () => void {
  const el = document.createElement("div");
  el.className = "tooltip";
  el.setAttribute("role", "tooltip");
  el.hidden = true;
  const text = document.createElement("span");
  const keys = document.createElement("kbd");
  keys.className = "kbd tooltip-kbd";
  el.append(text, keys);
  document.body.append(el);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let target: HTMLElement | null = null;
  let warm = 0; // a tooltip shown recently: the next one shows without the delay
  const place = (t: HTMLElement) => {
    const r = t.getBoundingClientRect();
    el.hidden = false;
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    let x = r.left + r.width / 2 - w / 2;
    x = Math.max(6, Math.min(innerWidth - w - 6, x));
    let y = r.bottom + 6;
    if (y + h > innerHeight - 6) y = r.top - h - 6;
    el.style.transform = `translate(${Math.round(x)}px, ${Math.round(y)}px)`;
  };
  const show = (t: HTMLElement) => {
    // key names for this platform (⌘ on a Mac, Ctrl elsewhere), in the text too
    text.textContent = keyLabel(t.dataset.tip ?? "");
    keys.textContent = keyLabel(t.dataset.kbd ?? "");
    keys.hidden = !t.dataset.kbd;
    place(t);
    el.dataset.on = "1";
    warm = performance.now();
  };
  const hide = () => {
    clearTimeout(timer);
    target = null;
    el.hidden = true;
    delete el.dataset.on;
  };
  const over = (e: PointerEvent) => {
    const t = (e.target as HTMLElement | null)?.closest?.<HTMLElement>("[data-tip]") ?? null;
    if (t === target) return;
    hide();
    if (!t || e.buttons) return;
    target = t;
    const delay = performance.now() - warm < 600 ? 60 : 480;
    timer = setTimeout(() => target === t && t.isConnected && show(t), delay);
  };
  const focus = (e: FocusEvent) => {
    const t = (e.target as HTMLElement | null)?.closest?.<HTMLElement>("[data-tip]") ?? null;
    if (!t || !t.matches(":focus-visible")) return;
    hide();
    target = t;
    show(t);
  };
  const out = (e: PointerEvent) => {
    if (!(e.relatedTarget as HTMLElement | null)?.closest?.("[data-tip]")) hide();
  };
  addEventListener("pointerover", over, true);
  addEventListener("pointerout", out, true);
  addEventListener("focusin", focus, true);
  addEventListener("focusout", hide, true);
  addEventListener("pointerdown", hide, true);
  addEventListener("keydown", hide, true);
  addEventListener("scroll", hide, true);
  return () => {
    hide();
    el.remove();
    removeEventListener("pointerover", over, true);
    removeEventListener("pointerout", out, true);
    removeEventListener("focusin", focus, true);
    removeEventListener("focusout", hide, true);
    removeEventListener("pointerdown", hide, true);
    removeEventListener("keydown", hide, true);
    removeEventListener("scroll", hide, true);
  };
}
