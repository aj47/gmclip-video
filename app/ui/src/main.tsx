// First: Solid's template assignments need the Trusted Types policy.
import "./tt-install";
import { render } from "solid-js/web";
import { App } from "./App";
import { actions } from "./actions";
import { attempt, boot, openCut, projects } from "./store";
import { watchSize } from "./sizeclass";
import "./design/brand.css";
import "./design/tokens.css";
import "./design/components.css";
import "./styles.css";
import "./design/shell.css";

watchSize();
const root = document.getElementById("root");
if (root) render(() => <App />, root);
void attempt(boot).then(async () => {
  const info = await attempt(actions.info);
  if (info?.autorun === "s2a") {
    const p = projects()[0];
    const c = p?.cuts.find((c) => c.imported);
    if (p && c) {
      await openCut(p.id, c.id);
      setTimeout(() => void (globalThis as unknown as { __gmclipS2a?: () => Promise<unknown> }).__gmclipS2a?.(), 2500);
    }
  }
});
