// Playwright drives the real UI build in Chromium against the real core through the
// dev bridge (SPEC §12.1, R-DEV-1): `vite preview` serves `dist/` with the app's CSP
// (Trusted Types enforced), and the global setup launches `gmclip-app --dev-bridge
// --no-window` with a throwaway HOME. Browsers come from the local Playwright cache
// (no downloads). `@wdio/tauri-service` against the WKWebView window comes later.
import { defineConfig } from "@playwright/test";
import { tmpdir } from "node:os";
import { join } from "node:path";

// Per run: the preview port and the state file (lanes run Playwright side by side).
const PORT = Number(process.env.GMCLIP_E2E_PORT ?? 4173);
process.env.GMCLIP_E2E_PORT = String(PORT);
process.env.GMCLIP_E2E_STATE ??= join(tmpdir(), `gmclip-e2e-${process.pid}.json`);

export default defineConfig({
  testDir: "tests/e2e",
  // scripts/e2e.mjs gives each spec its own (specs run side by side; a run clears it)
  outputDir: process.env.GMCLIP_E2E_OUT ?? "test-results",
  timeout: 240_000,
  expect: { timeout: 20_000 },
  workers: 1,
  fullyParallel: false,
  reporter: [["list"]],
  globalSetup: "./tests/e2e/global-setup.ts",
  globalTeardown: "./tests/e2e/global-teardown.ts",
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    browserName: "chromium",
    headless: true,
    trace: "off",
  },
  // Build first: the tests never run against a stale dist/. `scripts/e2e.mjs` builds once
  // and serves that build to every spec (GMCLIP_E2E_SERVED), so a spec run there starts
  // no server of its own.
  webServer: process.env.GMCLIP_E2E_SERVED
    ? undefined
    : {
        command: `pnpm exec vite build && pnpm exec vite preview --host 127.0.0.1 --port ${PORT} --strictPort`,
        url: `http://127.0.0.1:${PORT}`,
        reuseExistingServer: false,
        timeout: 60_000,
      },
});
