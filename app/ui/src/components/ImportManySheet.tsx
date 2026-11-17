// The import sheet for several videos (AJ 2026-10-15: "i should be able to load multiple
// video files into one project"; SPEC §5.1): each file's capability line and, for a file
// with more than one audio track, which track is its mic; "Are these the same recording?"
// for two files whose lengths agree within 10 % (yes by default: one video, the bigger
// picture with the other's sound, transcribed once); and, opening from Home, one project
// with all of them (the default) or a project each.
import { baseName } from "../platform";
import { createSignal, For, Show } from "solid-js";
import type { AddFile, ImportMany } from "../actions";
import { Button, SegmentedControl } from "../design/components";

const mmss = (s: number) => {
  const t = Math.round(s);
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const sec = String(t % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
};

export interface ManyAnswer {
  files: AddFile[];
  same: [number, number][];
  oneProject: boolean;
  /** The one project's name (absent: "<first> + N more"). */
  name?: string;
}

export function ImportManySheet(props: {
  sheet: ImportMany;
  /** `open`: from Home (a new project); `add`: to the project on screen. */
  mode: "open" | "add";
  project?: string;
  onDone: (a: ManyAnswer) => void;
  /** add mode: open them as a new project instead. */
  onElsewhere?: () => void;
  onCancel: () => void;
}) {
  const files = () => props.sheet.files;
  const [voices, setVoices] = createSignal<(number | null)[]>(files().map((f) => (f.ask ? (f.suggested ?? f.tracks[0]?.index ?? null) : null)));
  const [same, setSame] = createSignal<boolean[]>(props.sheet.pairs.map(() => true));
  const [one, setOne] = createSignal<"one" | "each">("one");
  const [name, setName] = createSignal("");
  const defaultName = () => `${files()[0]?.name ?? "Video"} + ${files().length - 1} more`;
  // a file is the same recording as one other at most: the first "yes" pair wins
  const pairs = (): [number, number][] => {
    const used = new Set<number>();
    const out: [number, number][] = [];
    props.sheet.pairs.forEach((p, k) => {
      if (!same()[k] || used.has(p[0]) || used.has(p[1])) return;
      used.add(p[0]);
      used.add(p[1]);
      out.push(p);
    });
    return out;
  };
  const go = () =>
    props.onDone({
      files: files().map((f, i) => ({ path: f.path, voice: voices()[i] ?? null })),
      same: props.mode === "add" || one() === "one" ? pairs() : [],
      oneProject: one() === "one",
      name: props.mode === "open" && one() === "one" && name().trim() ? name().trim() : undefined,
    });
  const title = () =>
    props.mode === "add"
      ? `Add ${files().length === 1 ? `“${files()[0]?.name ?? "video"}”` : `${files().length} videos`} to ${props.project ? `“${props.project}”` : "this project"}`
      : `Open ${files().length} videos`;
  return (
    <div class="keys-backdrop" onPointerDown={(e) => { if (e.target === e.currentTarget) props.onCancel(); }}>
      <section class="keys-dialog import-sheet import-many" role="dialog" aria-modal="true" aria-label="Import videos">
        <header>
          <h2>{title()}</h2>
          <button type="button" class="ghost" onClick={() => props.onCancel()} aria-label="Close">✕</button>
        </header>
        <ul class="import-files" aria-label="Videos">
          <For each={files()}>
            {(f, i) => (
              <li class="import-file" data-testid="import-file">
                <div class="import-file-head">
                  <b class="import-name">{f.name}</b>
                  <span class="muted small tabular">{mmss(f.duration_s)}</span>
                </div>
                <div class="cap-line" data-level={f.video.level}>
                  <span class="cap-dot" aria-hidden="true" />
                  <span>{f.video.text}</span>
                  <Show when={f.video.fix}><p class="fix">{f.video.fix}</p></Show>
                </div>
                <Show when={f.ask}>
                  <div class="import-mic" role="radiogroup" aria-label={`Which track is the mic of ${f.name}?`}>
                    <span class="small">Mic:</span>
                    <For each={f.tracks}>
                      {(t) => (
                        <label class="check small" data-tip={t.speech != null ? `speech ${Math.round(t.speech * 100)}% · ${t.level_db} dB` : t.line.text}>
                          <input type="radio" name={`mic-${i()}`} checked={voices()[i()] === t.index} onChange={() => setVoices((v) => v.map((x, k) => (k === i() ? t.index : x)))} />
                          Track {t.n}
                          <Show when={f.suggested === t.index}> <span class="badge">most speech</span></Show>
                        </label>
                      )}
                    </For>
                  </div>
                </Show>
              </li>
            )}
          </For>
        </ul>
        <For each={props.sheet.failed}>{(x) => <p class="note warn">{baseName(x.path)}: {x.error}</p>}</For>
        <For each={props.mode === "add" || one() === "one" ? props.sheet.pairs : []}>
          {(p, k) => (
            <fieldset class="same-rec" data-testid={`same-${p[0]}-${p[1]}`}>
              <legend>Are these the same recording?</legend>
              <p class="small">
                “{files()[p[0]]?.name}” ({mmss(files()[p[0]]?.duration_s ?? 0)}) and “{files()[p[1]]?.name}” ({mmss(files()[p[1]]?.duration_s ?? 0)}) are about the same length.
              </p>
              <SegmentedControl<"yes" | "no">
                label={`Same recording: ${files()[p[0]]?.name} and ${files()[p[1]]?.name}`}
                size="sm"
                value={same()[k()] ? "yes" : "no"}
                onChange={(v) => setSame((s) => s.map((x, j) => (j === k() ? v === "yes" : x)))}
                options={[
                  { value: "yes", label: "Yes, one video", hint: "The bigger picture with the other file's sound, lined up by their audio and transcribed once" },
                  { value: "no", label: "No, separate videos", hint: "Each is its own video with its own transcript" },
                ]}
              />
            </fieldset>
          )}
        </For>
        <Show when={props.mode === "open" && files().length > 1}>
          <div class="row">
            <SegmentedControl<"one" | "each">
              label="Projects"
              testid="one-project"
              value={one()}
              onChange={setOne}
              options={[
                { value: "one", label: `One project with these ${files().length} videos`, hint: "Cuts can mix parts from any of them" },
                { value: "each", label: "A project for each" },
              ]}
            />
          </div>
          <Show when={one() === "one"}>
            <label class="import-name-field">
              <span class="small">Project name <span class="muted">(optional)</span></span>
              <input type="text" data-testid="import-project-name" maxLength={120} placeholder={defaultName()} value={name()} onInput={(e) => setName(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter") go(); }} />
            </label>
          </Show>
        </Show>
        <div class="row">
          <Button variant="primary" data-testid="import-many-go" onClick={go}>
            {props.mode === "add" ? "Add to project" : one() === "one" ? "Open as one project" : "Open"}
          </Button>
          <Show when={props.mode === "add" && props.onElsewhere}>
            <Button variant="ghost" data-testid="import-many-alt" tooltip="A new project of its own instead" onClick={() => props.onElsewhere?.()}>
              Open as a new project
            </Button>
          </Show>
          <Button variant="ghost" onClick={() => props.onCancel()}>Cancel</Button>
        </div>
      </section>
    </div>
  );
}
