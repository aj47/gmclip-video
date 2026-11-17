import { readFileSync, rmSync } from "node:fs";
import { STATE } from "./global-setup";

export default async function teardown() {
  try {
    const s = JSON.parse(readFileSync(STATE, "utf8")) as { pid: number; home: string };
    process.kill(s.pid, "SIGTERM");
    if (!process.env.GMCLIP_E2E_KEEP && !process.env.GMCLIP_E2E_HOME) rmSync(s.home, { recursive: true, force: true });
  } catch {
    /* already gone */
  }
}
