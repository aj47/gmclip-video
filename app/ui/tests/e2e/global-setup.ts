// Launches the core with the dev bridge and a throwaway HOME; hands the bridge's port
// and token to the tests through a file the tests read (never a URL, R-DEV-1).
import { execFileSync, spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";

export const STATE = process.env.GMCLIP_E2E_STATE ?? join(tmpdir(), "gmclip-e2e-state.json");

export default async function setup() {
  const exe = process.platform === "win32" ? ".exe" : "";
  const bin = resolve(process.env.GMCLIP_APP_BIN ?? `../src-tauri/target/debug/gmclip-app${exe}`);
  // GMCLIP_E2E_HOME: a home kept between runs (the QA workflows reuse a long stream's
  // transcript instead of transcribing it again); the teardown leaves it in place.
  const home = process.env.GMCLIP_E2E_HOME ?? mkdtempSync(join(tmpdir(), "gmclip-e2e-home-"));
  mkdirSync(home, { recursive: true });
  // The speech models (read only) from the real cache, so opened videos get transcripts.
  // Only `models/` is linked: the rest of the cache (scrub proxies, H.05) stays in the
  // throwaway HOME.
  const models = join(homedir(), "Library/Caches/gmclip/models");
  if (existsSync(models) && !existsSync(join(home, "Library/Caches/gmclip/models"))) {
    mkdirSync(join(home, "Library/Caches/gmclip"), { recursive: true });
    symlinkSync(models, join(home, "Library/Caches/gmclip/models"));
  }
  // Windows: the cache is %LOCALAPPDATA%\gmclip (the throwaway home gets its own
  // LOCALAPPDATA below); a directory junction needs no admin or Developer Mode
  const winModels = process.platform === "win32" && process.env.LOCALAPPDATA ? join(process.env.LOCALAPPDATA, "gmclip", "models") : null;
  const winLink = join(home, "AppData", "Local", "gmclip", "models");
  if (winModels && existsSync(winModels) && !existsSync(winLink)) {
    mkdirSync(join(home, "AppData", "Local", "gmclip"), { recursive: true });
    symlinkSync(winModels, winLink, "junction");
  }
  // and the user's own fonts (%LOCALAPPDATA%\Microsoft\Windows\Fonts, where a per-user
  // install puts them; read by hash, as ~/Library/Fonts on a Mac)
  const winFonts = process.platform === "win32" && process.env.LOCALAPPDATA ? join(process.env.LOCALAPPDATA, "Microsoft", "Windows", "Fonts") : null;
  const winFontsLink = join(home, "AppData", "Local", "Microsoft", "Windows", "Fonts");
  if (winFonts && existsSync(winFonts) && !existsSync(winFontsLink)) {
    mkdirSync(join(home, "AppData", "Local", "Microsoft", "Windows"), { recursive: true });
    symlinkSync(winFonts, winFontsLink, "junction");
  }
  // The user's fonts (read only): the app finds a face it knows by SHA-256 in
  // ~/Library/Fonts (assets/fonts/faces.json), so the throwaway HOME links the real folder.
  const fonts = join(homedir(), "Library/Fonts");
  if (process.platform !== "win32" && existsSync(fonts) && !existsSync(join(home, "Library/Fonts"))) {
    mkdirSync(join(home, "Library"), { recursive: true });
    symlinkSync(fonts, join(home, "Library/Fonts"));
  }
  const origin = `http://127.0.0.1:${process.env.GMCLIP_E2E_PORT ?? 4173}`;
  // Its own temp dir (the agent server's socket lives there, apart from any running app)
  // and the zero-spend stub model agent runs talk to: the find-moments pass contract (A.01)
  // — begin, submit one moment by line and quote (gmclip anchors it), an empty sweep.
  const tmp = mkdtempSync(join(tmpdir(), "gmclip-e2e-tmp-"));
  const turns = join(tmp, "stub-turns.json");
  writeFileSync(turns, JSON.stringify([
    [["mcp__gmclip__ai_pass", { op: "begin_self", pass: "find_moments", target: {} }]],
    [["mcp__gmclip__pass_step", { pass_run: "{{pass_run}}", submit: [
      { start_line: "L1", end_line: "L1", quote: "We were able to generate that five second video in less than two minutes.", score: 4, type: "result", why: "a measured result (stub model)" },
    ] }]],
    [["mcp__gmclip__pass_step", { pass_run: "{{pass_run}}", submit: [] }]],
  ]));
  // GMCLIP_E2E_FAKE_CLIENTS: the four agent clients are scripted stand-ins (no model),
  // set as the clients' paths (an Owner setting: auto-detection never picks a script), each
  // recorded as allowed by the Owner for that exact file, as "Run anyway" on the sheet would
  // (the dev bridge treats every build as new, GMCLIP_TEST_CLIENT_IS_NEW).
  if (process.env.GMCLIP_E2E_FAKE_CLIENTS) {
    const bindir = join(tmp, "fake-bin");
    mkdirSync(bindir);
    const script = resolve("tests/e2e/fixtures/fake-client.py");
    const rows = [];
    const paths: Record<string, string> = {};
    for (const [file, harness, version] of [["claude", "claude-code", "2.1.300"], ["codex", "codex", "0.161.0"], ["pi", "pi", "0.70.2"], ["opencode", "opencode", "1.2.3"]] as const) {
      // a native exec wrapper (the launcher refuses script clients): python3 <script> <file> <args…>
      let f: string;
      if (process.platform === "win32") {
        // Windows: a Rust wrapper (rustc comes with the toolchain); Python from
        // GMCLIP_E2E_PYTHON (no python3 on PATH there)
        const py = process.env.GMCLIP_E2E_PYTHON;
        if (!py) throw new Error("GMCLIP_E2E_FAKE_CLIENTS on Windows needs GMCLIP_E2E_PYTHON (a python.exe)");
        const rs = join(bindir, `${file}.rs`);
        writeFileSync(rs, `fn main() { let s = std::process::Command::new(${JSON.stringify(py)}).arg(${JSON.stringify(script)}).arg("${file}").args(std::env::args_os().skip(1)).status(); std::process::exit(s.map_or(127, |s| s.code().unwrap_or(1))); }\n`);
        f = join(bindir, `${file}.exe`);
        execFileSync("rustc", ["-O", "-o", f, rs]);
      } else {
        const c = join(bindir, `${file}.c`);
        writeFileSync(c, `#include <unistd.h>\nint main(int argc, char **argv) { char *a[argc + 3]; a[0] = "/usr/bin/python3"; a[1] = ${JSON.stringify(script)}; a[2] = "${file}"; for (int i = 1; i < argc; i++) a[i + 2] = argv[i]; a[argc + 2] = 0; execv(a[0], a); return 127; }\n`);
        f = join(bindir, file);
        execFileSync("cc", ["-O0", "-o", f, c]);
      }
      const body = readFileSync(f);
      paths[harness === "claude-code" ? "claude_code" : harness] = f;
      rows.push({ harness, version, sha256: createHash("sha256").update(body).digest("hex"), exec: f, at_ms: Date.now(), result: "owner_allowed", checks: [] });
    }
    const appData = process.platform === "win32" ? join(home, "AppData", "Local", "gmclip") : join(home, "Library/Application Support/gmclip");
    mkdirSync(appData, { recursive: true });
    writeFileSync(join(appData, "verified-clients.json"), JSON.stringify(rows));
    writeFileSync(join(appData, "agent-client.json"), JSON.stringify(paths));
  }
  // Windows keeps app data under %LOCALAPPDATA% (and the profile under USERPROFILE):
  // both into the throwaway home too, never the real ones
  const win = process.platform === "win32"
    ? { USERPROFILE: home, LOCALAPPDATA: join(home, "AppData", "Local"), TEMP: tmp, TMP: tmp }
    : {};
  // What the analysis heard, kept across launches and runs (src-tauri/src/analysiscache.rs):
  // each test video is transcribed and measured once, not once per spec. Keyed by the
  // video's bytes, the models and the analysis code, so it never goes stale; set
  // GMCLIP_ANALYSIS_CACHE=off to hear everything again (the app ignores a relative path).
  const cache = process.env.GMCLIP_ANALYSIS_CACHE ?? resolve("../../target/e2e-analysis-cache");
  const analysisCache = cache === "off" ? {} : { GMCLIP_ANALYSIS_CACHE: resolve(cache) };
  const child = spawn(bin, ["--dev-bridge", "--no-window", "--dev-bridge-origin", origin], {
    // decision 0035: agent sessions speak to a scripted ACP agent (no model, no network)
    env: { ...process.env, HOME: home, TMPDIR: tmp, ...win, GMCLIP_AGENT_STUB_TURNS: turns, ...(process.platform === "win32" ? {} : { GMCLIP_ACP_FAKE: resolve("tests/e2e/fixtures/fake-acp.sh") }), GMCLIP_TEST_CLIENT_IS_NEW: "1", GMCLIP_PLAYBACK_SILENT: "1", ...analysisCache },
    stdio: ["ignore", "pipe", "inherit"],
  });
  const info = await new Promise<{ port: number; token: string; frame_port: number }>((ok, fail) => {
    const timer = setTimeout(() => {
      child.kill("SIGTERM");
      fail(new Error("gmclip-app did not print its dev bridge line"));
    }, 30_000);
    let buf = "";
    child.stdout.on("data", (d: Buffer) => {
      buf += d.toString();
      const m = /GMCLIP_DEV_BRIDGE (\{.*\})/.exec(buf);
      if (m?.[1]) {
        clearTimeout(timer);
        ok(JSON.parse(m[1]));
      }
    });
    child.on("exit", (c) => fail(new Error(`gmclip-app exited (${c})`)));
  });
  writeFileSync(STATE, JSON.stringify({ ...info, pid: child.pid, home }));
  child.unref();
}
