// Settings › Appearance and layout: theme, density, the transcript's text sizes and
// times, the panes a launch opens with, and Reset layout. Saved as UI preferences.
import { Button, SegmentedControl, Switch } from "../design/components";
import { resetLayout } from "../layout";
import { osName } from "../platform";
import { prefs, setPref } from "../prefs";
import { say } from "../store";

export function Appearance() {
  return (
    <section class="settings-section appearance" aria-label="Appearance and layout" data-testid="appearance">
      <h3>Appearance and layout</h3>
      <div class="appearance-grid">
        <span class="appearance-label">Theme</span>
        <SegmentedControl
          label="Theme"
          size="sm"
          testid="pref-theme"
          options={[
            { value: "dark", label: "Dark" },
            { value: "light", label: "Light" },
            { value: "system", label: `Follow ${osName}`, hint: "Dark or light with the system" },
          ]}
          value={prefs.theme}
          onChange={(v) => setPref("theme", v)}
        />
        <span class="appearance-label">Density</span>
        <SegmentedControl
          label="Density"
          size="sm"
          testid="pref-density"
          options={[
            { value: "comfortable", label: "Comfortable" },
            { value: "compact", label: "Compact", hint: "Tighter bars and controls: more room for the picture" },
          ]}
          value={prefs.density}
          onChange={(v) => setPref("density", v)}
        />
        <label class="appearance-label" for="pref-tr-size">Transcript text</label>
        <span class="appearance-range">
          <input id="pref-tr-size" type="range" min="11" max="22" step="1" value={prefs.tr_size} data-testid="pref-tr-size" onInput={(e) => setPref("tr_size", Number(e.currentTarget.value))} />
          <span class="small tabular">{prefs.tr_size} px</span>
        </span>
        <label class="appearance-label" for="pref-read-size">Reading view text</label>
        <span class="appearance-range">
          <input id="pref-read-size" type="range" min="14" max="34" step="1" value={prefs.read_size} data-testid="pref-read-size" onInput={(e) => setPref("read_size", Number(e.currentTarget.value))} />
          <span class="small tabular">{prefs.read_size} px</span>
        </span>
        <span class="appearance-label">Transcript</span>
        <label class="appearance-label" for="pref-open-panes">A cut opens with</label>
        <select id="pref-open-panes" data-testid="pref-open-panes" value={prefs.open_panes} onChange={(e) => setPref("open_panes", e.currentTarget.value as "last" | "all" | "focus")}>
          <option value="last">The panes as I left them</option>
          <option value="all">Every pane showing</option>
          <option value="focus">Focus: viewer and timeline</option>
        </select>
        <span class="appearance-label">Layout</span>
        <span>
          <Button size="sm" icon="restore" keys="⌥0" tooltip="Every pane back, at its own size" data-testid="pref-reset-layout" onClick={() => { resetLayout(); say("Layout reset: every pane showing at its own size"); }}>
            Reset layout
          </Button>
        </span>
      </div>
      <p class="muted small">Every pane also shows and hides from <b>View</b> in the top bar of a cut, with its key (⌥1–⌥5, ⌥R reads the transcript, ⌥F focuses).</p>
    </section>
  );
}
