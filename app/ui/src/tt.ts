// Trusted Types (R-UI-SEC-1). The CSP requires Trusted Types for script sinks and allows
// one policy, `default`. Solid's compiled templates are the only HTML the app ever
// parses; the build (vite.config.ts) writes the exact list of them here, and the policy
// accepts those strings and refuses everything else. App code never names an HTML sink
// (the ESLint ban), so anything reaching this policy that is not a template is a bug or
// an injection, and throws.
const TEMPLATES = "__GMCLIP_SOLID_TEMPLATES__";

interface TTFactory {
  createPolicy(name: string, rules: { createHTML?: (s: string) => string }): unknown;
}

export function installTrustedTypes(): "enforced" | "unsupported" | "dev" {
  const tt = (globalThis as unknown as { trustedTypes?: TTFactory }).trustedTypes;
  if (!tt) return "unsupported";
  let allowed: Set<string> | null = null;
  try {
    allowed = new Set(JSON.parse(TEMPLATES) as string[]);
  } catch {
    allowed = null;
  }
  const dev = allowed === null && import.meta.env.DEV;
  tt.createPolicy("default", {
    createHTML: (s: string) => {
      if (dev || allowed?.has(s)) return s;
      throw new TypeError("gmclip: refused HTML that is not one of the app's templates (Trusted Types)");
    },
  });
  return dev ? "dev" : "enforced";
}
