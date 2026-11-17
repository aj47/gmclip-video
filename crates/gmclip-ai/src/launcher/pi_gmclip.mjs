// gmclip's bridge for pi (SPEC §10.7, decisions/0012). pi has no MCP client, so the
// launcher writes this file into the run's own config folder and loads it with
// `--no-extensions -e <this file>`. It starts the `gmclip mcp` shim (pi's environment,
// which carries GMCLIP_RUN, passes through), lists gmclip's tools and registers only the
// run's allow-listed ones, under their `mcp__gmclip__` names; pi's `--tools` lists the
// same names, so nothing else is active. `GMCLIP` (command, args, tools, prefix) is the
// line the launcher prepends. No network, no files, no other process.
import { spawn } from "node:child_process";
import { Type } from "typebox";

export default async function (pi) {
  const child = spawn(GMCLIP.command, GMCLIP.args, {
    stdio: ["pipe", "pipe", "inherit"],
    env: process.env,
  });
  const pending = new Map();
  let next = 1;
  let buf = "";
  child.stdout.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    buf += chunk;
    for (let i = buf.indexOf("\n"); i >= 0; i = buf.indexOf("\n")) {
      const line = buf.slice(0, i).trim();
      buf = buf.slice(i + 1);
      if (!line) continue;
      let m;
      try {
        m = JSON.parse(line);
      } catch {
        continue;
      }
      const p = m.id !== undefined ? pending.get(m.id) : undefined;
      if (!p) continue;
      pending.delete(m.id);
      if (m.error) p.reject(new Error(String(m.error.message ?? "gmclip refused the call")));
      else p.resolve(m.result ?? {});
    }
  });
  const stopped = () => {
    for (const p of pending.values()) p.reject(new Error("the gmclip server stopped"));
    pending.clear();
  };
  child.on("exit", stopped);
  child.on("error", stopped);
  const send = (msg) => child.stdin.write(JSON.stringify({ jsonrpc: "2.0", ...msg }) + "\n");
  const rpc = (method, params) =>
    new Promise((resolve, reject) => {
      const id = next++;
      pending.set(id, { resolve, reject });
      send({ id, method, params });
    });

  await rpc("initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "pi", version: "gmclip-launcher" },
  });
  send({ method: "notifications/initialized" });
  const listed = [];
  let cursor;
  do {
    const page = await rpc("tools/list", cursor ? { cursor } : {});
    listed.push(...(page.tools ?? []));
    cursor = page.nextCursor;
  } while (cursor);

  const allow = new Set(GMCLIP.tools);
  for (const t of listed) {
    if (!allow.has(t.name)) continue;
    const schema = t.inputSchema ?? { type: "object", properties: {} };
    pi.registerTool({
      name: GMCLIP.prefix + t.name,
      label: t.name,
      description: t.description ?? "",
      parameters: typeof Type.Unsafe === "function" ? Type.Unsafe(schema) : schema,
      async execute(_callId, params) {
        const r = await rpc("tools/call", { name: t.name, arguments: params ?? {} });
        const content = (r.content ?? []).map((c) =>
          c.type === "image"
            ? { type: "image", data: c.data, mimeType: c.mimeType }
            : { type: "text", text: c.type === "text" ? c.text : JSON.stringify(c) },
        );
        if (r.isError) {
          const text = content.filter((c) => c.type === "text").map((c) => c.text).join("\n");
          throw new Error(text || "gmclip refused the call");
        }
        return { content, details: {} };
      },
    });
  }
}
