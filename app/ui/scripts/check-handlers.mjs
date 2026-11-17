// U.17 static check (R-CMD-2/3): UI handlers reach the core only through `actions.ts`,
// and every command `actions.ts` names is one the app declares — an app-shell command
// (`APP_COMMANDS` in app/src-tauri/src/ipc.rs) or a core command (`#[command(name = …)]`
// in crates/gmclip-core). Run on src/ it must pass; run on handler-fixtures/ every
// fixture must fail (each calls the bridge or an undeclared command directly). And every
// app-shell command declared has a match arm that runs it.
//   node scripts/check-handlers.mjs            # src/
//   node scripts/check-handlers.mjs --fixtures # handler-fixtures/*: each must fail
// In actions.ts a direct core call names its generated argument type (A2.01).
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

const ROOT = "../..";
const appCommands = (() => {
  const s = readFileSync(join(ROOT, "app/src-tauri/src/ipc.rs"), "utf8");
  const body = s.slice(s.indexOf("pub const APP_COMMANDS"), s.indexOf("];", s.indexOf("pub const APP_COMMANDS")));
  return new Set([...body.matchAll(/"([a-z0-9_]+)"/g)].map((m) => m[1]));
})();
const coreCommands = (() => {
  const dir = join(ROOT, "crates/gmclip-core/src/commands");
  const out = new Set();
  for (const f of readdirSync(dir)) {
    const s = readFileSync(join(dir, f), "utf8");
    for (const m of s.matchAll(/#\[command\(\s*name = "([a-z0-9_]+)"/g)) out.add(m[1]);
  }
  return out;
})();
const declared = (c) => appCommands.has(c) || coreCommands.has(c);

/** Problems in one source file. */
function check(file, text, isActions) {
  const out = [];
  // direct core access outside actions.ts / ipc.ts
  if (!isActions) {
    for (const re of [/\bcall\s*(<[^>]*>)?\s*\(\s*["'`]/g, /__gmclipBridgeCall/g, /\binvoke\s*\(\s*["'`]gm_call/g, /from\s+["'][./]*ipc["']\s*;?/g]) {
      for (const m of text.matchAll(re)) {
        // store.ts may import the event plumbing (subscribe, onMutation, CallError)
        if (re.source.includes("ipc") && /store\.ts$/.test(file) && !/\bcall\b/.test(text.slice(Math.max(0, m.index - 80), m.index))) continue;
        out.push(`${file}: reaches the core directly (${m[0].trim()}); go through actions.ts`);
      }
    }
  }
  // every command a call names must be declared
  for (const m of text.matchAll(/\bcall\s*(?:<[^()]*?>)?\s*\(\s*"([a-z0-9_]+)"/g)) {
    if (!declared(m[1])) out.push(`${file}: calls "${m[1]}", which neither the app nor the core declares`);
    // A2.01: a direct core call (a core command the app shell does not answer itself)
    // checks its arguments against the generated types: `satisfies Args<"name">` before
    // the next call
    if (isActions && coreCommands.has(m[1]) && !appCommands.has(m[1])) {
      const rest = text.slice(m.index + m[0].length);
      const next = rest.search(/\bcall\s*(?:<[^()]*?>)?\s*\(/);
      const span = next < 0 ? rest : rest.slice(0, next);
      if (!span.includes(`satisfies Args<"${m[1]}">`)) out.push(`${file}: calls the core's "${m[1]}" without \`satisfies Args<"${m[1]}">\` (gen/commands.ts)`);
    }
  }
  return out;
}

function walk(dir) {
  return readdirSync(dir).flatMap((f) => {
    const p = join(dir, f);
    return statSync(p).isDirectory() ? walk(p) : /\.(ts|tsx)$/.test(f) ? [p] : [];
  });
}

if (process.argv.includes("--fixtures")) {
  let bad = 0;
  const files = walk("handler-fixtures");
  for (const f of files) {
    // `actions-*.ts` fixtures are checked as actions.ts is (typed core calls)
    const n = check(f, readFileSync(f, "utf8"), /(^|[\\/])actions-[^\\/]*\.ts$/.test(f)).length;
    console.log(`${n ? "ok  " : "MISS"} ${f}`);
    if (!n) bad++;
  }
  if (bad || !files.length) {
    console.error(`${bad} handler fixture(s) passed the check`);
    process.exit(1);
  }
  console.log(`${files.length}/${files.length} handler fixtures fail the check`);
} else {
  const problems = walk("src").flatMap((f) =>
    check(f, readFileSync(f, "utf8"), /(^|[\\/])(actions|ipc)\.ts$/.test(f)),
  );
  // the MUTATING list names only declared commands too
  const ipc = readFileSync("src/ipc.ts", "utf8");
  const mut = ipc.slice(ipc.indexOf("const MUTATING"), ipc.indexOf("]);", ipc.indexOf("const MUTATING")));
  for (const m of mut.matchAll(/"([a-z0-9_]+)"/g)) if (!declared(m[1])) problems.push(`src/ipc.ts: MUTATING names "${m[1]}", which is not declared`);
  // every app command the shell declares has its match arm (\`"hook_layout_keep"\` was
  // declared, called by a button, and answered "unknown command": U.09, 2026-10-16)
  const shell = readdirSync(join(ROOT, "app/src-tauri/src"))
    .filter((f) => f.endsWith(".rs"))
    .map((f) => readFileSync(join(ROOT, "app/src-tauri/src", f), "utf8"))
    .join("\n");
  for (const c of appCommands) {
    if (!new RegExp(`"${c}"\\s*(\\|[^=]*)?=>|\\|\\s*"${c}"`).test(shell)) problems.push(`app/src-tauri: "${c}" is in APP_COMMANDS but no match arm runs it`);
  }
  for (const p of problems) console.error(p);
  if (problems.length) process.exit(1);
  console.log(`handlers ok: ${appCommands.size} app + ${coreCommands.size} core commands declared; every call in src/ goes through actions.ts; every app command has its match arm`);
}
