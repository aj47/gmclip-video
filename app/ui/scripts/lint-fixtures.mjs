// Every fixture in lint-fixtures/ uses one banned sink and must FAIL the lint
// (R-UI-SEC-1 acceptance: "CI lint fixtures for each banned sink fail").
import { ESLint } from "eslint";
import { readdirSync } from "node:fs";

const eslint = new ESLint({ overrideConfig: { files: ["lint-fixtures/**"] } });
const files = readdirSync("lint-fixtures").map((f) => `lint-fixtures/${f}`);
let bad = 0;
for (const f of files) {
  const [r] = await eslint.lintFiles([f]);
  // Only a ban counts: a parse error or an unrelated rule is not a caught sink.
  const bans = new Set(["no-restricted-properties", "no-restricted-syntax", "no-eval", "no-implied-eval", "no-new-func"]);
  const fatal = r?.messages.some((m) => m.fatal) ?? true;
  const n = fatal ? 0 : (r?.messages.filter((m) => bans.has(m.ruleId ?? "")).length ?? 0);
  console.log(`${n > 0 ? "ok  " : "MISS"} ${f} (${n} error${n === 1 ? "" : "s"})`);
  if (n === 0) bad++;
}
if (bad) {
  console.error(`${bad} fixture(s) passed the lint: a banned sink is not caught`);
  process.exit(1);
}
console.log(`${files.length}/${files.length} banned-sink fixtures fail the lint`);
