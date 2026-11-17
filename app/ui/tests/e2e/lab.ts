// Lab media and fixtures for the specs that drive AJ's own footage (read only). The
// media is the folder GMCLIP_LAB_MEDIA names (default ~/Development/gmclip-lab-media,
// made by tools/lab_media.sh: symlinks, no copies); the fixtures are committed under
// lab/fixtures. The same rule as the Rust tests' `gmclip_model::lab`.
import { expect, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { basename, isAbsolute, join, resolve } from "node:path";
import { STATE } from "./global-setup";

/** The lab media folder. */
export const LAB = process.env.GMCLIP_LAB_MEDIA || join(homedir(), "Development/gmclip-lab-media");

/** A file in the lab media folder. */
export const labMedia = (rel: string) => join(LAB, rel);

/** Whether every named lab file is on this machine. */
export const hasLab = (...rels: string[]) => rels.every((r) => existsSync(labMedia(r)));

/** The one line a spec skips with when a lab file is missing (the Rust labs' line). */
export const labSkipLine = (rel: string) => `skipped: lab source ${rel} not on this machine`;

/** A committed fixture under lab/fixtures. */
export const labFixture = (rel: string) => resolve("../../lab/fixtures", rel);

/** A copy of the RenderPlan at `path` with its relative paths resolved against the lab
 * media folder (sources, fonts, the reference render), for the import field. */
export function labPlan(path: string): string {
  const plan = JSON.parse(readFileSync(path, "utf8"));
  const fix = (p: string) => (isAbsolute(p) ? p : labMedia(p));
  for (const s of plan.sources ?? []) s.path = fix(s.path);
  for (const f of plan.fonts ?? []) f.path = fix(f.path);
  if (plan.checks?.reference?.path) plan.checks.reference.path = fix(plan.checks.reference.path);
  const out = join(mkdtempSync(join(tmpdir(), "gmclip-lab-plan-")), basename(path));
  writeFileSync(out, JSON.stringify(plan, null, 1));
  return out;
}

/** A project fixture (fixtures/projects/<name>.json in the lab media folder) made a project home in the
 * running app's library by `gmclip-app lab-project`; returns the home. */
export function labProject(name: string): string {
  const s = JSON.parse(readFileSync(STATE, "utf8")) as { home: string };
  const exe = process.platform === "win32" ? ".exe" : "";
  const bin = resolve(process.env.GMCLIP_APP_BIN ?? `../src-tauri/target/debug/gmclip-app${exe}`);
  // Windows keeps app data (the project registry) under %LOCALAPPDATA%: the throwaway one
  const win = process.platform === "win32" ? { LOCALAPPDATA: join(s.home, "AppData", "Local") } : {};
  const env = { ...process.env, HOME: s.home, USERPROFILE: s.home, ...win, GMCLIP_LAB_MEDIA: LAB };
  const out = execFileSync(bin, ["lab-project", labMedia(`fixtures/projects/${name}.json`)], { env, encoding: "utf8" });
  return (JSON.parse(out.trim().split("\n").pop()!) as { home: string }).home;
}

/** Opens a project fixture from Home's project folder field (behind "Open folder…") and
 * goes to its first cut; a fixture with no cut stays on Home, its row shown. */
export async function openLabProject(page: Page, name: string) {
  const home = labProject(name);
  await page.getByTestId("open-folder").click();
  await page.getByLabel("Project folder").fill(home);
  await page.getByLabel("Project folder").press("Enter");
  const row = page.locator(".project").filter({ has: page.getByRole("heading", { name, exact: true }) });
  await expect(row).toBeVisible({ timeout: 30_000 });
  if (await row.locator(".item[data-framed=yes]").count()) await openCutFromHome(page, name);
}

/** A new project named `name` with the RenderPlan at `plan` imported as a cut, the way an
 * agent does it (project_new, cut_import: Home has no import field since decision 0054),
 * then opened from Home's project folder field; leaves the cut on screen and returns its
 * project id. */
export async function importPlanProject(page: Page, name: string, plan: string): Promise<string> {
  type Reply = { ok: boolean; output?: unknown; error?: unknown };
  const call = async <T,>(command: string, args: Record<string, unknown>, project?: string): Promise<T> => {
    const r = await page.evaluate(
      ([c, a, p]) => (globalThis as unknown as { __gmclipBridgeCall: (c: string, a: unknown, p?: string) => Promise<Reply> }).__gmclipBridgeCall(c as string, a, p as string | undefined),
      [command, args, project] as const,
    );
    if (!r.ok) throw new Error(`${command}: ${JSON.stringify(r.error)}`);
    return r.output as T;
  };
  const p = await call<{ id: string; home: string }>("project_new", { name });
  const cut = await call<{ cut: string; title: string }>("cut_import", { path: plan }, p.id);
  await page.getByTestId("open-folder").click();
  await page.getByLabel("Project folder").fill(p.home);
  await page.getByLabel("Project folder").press("Enter");
  await openCutFromHome(page, name, cut.title);
  return p.id;
}

/** On Home: opens the project's cut titled `title` (else its first framed cut) from its row. */
export async function openCutFromHome(page: Page, project: string, title?: string) {
  const row = page.locator(".project").filter({ has: page.getByRole("heading", { name: project, exact: true }) });
  await expect(row).toBeVisible({ timeout: 30_000 });
  const cuts = row.locator(".item[data-framed=yes]");
  const cut = title ? cuts.filter({ hasText: title }).first() : cuts.first();
  await cut.getByRole("button", { name: "Open", exact: true }).click();
  await expect(page.getByTestId("cut-title")).toBeVisible({ timeout: 60_000 });
}
