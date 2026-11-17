// The brand kit (inspector): name, handle, face, colours, logo and the caption style new
// cuts start with; saved in the project and kept as my kit (the profile's library). Its
// templates insert at the playhead; a lower third and an end card come from it.
import { createEffect, createResource, createSignal, For, Show } from "solid-js";
import { actions, type BrandColours, type BrandView } from "../actions";
import { attempt, cutView, viewer } from "../store";
import { ImageThumb } from "./OverlayList";
import { Button, Panel, Switch, tip } from "../design/components";
import "../look.css";

const COLOURS: [keyof BrandColours, string][] = [
  ["primary", "Primary"],
  ["accent", "Accent"],
  ["text", "Text"],
  ["background", "Background"],
];

async function b64(file: File): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

export function BrandKit() {
  const [open, setOpen] = createSignal(false);
  const key = () => {
    const v = cutView();
    return open() && v ? { p: v.project, rev: v.rev } : null;
  };
  const [view, { mutate }] = createResource(key, async (k) => (await attempt(() => actions.brandKit(k.p))) ?? null);
  const [name, setName] = createSignal("");
  const [handle, setHandle] = createSignal("");
  const [face, setFace] = createSignal("");
  const [preset, setPreset] = createSignal("");
  const [colours, setColours] = createSignal<BrandColours>({ primary: "#0A84FF", accent: "#FFD400", text: "#FFFFFF", background: "#0C1118" });
  const [loopEnd, setLoopEnd] = createSignal(true);
  createEffect(() => {
    const k = view()?.kit;
    if (!k) return;
    setName(k.name);
    setHandle(k.handle);
    setFace(k.heading_font ?? "");
    setPreset(k.caption_preset ?? "");
    setColours(k.colours);
  });
  const set = (r: BrandView | null | undefined) => {
    if (r) mutate(r);
  };
  // the kit's face: the one picked, else the first offered (a lower third and an end card
  // need one; a kit saved without it enabled both and then refused them)
  const faceOr = () => face() || view()?.faces?.[0] || "";
  const save = () => {
    const v = cutView();
    if (!v) return;
    void attempt(() =>
      actions.brandKitSave(v.project, { name: name(), handle: handle(), colours: colours(), ...(faceOr() ? { heading_font: faceOr() } : {}), ...(preset() ? { caption_preset: preset() } : {}) }),
    ).then(set);
  };
  const frame = () => viewer()?.frame ?? 0;
  const templates = () => Object.entries(view()?.kit?.templates ?? {});
  return (
    <Panel
      class="look"
      label="Brand kit"
      title="Brand kit"
      actions={<Button variant="ghost" size="sm" icon="sparkles" data-testid="brand-toggle" aria-expanded={open()} onClick={() => setOpen(!open())}>{open() ? "Hide" : "Edit"}</Button>}
    >
      <Show when={open() && view()}>
        <Show when={!view()?.kit && (view()?.mine || view()?.profile)}>
          <Button variant="primary" size="sm" data-testid="brand-use-mine" tooltip="Name, colours, face, logo and templates from your profile"
            onClick={() => { const v = cutView(); if (v) void attempt(() => actions.brandKitUseMine(v.project)).then(set); }}>Use my brand kit</Button>
        </Show>
        <form class="look-form" onSubmit={(e) => { e.preventDefault(); save(); }}>
          <label for="brand-name">Name</label>
          <input id="brand-name" data-testid="brand-name" maxLength={80} value={name()} onInput={(e) => setName(e.currentTarget.value)} />
          <label for="brand-handle">Handle</label>
          <input id="brand-handle" data-testid="brand-handle" maxLength={80} placeholder="@handle" value={handle()} onInput={(e) => setHandle(e.currentTarget.value)} />
          <label for="brand-face">Face</label>
          <select id="brand-face" data-testid="brand-face" value={faceOr()} onChange={(e) => setFace(e.currentTarget.value)}>
            <For each={view()?.faces ?? []}>{(f) => <option value={f} selected={f === faceOr()}>{f}</option>}</For>
          </select>
          <label>Colours</label>
          <div class="brand-colours">
            <For each={COLOURS}>
              {([k, label]) => (
                <input type="color" class="look-swatch" aria-label={label} {...tip(label)} data-testid={`brand-colour-${k}`} value={colours()[k].toLowerCase()}
                  onInput={(e) => setColours({ ...colours(), [k]: e.currentTarget.value.toUpperCase() })} />
              )}
            </For>
          </div>
          <label for="brand-preset">Captions</label>
          <select id="brand-preset" data-testid="brand-preset" value={preset()} {...tip("The caption style new cuts start with")} onChange={(e) => setPreset(e.currentTarget.value)}>
            <option value="">—</option>
            <For each={view()?.presets ?? []}>{(p) => <option value={p.name} selected={p.name === preset()}>{p.label}</option>}</For>
          </select>
          <label>Logo</label>
          <div class="brand-logo">
            <Show when={view()?.kit?.logo}>{(l) => <ImageThumb project={cutView()!.project} asset={l().asset} />}</Show>
            <input type="file" accept="image/png,image/jpeg" aria-label="Logo file" data-testid="brand-logo"
              onChange={(e) => {
                const f = e.currentTarget.files?.[0];
                const v = cutView();
                if (f && v) void b64(f).then((d) => attempt(() => actions.brandLogoSet(v.project, f.name, d))).then(set);
              }} />
          </div>
          <span />
          <Button type="submit" variant="primary" size="sm" data-testid="brand-save" tooltip="Save in this project and as my brand kit">Save</Button>
        </form>
        <div class="look-inline" role="group" aria-label="From the brand kit">
          <Button size="sm" icon="type" data-testid="lower-third" disabled={!view()?.kit} tooltip="Name and handle at the playhead"
            onClick={() => { const v = cutView(); if (v) void attempt(() => actions.lowerThirdAdd(v.project, v.cut, frame())); }}>Lower third</Button>
          <Button size="sm" icon="flag" data-testid="end-card" disabled={!view()?.kit} tooltip={loopEnd() ? "Logo and handle over the last seconds; the last part ends on the opening layout" : "A full card over the last seconds"}
            onClick={() => { const v = cutView(); if (v) void attempt(() => actions.endCardAdd(v.project, v.cut, loopEnd())); }}>End card</Button>
          <Switch label="Loop" testid="end-card-loop" on={loopEnd()} hint="End on the opening layout" onChange={setLoopEnd} />
        </div>
        <Show when={templates().length}>
          <div class="look-inline" role="group" aria-label="Templates">
            <For each={templates()}>
              {([n]) => (
                <Button variant="ghost" size="sm" icon="plus" data-testid="template-insert" data-name={n} tooltip={`Insert “${n}” at the playhead`}
                  onClick={() => { const v = cutView(); if (v) void attempt(() => actions.templateInsert(v.project, v.cut, n, frame())); }}>{n}</Button>
              )}
            </For>
          </div>
        </Show>
      </Show>
    </Panel>
  );
}
