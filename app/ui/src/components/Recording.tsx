// Recording (decision 0045, SPEC §5.18): Settings › Recording (the switch, the five
// permissions, the folder), the Record sheet (what to record, camera, mic, system sound,
// frame rate), and the countdown and recording bar shown over every view.
import { createEffect, createResource, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { recordingActions, type RecPermKind, type RecStatus } from "../actions";
import { Button, Chip, SegmentedControl, Sheet, Switch } from "../design/components";
import { previewPref } from "../prefs";
import { addMarker, cancelCountdown, countdown, rec, recMissing, recRecovery, recStopping, openRecording, setRecRecovery, startRecording, stopRecording } from "../recordingState";
import { attempt, say } from "../store";
import "./recording.css";

const PERMS: { kind: RecPermKind; name: string; why: string; pane: string }[] = [
  { kind: "screen", name: "Screen Recording", why: "the screen files and system sound", pane: "Screen & System Audio Recording" },
  { kind: "camera", name: "Camera", why: "the face file", pane: "Camera" },
  { kind: "microphone", name: "Microphone", why: "your voice, in the face file", pane: "Microphone" },
  { kind: "accessibility", name: "Accessibility", why: "the focused field and caret, for zooms", pane: "Accessibility" },
  { kind: "input", name: "Input Monitoring", why: "the cursor, clicks and ⌃⌥S / ⌃⌥M", pane: "Input Monitoring" },
];
const STATE: Record<string, { label: string; tone: "success" | "danger" | "neutral" }> = {
  granted: { label: "Allowed", tone: "success" },
  denied: { label: "Not allowed", tone: "danger" },
  not_asked: { label: "Not asked", tone: "neutral" },
};

/** m:ss, or h:mm:ss */
function clock(s: number): string {
  const t = Math.max(0, Math.floor(s));
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const sec = String(t % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

/** "Recording 2026-10-19 14-05-09" in local time: a folder name (no slashes or colons). */
function localName(d = new Date()): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `Recording ${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}-${p(d.getMinutes())}-${p(d.getSeconds())}`;
}

/** Settings › Recording: off by default; on asks for every permission in order. */
export function RecordingSettings() {
  const [st, { refetch, mutate }] = createResource(async () => (await attempt(recordingActions.status)) ?? null);
  const [busy, setBusy] = createSignal(false);
  // back from System Settings: read the permissions again
  onMount(() => {
    const again = () => void refetch();
    addEventListener("focus", again);
    onCleanup(() => removeEventListener("focus", again));
  });
  const perm = (s: RecStatus, k: RecPermKind) => s.permissions?.[k];
  /** The first permission not allowed yet: the one asked for next. */
  const next = (s: RecStatus) => PERMS.find((p) => perm(s, p.kind)?.state !== "granted")?.kind;
  /** Asks for one permission. Camera and mic wait for their sheet; the other three answer
   * at once (System Settings opens), so each is asked on its own and never stacked. */
  // asked once this visit: macOS prompts once, so asking again opens System Settings
  const asked = new Set<RecPermKind>();
  const ask = async (k: RecPermKind) => {
    setBusy(true);
    const again = asked.has(k);
    asked.add(k);
    const r = await attempt(() => recordingActions.ask(k, again));
    setBusy(false);
    if (r) mutate(r);
    else void refetch();
  };
  const toggle = async (on_: boolean) => {
    setBusy(true);
    const r = await attempt(() => recordingActions.enable(on_));
    setBusy(false);
    if (!r) return void refetch();
    mutate(r);
    previewPref("recording", r.enabled);
    if (!r.enabled) return;
    say("Recording is on: Record is on Home");
    const k = next(r);
    if (k) await ask(k);
  };
  const relaunch = (s: RecStatus) => perm(s, "screen")?.state === "denied" || perm(s, "input")?.state === "denied";
  return (
    <section class="settings-section rec-settings" aria-label="Recording" data-testid="rec-settings" aria-busy={busy()}>
      <h3>Recording</h3>
      <Show when={st()} fallback={<p class="muted small">Loading…</p>}>
        {(s) => (
          <Show when={!s().unsupported} fallback={<p class="muted small" data-testid="rec-unsupported">Recording is macOS only for now.</p>}>
            <Switch
              label="Record screen and face in gmclip"
              hint="A Record button on Home: each display and the camera as their own files, with the cursor and the focused field logged every frame"
              testid="rec-enable"
              on={s().enabled}
              disabled={busy()}
              onChange={(x) => void toggle(x)}
            />
            <Show when={busy()}>
              <p class="muted small" data-testid="rec-asking">Asking macOS: answer its prompt, then the next one is asked here…</p>
            </Show>
            <ul class="rec-perms" aria-label="Permissions">
              <For each={PERMS}>
                {(p) => {
                  const v = () => perm(s(), p.kind);
                  const state = () => STATE[v()?.state ?? "not_asked"] ?? STATE.not_asked!;
                  return (
                    <li class="rec-perm" data-testid="rec-perm" data-kind={p.kind} data-state={v()?.state ?? "not_asked"}>
                      <span class="rec-perm-name">
                        <b>{p.name}</b> <span class="muted small">{p.why}</span>
                      </span>
                      <Chip tone={state().tone} dot>{state().label}</Chip>
                      <Show when={s().enabled && next(s()) === p.kind}>
                        <Button size="sm" variant="primary" data-testid="rec-perm-ask" disabled={busy()} onClick={() => void ask(p.kind)}>
                          {v()?.state === "denied" && (p.kind === "camera" || p.kind === "microphone") ? "Open Settings" : "Ask"}
                        </Button>
                      </Show>
                      <Show when={v() && v()!.state !== "granted"}>
                        <span class="rec-perm-fix muted small">
                          System Settings › Privacy & Security › {p.pane}{" "}
                          <button
                            type="button"
                            class="ghost small"
                            data-testid="rec-perm-copy"
                            data-tip={v()!.settings}
                            onClick={() => void navigator.clipboard?.writeText(v()!.settings).then(() => say("Link copied: paste it in Safari's address bar"), () => say(v()!.settings))}
                          >
                            Copy link
                          </button>
                        </span>
                      </Show>
                    </li>
                  );
                }}
              </For>
            </ul>
            <Show when={s().bundled === false && (perm(s(), "camera")?.state !== "granted" || perm(s(), "microphone")?.state !== "granted")}>
              <p class="small" data-testid="rec-unbundled">
                gmclip was started from a terminal, so macOS asks that app for the camera and microphone and may never show the sheet. Open gmclip.app instead (<code>app/scripts/dev-app-mac.sh</code> for a dev build).
              </p>
            </Show>
            <Show when={relaunch(s())}>
              <p class="small" data-testid="rec-relaunch">Screen Recording and Input Monitoring take effect after you quit and reopen gmclip.</p>
            </Show>
            <p class="small">
              <Button size="sm" variant="ghost" icon="restore" data-testid="rec-recheck" onClick={() => void refetch()}>Check again</Button>
            </p>
            <dl class="setup-defaults">
              <dt>Recordings go to</dt><dd data-testid="rec-folder">{s().folder}</dd>
              <dt>While recording</dt><dd>⌃⌥S stops · ⌃⌥M drops a marker</dd>
            </dl>
          </Show>
        )}
      </Show>
    </section>
  );
}

/** The Record sheet: what to record and with what, then Start (the countdown follows). */
export function RecordSheet(props: { onClose: () => void }) {
  const [src] = createResource(async () => (await attempt(recordingActions.sources)) ?? null);
  const [what, setWhat] = createSignal<"displays" | "window">("displays");
  const [displays, setDisplays] = createSignal<number[]>([]);
  const [win, setWin] = createSignal<number | null>(null);
  const [face, setFace] = createSignal(true);
  const [camera, setCamera] = createSignal("");
  const [mic, setMic] = createSignal(true);
  const [micId, setMicId] = createSignal("");
  const [system, setSystem] = createSignal(true);
  const [fps, setFps] = createSignal<"30" | "60">("30");
  const [name, setName] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  // the main display by default; the first window when "one window" is picked
  createEffect(on(src, (s) => {
    if (!s) return;
    const main = s.displays.find((d) => d.main) ?? s.displays[0];
    if (main && !displays().length) setDisplays([main.id]);
    if (s.windows[0] && win() === null) setWin(s.windows[0].id);
    if (!s.cameras.length) setFace(false);
    // the default camera (listed first) by name, so the preview shows the one recorded
    else if (!camera()) setCamera(s.cameras[0]!.id);
    if (!s.mics.length) setMic(false);
  }));
  // A live look at the camera while the sheet is open, from the app's own capture in the
  // camera's own mode, as the face file records it (the webview's camera picks a mode of
  // its own, and a capture card sends nothing in another).
  const [previewErr, setPreviewErr] = createSignal("");
  const [previewOn, setPreviewOn] = createSignal(false);
  let canvas: HTMLCanvasElement | undefined;
  let gen = 0;
  const stopPreview = () => {
    gen++;
    if (previewOn()) void recordingActions.preview(false).catch(() => undefined);
    setPreviewOn(false);
  };
  /** Draws each new picture until the preview stops or moves on. */
  const pump = async (g: number) => {
    let seq = 0;
    let since = performance.now();
    while (g === gen) {
      const buf = await recordingActions.previewFrame(seq);
      if (g !== gen) return;
      if (buf && buf.byteLength > 12) {
        const v = new DataView(buf);
        const w = v.getUint32(0, true);
        const h = v.getUint32(4, true);
        seq = v.getUint32(8, true);
        since = performance.now();
        if (canvas && buf.byteLength === 12 + w * h * 4) {
          if (canvas.width !== w) canvas.width = w;
          if (canvas.height !== h) canvas.height = h;
          canvas.getContext("2d")?.putImageData(new ImageData(new Uint8ClampedArray(buf, 12), w, h), 0, 0);
        }
      } else if (performance.now() - since > 4000) {
        setPreviewErr(seq ? "the camera stopped sending pictures" : "the camera opened but sends no picture");
      }
      await new Promise((r) => setTimeout(r, 66));
    }
  };
  createEffect(on([face, camera, src], async ([on_, cam, s]) => {
    stopPreview();
    setPreviewErr("");
    if (!on_ || !s) return;
    const g = gen;
    const r = await attempt(() => recordingActions.preview(true, cam || undefined));
    if (g !== gen) return;
    if (!r?.on) return void setPreviewErr("the camera did not open");
    setPreviewOn(true);
    void pump(g);
  }));
  onCleanup(stopPreview);
  const pick = (id: number, on: boolean) => setDisplays((d) => (on ? [...d.filter((x) => x !== id), id] : d.filter((x) => x !== id)));
  const ready = () => !busy() && !!src() && (what() === "displays" ? displays().length > 0 : win() !== null);
  const start = async () => {
    if (!ready()) return;
    setBusy(true);
    // the recorder opens the camera itself
    stopPreview();
    const ok = await startRecording({
      ...(what() === "displays" ? { displays: displays() } : { window: win()! }),
      face: face(),
      ...(face() && camera() ? { camera: camera() } : {}),
      mic: mic(),
      ...(mic() && micId() ? { mic_id: micId() } : {}),
      system_audio: system(),
      fps: fps() === "60" ? 60 : 30,
      // the person's own clock (the core's default name is UTC)
      name: name().trim() || localName(),
    });
    setBusy(false);
    if (ok) props.onClose();
  };
  return (
    <Sheet
      label="Record"
      title="Record"
      sub="Screen and face as files of their own, with the cursor logged every frame."
      onClose={props.onClose}
      testid="rec-sheet"
      class="rec-sheet"
      actions={
        <Button variant="primary" icon="record" data-testid="rec-start" disabled={!ready()} onClick={() => void start()}>
          {busy() ? "Starting…" : "Start"}
        </Button>
      }
    >
      <Show when={src()} fallback={<p class="muted small">{src.loading ? "Looking for displays, windows, cameras and mics…" : "gmclip could not list what it can record"}</p>}>
        {(s) => (
          <>
            <div class="rec-field">
              <span class="rec-label">Record</span>
              <SegmentedControl<"displays" | "window">
                label="Record"
                size="sm"
                testid="rec-what"
                value={what()}
                onChange={setWhat}
                options={[
                  { value: "displays", label: s().displays.length > 1 ? "Displays" : "The screen", icon: "video" },
                  { value: "window", label: "One window", icon: "square-dashed" },
                ]}
              />
              <Show
                when={what() === "displays"}
                fallback={
                  <select aria-label="Window" data-testid="rec-window" value={String(win() ?? "")} onChange={(e) => setWin(Number(e.currentTarget.value))}>
                    <For each={s().windows} fallback={<option value="">No windows</option>}>
                      {(w) => <option value={String(w.id)}>{w.app}{w.title ? ` — ${w.title}` : ""}</option>}
                    </For>
                  </select>
                }
              >
                <div class="rec-displays" role="group" aria-label="Displays">
                  <For each={s().displays}>
                    {(d) => (
                      <label class="rec-check">
                        <input type="checkbox" data-testid="rec-display" checked={displays().includes(d.id)} onChange={(e) => pick(d.id, e.currentTarget.checked)} />
                        <span>{d.name}{d.main ? " (main)" : ""}</span>
                        <span class="muted small tabular">{d.width}×{d.height}</span>
                      </label>
                    )}
                  </For>
                </div>
                <Show when={displays().length > 1}>
                  <p class="muted small">Each display is a file of its own.</p>
                </Show>
              </Show>
            </div>
            <div class="rec-field">
              <span class="rec-label">Camera</span>
              <div class="row">
                <Switch label="Face" testid="rec-face" on={face()} disabled={!s().cameras.length} hint={s().cameras.length ? "A face file beside the screen" : "No camera found"} onChange={setFace} />
                <select aria-label="Camera" data-testid="rec-camera" disabled={!face()} value={camera()} onChange={(e) => setCamera(e.currentTarget.value)}>
                  <For each={s().cameras}>{(c) => <option value={c.id}>{c.name}</option>}</For>
                </select>
              </div>
              <Show when={face()}>
                <div class="rec-cam-preview" data-testid="rec-cam-preview">
                  <canvas ref={canvas} classList={{ live: previewOn() && !previewErr() }} />
                  <Show when={!previewOn() || previewErr()}>
                    <span class="muted small">{previewErr() ? `No preview: ${previewErr()}` : "Opening the camera…"}</span>
                  </Show>
                </div>
              </Show>
            </div>
            <div class="rec-field">
              <span class="rec-label">Sound</span>
              <div class="row">
                <Switch label="Microphone" testid="rec-mic" on={mic()} disabled={!s().mics.length} hint="Your voice, as the face file's sound" onChange={setMic} />
                <select aria-label="Microphone" data-testid="rec-mic-id" disabled={!mic()} value={micId()} onChange={(e) => setMicId(e.currentTarget.value)}>
                  <option value="">Default microphone</option>
                  <For each={s().mics}>{(m) => <option value={m.id}>{m.name}</option>}</For>
                </select>
              </div>
              <Switch label="System sound" testid="rec-system" on={system()} hint="What the Mac plays, as the screen file's sound" onChange={setSystem} />
            </div>
            <div class="rec-field">
              <span class="rec-label">Frame rate</span>
              <SegmentedControl<"30" | "60">
                label="Frame rate"
                size="sm"
                testid="rec-fps"
                value={fps()}
                onChange={setFps}
                options={[
                  { value: "30", label: "30 fps" },
                  { value: "60", label: "60 fps", hint: "Smoother motion; may drop frames on big displays" },
                ]}
              />
              <Show when={src()?.live?.length} fallback={<Show when={fps() === "60"}><p class="muted small">60 fps may drop frames on big displays.</p></Show>}>
                <p class="muted small" data-testid="rec-streaming-safe">
                  {src()!.live!.join(", ")} is running: the screen records at 30 fps within 2560×1440 so the stream keeps the encoder.
                </p>
              </Show>
            </div>
            <div class="rec-field">
              <span class="rec-label">Name</span>
              <input type="text" aria-label="Recording name" data-testid="rec-name" placeholder="Optional: “Recording <date> <time>” otherwise" value={name()} onInput={(e) => setName(e.currentTarget.value)} />
            </div>
          </>
        )}
      </Show>
    </Sheet>
  );
}

/** The 3-2-1 before the files start (Esc cancels) and, while recording, the bar: time,
 * markers, Marker and Stop. Shown over every view. */
export function RecordingOverlay() {
  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || countdown() === null) return;
      e.preventDefault();
      e.stopPropagation();
      void cancelCountdown();
    };
    addEventListener("keydown", onKey, true);
    onCleanup(() => removeEventListener("keydown", onKey, true));
  });
  return (
    <>
      <Show when={countdown()}>
        {(n) => (
          <div class="rec-countdown" role="status" aria-live="assertive" data-testid="rec-countdown">
            <span class="rec-count tabular">{n()}</span>
            <Show when={recMissing().length}>
              <p class="small">Recording without: {recMissing().join(" · ")}</p>
            </Show>
            <p class="muted small">Esc cancels</p>
          </div>
        )}
      </Show>
      <Show when={!rec() && countdown() === null && recRecovery()}>
        {(dir) => (
          <div class="rec-bar rec-recovery" role="alert" data-testid="rec-recovery">
            <span>Recording could not open. Your files are in <span class="selectable">{dir()}</span></span>
            <Button size="sm" onClick={() => void openRecording(dir())}>Try opening again</Button>
            <Button size="sm" variant="ghost" onClick={() => setRecRecovery(null)}>Dismiss</Button>
          </div>
        )}
      </Show>
      <Show when={rec()}>
        {(r) => (
          <div class="rec-bar" role="status" aria-label="Recording" data-testid="rec-bar">
            <span class="rec-dot" aria-hidden="true" />
            <span class="rec-time tabular" data-testid="rec-time">{clock(r().elapsed_s)}</span>
            <span class="muted small" data-testid="rec-markers">{r().markers} marker{r().markers === 1 ? "" : "s"}</span>
            <Show when={r().missing.length}>
              <Chip tone="warning" tooltip={r().missing.join(" · ")}>without {r().missing.length}</Chip>
            </Show>
            <Show when={(r().dropped ?? 0) > 0}>
              <span class="rec-trouble small" role="alert" data-testid="rec-overload">Capture has fallen behind. For smoother video, use 30 fps or record a smaller area.</span>
            </Show>
            <Show when={r().trouble}>
              {(t) => <span class="rec-trouble small" role="alert" data-testid="rec-trouble">{t()}</span>}
            </Show>
            <Button size="sm" variant="ghost" icon="flag" keys="⌃⌥M" tooltip="Drop a marker here" data-testid="rec-marker" disabled={recStopping()} onClick={() => void addMarker()}>Marker</Button>
            <Button size="sm" variant="danger" icon="stop" keys="⌃⌥S" tooltip="Stop and open the recording" data-testid="rec-stop" disabled={recStopping()} onClick={() => void stopRecording()}>
              {recStopping() ? "Opening…" : "Stop"}
            </Button>
          </div>
        )}
      </Show>
    </>
  );
}
