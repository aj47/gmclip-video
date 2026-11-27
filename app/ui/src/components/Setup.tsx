// Settings (SPEC §12.5): what agents may use, the style rules, the agent, the setup
// answers, storage and backups; recording (decision 0045); the developer setting (decision
// 0054), with the review link and Pair with Clippy under it.
import { createResource, createSignal, For, Show } from "solid-js";
import { MachineCaps } from "./MachineCaps";
import { thisMachineTitle } from "../platform";
import { Button, Switch } from "../design/components";
import { prefs, setPref } from "../prefs";
import { Appearance } from "./Appearance";
import { RecordingSettings } from "./Recording";
import { actions } from "../actions";
import { attempt, say } from "../store";
import { AgentAccess, AgentPicker } from "./AgentPanel";
import { SetupSheet, SetupSummary } from "./FirstRun";
import { RestoreSheet, StoragePanel } from "./Reliability";
import { ReviewLink } from "./ReviewLink";

/** The sections the left list scrolls to. */
const SECTIONS = [
  { id: "appearance", label: "Appearance" },
  { id: "recording", label: "Recording" },
  { id: "agents", label: "Agents" },
  { id: "setup", label: "Setup" },
  { id: "mac", label: thisMachineTitle },
  { id: "developer", label: "Developer" },
];

/** Settings (Owner, read only here): what agents may use, the style rules, the agent. */
export function Settings() {
  const [st, { refetch: refetchSt }] = createResource(async () => (await attempt(actions.ownerSettings)) ?? null);
  const pickStyle = async () => {
    const r = await attempt(actions.stylePick);
    if (r && !r.cancelled) say("Style rules set");
    void refetchSt();
  };
  const reviewStyle = async () => {
    const r = await attempt(actions.styleReview);
    if (r?.accepted) say("Agents now read the new style rules");
    void refetchSt();
  };
  const [setupOpen, setSetupOpen] = createSignal(false);
  const [setupKey, setSetupKey] = createSignal(0);
  const [restoring, setRestoring] = createSignal(false);
  const [access, setAccess] = createSignal(false);
  // Clippy's pairing code shows once on a native sheet (Owner)
  const pairClippy = async () => {
    if (await attempt(actions.clippyPair)) say("Paired with Clippy");
  };
  return (
    <main class="settings" aria-label="Settings">
      {/* the sections, each a click away (AJ 2026-11-22) */}
      <nav class="settings-nav" aria-label="Settings sections">
        <For each={SECTIONS}>
          {(x) => (
            <button type="button" class="ghost settings-nav-item" data-testid={`settings-nav-${x.id}`} onClick={() => document.getElementById(`set-${x.id}`)?.scrollIntoView({ block: "start", behavior: "smooth" })}>
              {x.label}
            </button>
          )}
        </For>
      </nav>
      <div class="settings-body">
      <h2>Settings</h2>
      <div id="set-appearance" class="settings-anchor" />
      <Appearance />
      <div id="set-recording" class="settings-anchor" />
      <RecordingSettings />
      <div id="set-agents" class="settings-anchor" />
      {/* what agents may use (Owner: read only here, changed through Agent access) */}
      <section class="settings-section" aria-label="Agent access" data-testid="settings-agent-access">
        <h3>Agent access</h3>
        <Show when={st()} fallback={<p class="muted small" data-testid="agent-access-wait">{st.loading ? "Finding your agents…" : "Not available in this window."}</p>}>
          {(s) => (
            <dl class="setup-defaults">
              <dt>Profile</dt><dd data-testid="settings-profile">{s().profile ?? "none"}</dd>
              <dt>Style rules</dt>
              <dd>
                <span data-testid="settings-style">{s().style ? `${s().style!.path} (${s().style!.sha256.slice(0, 12)}…)` : "none"}</span>{" "}
                <Show when={s().style?.changed_on_disk}>
                  <span class="chip chip-warning" data-testid="style-changed">changed on disk: agents still read the accepted version</span>{" "}
                  <button type="button" class="ghost small" data-testid="style-review" onClick={() => void reviewStyle()}>Review changes…</button>{" "}
                </Show>
                <button type="button" class="ghost small" data-testid="style-pick" onClick={() => void pickStyle()}>Choose file…</button>
              </dd>
              <dt>Folders agents may read</dt><dd>{[...(s().home && s().home_readable ? [`${s().home} (your home folder)`] : []), ...new Set([...(s().read_roots ?? []), ...(s().write_roots ?? [])])].join(", ") || "none"}</dd>
              <dt>Folders agents may write</dt><dd>{(s().write_roots ?? []).join(", ") || "none"}</dd>
              <dt>Recordings folder</dt><dd>{s().recordings ?? "none"}</dd>
              <dt>Your agent</dt><dd>{s().agent?.found ? s().agent?.path : s().agent?.message}</dd>
              <dt>Blocked clients</dt><dd>{(s().revoked ?? []).join(", ") || "none"}</dd>
            </dl>
          )}
        </Show>
        <p class="muted small">
          Changes go through Agent access (each asks you in a system dialog).{" "}
          <button type="button" class="ghost small" data-testid="agent-access" onClick={() => setAccess(true)}>Agent access…</button>
        </p>
      </section>
      <AgentPicker />
      <div id="set-setup" class="settings-anchor" />
      <SetupSummary tick={setupKey()} onChange={() => setSetupOpen(true)} />
      <div id="set-mac" class="settings-anchor" />
      <section class="settings-section" data-testid="settings-mac">
        <h3>{thisMachineTitle}</h3>
        <MachineCaps />
      </section>
      <StoragePanel />
      <section class="settings-section" aria-label="Backups">
        <h3>Backups</h3>
        <p class="small">
          A copy of each project when you quit and every 30 minutes while you edit.{" "}
          <button type="button" class="ghost small" data-testid="settings-restore" onClick={() => setRestoring(true)}>Restore an earlier copy…</button>
        </p>
      </section>
      <div id="set-developer" class="settings-anchor" />
      {/* decision 0054: what a team or a tester uses, off for one person editing with agents */}
      <section class="settings-section" aria-label="Developer" data-testid="settings-developer">
        <h3>Developer</h3>
        <Switch
          label="Developer tools"
          hint="Review links, Pair with Clippy, starting agents inside gmclip, every side panel in the bar, and the preview's measuring tools"
          on={prefs.developer}
          onChange={(on) => setPref("developer", on)}
          testid="pref-developer"
        />
        <Show when={prefs.developer}>
          <p class="small">
            <Button size="sm" variant="ghost" data-testid="clippy-pair" onClick={() => void pairClippy()}>Pair with Clippy…</Button>
          </p>
        </Show>
      </section>
      <Show when={prefs.developer}>
        <ReviewLink />
      </Show>
      <Show when={setupOpen()}>
        <SetupSheet onClose={() => setSetupOpen(false)} onDone={() => { setSetupOpen(false); setSetupKey(setupKey() + 1); }} />
      </Show>
      <Show when={restoring()}>
        <RestoreSheet onClose={() => setRestoring(false)} />
      </Show>
      <Show when={access()}>
        <AgentAccess onClose={() => setAccess(false)} />
      </Show>
      </div>
    </main>
  );
}
