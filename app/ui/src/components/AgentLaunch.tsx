// "New agent session" (decision 0035): which of the user's agents and model, on what (the library,
// a project, one video or one cut — prefilled from where it was opened), with which
// access, and the first message. Pressing Start is the human click (R-AGT-3); the Owner's
// sheet asks first when the agent may see and hear the footage.
import { createEffect, createMemo, createResource, createSignal, For, Show } from "solid-js";
import { actions, sessionActions, type AgentClient, type SessionAccess, type SessionTargetArgs } from "../actions";
import { Button, SegmentedControl, Sheet, Switch } from "../design/components";
import { setRoute, attempt, projects } from "../store";
import { lastAccess, lastModel, setLastAccess, setLastModel, startSession } from "../agentsState";
import { ModelSelect } from "./AgentDock";
import "./agents.css";

type Kind = "library" | "project" | "video" | "cut";

const STARTERS: Record<Kind, string[]> = {
  library: ["Which of my streams has the most clip-worthy moments? List the best five with why.", "Find every time I talked about pricing across my projects."],
  project: ["Find the ten strongest moments in this project and draft a cut from each.", "Draft three short cuts from the best moments, 30 to 60 seconds each."],
  video: ["Read this video's transcript and add the moments worth a short.", "Make a 60 second payoff-first cut from this video."],
  cut: ["Tighten this cut: drop pauses and fillers, hook first, keep it under 60 s.", "Grow this into a ~60 s story with a clear payoff.", "Check the captions for misheard words and fix them."],
};

export function AgentLaunch(props: { at: SessionTargetArgs & { harness?: string }; onClose: () => void }) {
  const [agents] = createResource(async () => (await attempt(sessionActions.agents)) ?? null);
  const initial: Kind = props.at.cut ? "cut" : props.at.source ? "video" : props.at.project ? "project" : "library";
  const [kind, setKind] = createSignal<Kind>(initial);
  const [project, setProject] = createSignal(props.at.project ?? projects()[0]?.id ?? "");
  const [source, setSource] = createSignal(props.at.source ?? "");
  const [cut, setCut] = createSignal(props.at.cut ?? "");
  const [harness, setHarnessRaw] = createSignal<AgentClient["harness"] | "">("");
  const [model, setModelRaw] = createSignal("");
  const setHarness = (h: AgentClient["harness"]) => {
    setHarnessRaw(h);
    setModelRaw(lastModel(h));
  };
  const setModel = (m: string) => {
    setModelRaw(m);
    if (harness()) setLastModel(harness(), m);
  };
  const [access, setAccessRaw] = createSignal<SessionAccess>(lastAccess());
  const setAccess = (a: SessionAccess) => {
    setAccessRaw(a);
    setLastAccess(a);
  };
  const [media, setMedia] = createSignal(false);
  const [folder, setFolder] = createSignal("");
  const [message, setMessage] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [map] = createResource(
    () => (kind() === "video" && project() ? project() : null),
    async (p) => (await attempt(() => actions.projectMap(p))) ?? null,
  );
  const proj = () => projects().find((p) => p.id === project());
  const cuts = () => proj()?.cuts ?? [];
  const videos = () => (map()?.sources ?? []).filter((s) => !s.offline);
  // a picked project's first cut / video, when the one asked for is not in it
  createEffect(() => {
    if (kind() === "cut" && !cuts().some((c) => c.id === cut())) setCut(cuts()[0]?.id ?? "");
  });
  createEffect(() => {
    if (kind() === "video" && map() && !videos().some((v) => v.id === source())) setSource(videos()[0]?.id ?? "");
  });
  createEffect(() => {
    const a = agents();
    // the agent picked on the Agents page (its chip), else the default, else the first found
    if (a && !harness()) setHarness(a.agents.find((x) => x.harness === props.at.harness && x.available)?.harness ?? a.agents.find((x) => x.harness === a.default && x.available)?.harness ?? a.agents.find((x) => x.available)?.harness ?? a.default);
  });
  const chosen = createMemo(() => agents()?.agents.find((a) => a.harness === harness()));
  const target = (): SessionTargetArgs | null => {
    switch (kind()) {
      case "library":
        return {};
      case "project":
        return project() ? { project: project() } : null;
      case "video":
        return project() && source() ? { project: project(), source: source() } : null;
      case "cut":
        return project() && cut() ? { project: project(), cut: cut() } : null;
    }
  };
  const ready = () => !!target() && !!message().trim() && !!chosen()?.available && !busy();
  const start = async () => {
    const t = target();
    const h = harness();
    if (!t || !h) return;
    setBusy(true);
    const offered = chosen()?.models ?? [];
    const id = await startSession(t, h, access(), message().trim(), kind() === "cut" && media(), access() !== "gmclip" ? folder().trim() || undefined : undefined, offered.some((x) => x.id === model()) ? model() : undefined);
    setBusy(false);
    if (id) {
      props.onClose();
      setRoute({ view: "agents" });
    }
  };
  return (
    <Sheet
      label="New agent session"
      title="New agent session"
      sub="Your own agent, on your own plan, with gmclip's tools."
      onClose={props.onClose}
      testid="agent-launch"
      class="ag-launch"
      actions={
        <Button variant="primary" icon="send" data-testid="launch-start" disabled={!ready()} onClick={() => void start()}>
          {busy() ? "Starting…" : "Start"}
        </Button>
      }
    >
      <div class="ag-field">
        <span class="ag-label">Agent</span>
        <Show when={agents()} fallback={<p class="muted small">Looking for your agents…</p>}>
          {(a) => (
            <div class="ag-agents" role="radiogroup" aria-label="Agent">
              <For each={a().agents}>
                {(x) => (
                  <button
                    type="button"
                    role="radio"
                    aria-checked={harness() === x.harness}
                    class={`ag-agent${harness() === x.harness ? " on" : ""}`}
                    disabled={!x.available}
                    data-testid="launch-agent"
                    data-harness={x.harness}
                    data-tip={x.available ? `via ${x.via}` : (x.problem ?? "not found")}
                    onClick={() => setHarness(x.harness)}
                  >
                    <b>{x.label}</b>
                    <span class="muted small">{x.available ? (x.via?.startsWith("npx") ? "via npx" : "installed") : "not found"}</span>
                  </button>
                )}
              </For>
            </div>
          )}
        </Show>
        <ModelSelect models={chosen()?.models ?? []} value={model()} onChange={setModel} testid="launch-model" />
      </div>

      <div class="ag-field">
        <span class="ag-label">Work on</span>
        <SegmentedControl<Kind>
          label="Work on"
          size="sm"
          testid="launch-kind"
          value={kind()}
          onChange={setKind}
          options={[
            { value: "library", label: "Library", icon: "book" },
            { value: "project", label: "Project", icon: "folder-open" },
            { value: "video", label: "Video", icon: "video" },
            { value: "cut", label: "Cut", icon: "scissors" },
          ]}
        />
        <Show when={kind() !== "library"}>
          <div class="row ag-pickers">
            <select aria-label="Project" data-testid="launch-project" value={project()} onChange={(e) => setProject(e.currentTarget.value)}>
              <For each={projects()} fallback={<option value="">No project open</option>}>{(p) => <option value={p.id}>{p.name}</option>}</For>
            </select>
            <Show when={kind() === "video"}>
              <select aria-label="Video" data-testid="launch-video" value={source()} onChange={(e) => setSource(e.currentTarget.value)}>
                <For each={videos()} fallback={<option value="">{map.loading ? "Loading…" : "No videos"}</option>}>{(v) => <option value={v.id}>{v.name}</option>}</For>
              </select>
            </Show>
            <Show when={kind() === "cut"}>
              <select aria-label="Cut" data-testid="launch-cut" value={cut()} onChange={(e) => setCut(e.currentTarget.value)}>
                <For each={cuts()} fallback={<option value="">No cuts</option>}>{(c) => <option value={c.id}>{c.title}</option>}</For>
              </select>
            </Show>
          </div>
        </Show>
      </div>

      <div class="ag-field">
        <span class="ag-label">Access</span>
        <SegmentedControl<SessionAccess>
          label="Access"
          size="sm"
          testid="launch-access"
          value={access()}
          onChange={setAccess}
          options={[
            { value: "gmclip", label: "gmclip tools only", icon: "sparkles" },
            { value: "full", label: "Full access", icon: "command" },
            { value: "bypass", label: "No asking", icon: "triangle-alert" },
          ]}
        />
        <p class="muted small" data-testid="launch-access-note">
          {access() === "gmclip"
            ? chosen()?.contained
              ? `${chosen()?.label} starts with its own tools and your settings off: it can only use gmclip.`
              : `gmclip refuses any other tool ${chosen()?.label ?? "the agent"} asks to run. Read-only tools it runs without asking may still run.`
            : access() === "full"
              ? "It also gets its own tools (shell, files, web) and your settings, skills and CLAUDE.md. Each of its own tools asks you first. gmclip's tools may open files in your home folder for it."
              : "It also gets its own tools (shell, files, web) and your settings, skills and CLAUDE.md, and runs them without asking you. gmclip's tools may open files in your home folder for it."}
          {chosen() && !chosen()!.gmclip_tools ? ` ${chosen()!.label} has no MCP client, so it cannot use gmclip's tools.` : ""}
        </p>
        <Show when={access() !== "gmclip"}>
          <input type="text" aria-label="Folder to work in" data-testid="launch-folder" placeholder="Folder to work in (optional; a scratch folder otherwise)" value={folder()} onInput={(e) => setFolder(e.currentTarget.value)} />
        </Show>
        <Show when={kind() === "cut"}>
          <Switch label="Let it see and hear the footage" hint="Asks you on a system sheet first" on={media()} onChange={setMedia} testid="launch-media" />
        </Show>
      </div>

      <div class="ag-field">
        <span class="ag-label">What should it do?</span>
        <textarea
          rows="4"
          aria-label="First message"
          data-testid="launch-message"
          placeholder="Say it like you would to an editor."
          value={message()}
          onInput={(e) => setMessage(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && ready()) {
              e.preventDefault();
              void start();
            }
          }}
        />
        <div class="ag-starters">
          <For each={STARTERS[kind()]}>
            {(s) => (
              <button type="button" class="ag-starter" onClick={() => setMessage(s)}>
                {s}
              </button>
            )}
          </For>
        </div>
      </div>
    </Sheet>
  );
}
