// The left sidebar of the cut view (SPEC §5.17 project map, §12.2): a collapsible,
// styled container around lane T's ProjectMap (stream edit → topics → long-forms →
// shorts). Docked in the wide layout (≥ 1280 px), an overlay drawer below that; hidden in
// focus and reading. Its width is dragged on the splitter at its right edge.
import { IconButton } from "../design/components";
import { layout, paneShown, togglePane } from "../layout";
import { cutView, projects } from "../store";
import { ProjectMap } from "./ProjectMap";

/** Whether the map shows now, and the top bar's toggle for it. */
export function mapShown(): boolean {
  return paneShown("map");
}
export function toggleMap() {
  togglePane("map");
}

export function MapSidebar() {
  // docked on wide windows when the person keeps it open; below 1280 px a drawer that
  // opens only when asked (never by itself over the viewer)
  const state = () => (!paneShown("map") ? "closed" : layout() === "wide" ? "docked" : "overlay");
  const project = () => projects().find((p) => p.id === cutView()?.project);
  return (
    <aside class="map" aria-label="Project map sidebar" data-open={state()} hidden={state() === "closed"}>
      <header class="mapbar-head">
        <span>{project()?.name ?? "Project"}</span>
        <IconButton icon="x" size="sm" label="Hide the project map" keys="⌥1" onClick={() => togglePane("map")} />
      </header>
      <div class="mapbar-body">
        <ProjectMap />
      </div>
    </aside>
  );
}
