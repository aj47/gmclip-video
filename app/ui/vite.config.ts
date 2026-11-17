// Vite for the gmclip webview. Two things beyond the defaults:
// 1. Trusted Types (R-UI-SEC-1). Solid compiles JSX into static `template("<div…>")`
//    strings that it assigns to a <template>'s innerHTML. Under
//    `require-trusted-types-for 'script'` that assignment needs a policy, so the build
//    collects every template literal Solid emitted and the app's `default` policy
//    (src/tt.ts) accepts exactly those strings and nothing else.
// 2. `vite preview` sends the app's CSP, so Playwright runs against the real policy.
import { defineConfig, type Plugin } from "vite";
import solid from "vite-plugin-solid";

const templates = new Set<string>();

function solidTemplateAllowlist(): Plugin {
  return {
    name: "gmclip-solid-template-allowlist",
    enforce: "post",
    transform(code, id) {
      if (!/\.[jt]sx$/.test(id)) return null;
      // Solid emits `_$template(\`…\`)`; parse the module and read cooked literals.
      const ast = this.parse(code) as unknown as { body: unknown[] };
      const walk = (n: unknown): void => {
        if (!n || typeof n !== "object") return;
        const node = n as Record<string, unknown>;
        if (node.type === "CallExpression") {
          const callee = node.callee as Record<string, unknown> | undefined;
          const name = callee && (callee.name as string | undefined);
          if (name && /template$/.test(name)) {
            const arg = (node.arguments as Record<string, unknown>[] | undefined)?.[0];
            if (arg?.type === "TemplateLiteral") {
              const quasis = arg.quasis as { value: { cooked: string } }[];
              if (quasis.length === 1 && quasis[0]) templates.add(quasis[0].value.cooked);
            } else if (arg?.type === "Literal" && typeof arg.value === "string") {
              templates.add(arg.value);
            }
          }
        }
        for (const v of Object.values(node)) {
          if (Array.isArray(v)) v.forEach(walk);
          else if (v && typeof v === "object") walk(v);
        }
      };
      walk(ast);
      return null;
    },
    renderChunk(code) {
      const marker = '"__GMCLIP_SOLID_TEMPLATES__"';
      if (!code.includes(marker)) return null;
      return code.replace(marker, JSON.stringify(JSON.stringify([...templates])));
    },
  };
}

const CSP =
  "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; " +
  "object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'; " +
  "connect-src 'self' ws://127.0.0.1:*; require-trusted-types-for 'script'; trusted-types default";

export default defineConfig({
  plugins: [solid(), solidTemplateAllowlist()],
  clearScreen: false,
  server: { port: 5173, strictPort: true, host: "localhost", fs: { allow: ["../.."] } },
  preview: { headers: { "Content-Security-Policy": CSP } },
  build: { target: "es2022", sourcemap: false, modulePreload: { polyfill: false } },
});
