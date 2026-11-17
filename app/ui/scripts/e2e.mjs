// Each spec runs against its own launch of the core: the frame socket accepts one
// connection per launch (R-FRAME-1), and each spec's page needs one. The UI is built
// once and one `vite preview` serves that `dist/` to every spec (it is static files:
// nothing a spec does reaches it), instead of a build and a server per spec.
//
// Launches are independent (each has its own HOME, TMPDIR, state file and loopback ports),
// so specs run `--jobs` at a time (default GMCLIP_E2E_JOBS, else 2). The specs that gate
// on timing (SOLO below) run afterwards, one at a time, with nothing else running.
//
//   node scripts/e2e.mjs                 every spec (qa-*, shots and s2a only with
//                                        GMCLIP_QA, GMCLIP_SHOTS or GMCLIP_S2A set)
//   node scripts/e2e.mjs lanes crop      only these (names without .spec.ts)
//   node scripts/e2e.mjs --repeat 5 lanes   each named spec five times in a row
//   node scripts/e2e.mjs --jobs 1        one spec at a time, its output live
//
// Prints each spec's time and a summary (failures, slowest specs, the total).
import { spawn, spawnSync } from "node:child_process";
import { readdirSync } from "node:fs";
import { request } from "node:http";

const args = process.argv.slice(2);
let repeat = 1;
let jobs = Math.max(1, Number(process.env.GMCLIP_E2E_JOBS) || 2);
const only = [];
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--repeat") repeat = Math.max(1, Number(args[++i]) || 1);
  else if (args[i] === "--jobs") jobs = Math.max(1, Number(args[++i]) || 1);
  else only.push(args[i].replace(/\.spec\.ts$/, ""));
}
// Gated on frame times, latency or load times: measured with no other launch running.
const SOLO = new Set(["evtburst.spec.ts", "hugefile.spec.ts", "tlcolumns.spec.ts", "tlperf.spec.ts", "words3h.spec.ts"]);
// Opt-in specs skip every test without their variable, so a run without it would pay an
// app launch for nothing: they are left out unless it is set (or they are named).
const optIn = (f) =>
  f === "s2a.spec.ts" ? !process.env.GMCLIP_S2A : f.startsWith("qa-") ? !process.env.GMCLIP_QA : f === "shots.spec.ts" ? !process.env.GMCLIP_SHOTS : false;
const every = readdirSync("tests/e2e").filter((f) => f.endsWith(".spec.ts")).sort();
const all = every.filter((f) => !optIn(f));
const specs = only.length ? only.map((n) => `${n}.spec.ts`).filter((f) => every.includes(f)) : all;
if (only.length && specs.length !== only.length) {
  console.error(`no such spec: ${only.filter((n) => !specs.includes(`${n}.spec.ts`)).join(", ")}`);
  process.exit(2);
}

const PORT = Number(process.env.GMCLIP_E2E_PORT ?? 4173);
const win = process.platform === "win32";
const t0 = Date.now();
const secs = (ms) => `${(ms / 1000).toFixed(1)}s`;

// on Windows pnpm is a .cmd / .ps1 shim, which only a shell runs
const build = spawnSync("pnpm", ["exec", "vite", "build"], { stdio: "inherit", shell: win });
if (build.status !== 0) process.exit(build.status ?? 1);
const builtMs = Date.now() - t0;

const preview = spawn("pnpm", ["exec", "vite", "preview", "--host", "127.0.0.1", "--port", String(PORT), "--strictPort"], {
  stdio: ["ignore", "ignore", "inherit"],
  shell: win,
  detached: !win,
});
const stopPreview = () => {
  try {
    if (win) spawnSync("taskkill", ["/pid", String(preview.pid), "/t", "/f"]);
    else process.kill(-preview.pid, "SIGTERM");
  } catch {
    /* gone */
  }
};
for (const sig of ["SIGINT", "SIGTERM"]) process.on(sig, () => { stopPreview(); process.exit(130); });

/** Resolves once the preview answers (a 200 for `/`). */
async function servedWithin(ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (preview.exitCode !== null) return false;
    const ok = await new Promise((done) => {
      const r = request({ host: "127.0.0.1", port: PORT, path: "/", timeout: 2000 }, (res) => {
        res.resume();
        done(res.statusCode === 200);
      });
      r.on("error", () => done(false));
      r.on("timeout", () => { r.destroy(); done(false); });
      r.end();
    });
    if (ok) return true;
    await new Promise((d) => setTimeout(d, 200));
  }
  return false;
}
if (!(await servedWithin(60_000))) {
  console.error(`vite preview did not start on port ${PORT}`);
  stopPreview();
  process.exit(1);
}

const runs = [];
/** One spec, once: its own Playwright process (and so its own launch of the core). With
 * other specs running beside it, its output is printed in one piece when it ends. */
function runSpec(f, k, live) {
  return new Promise((done) => {
    const s = Date.now();
    // GMCLIP_E2E_SERVED: the config uses the preview above instead of starting its own;
    // GMCLIP_E2E_OUT: Playwright's output folder, its own per spec (each run clears it)
    const p = spawn("pnpm", ["exec", "playwright", "test", `tests/e2e/${f}`], {
      stdio: live ? "inherit" : ["ignore", "pipe", "pipe"],
      shell: win,
      env: { ...process.env, GMCLIP_E2E_PORT: String(PORT), GMCLIP_E2E_SERVED: "1", GMCLIP_E2E_OUT: `test-results/${f.replace(/\.spec\.ts$/, "")}` },
    });
    const out = [];
    if (!live) {
      p.stdout.on("data", (d) => out.push(d));
      p.stderr.on("data", (d) => out.push(d));
    }
    p.on("close", (code) => {
      const ok = code === 0;
      if (!live) process.stdout.write(Buffer.concat(out));
      runs.push({ spec: f, run: k + 1, ok, ms: Date.now() - s });
      console.log(`e2e: ${f}${repeat > 1 ? ` #${k + 1}` : ""} ${ok ? "passed" : "FAILED"} in ${secs(Date.now() - s)}`);
      done();
    });
  });
}

/** Runs the queue `n` at a time. */
async function pool(queue, n) {
  const work = [...queue];
  await Promise.all(
    Array.from({ length: Math.min(n, work.length) }, async () => {
      for (let w = work.shift(); w; w = work.shift()) await runSpec(w.f, w.k, n === 1);
    }),
  );
}

const queue = specs.flatMap((f) => Array.from({ length: repeat }, (_, k) => ({ f, k })));
await pool(queue.filter((w) => !SOLO.has(w.f)), jobs);
await pool(queue.filter((w) => SOLO.has(w.f)), 1);
stopPreview();

const failed = runs.filter((r) => !r.ok);
const slow = [...runs].sort((a, b) => b.ms - a.ms).slice(0, 10);
console.log(`\ne2e summary: ${runs.length - failed.length}/${runs.length} passed; ${jobs} at a time; UI build ${secs(builtMs)}; total ${secs(Date.now() - t0)}`);
console.log(`slowest: ${slow.map((r) => `${r.spec.replace(".spec.ts", "")} ${secs(r.ms)}`).join(", ")}`);
for (const r of failed) console.log(`  failed: ${r.spec}${repeat > 1 ? ` #${r.run}` : ""} (${secs(r.ms)})`);
process.exit(failed.length ? 1 : 0);
