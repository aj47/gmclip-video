// R-UI-SEC-1: the HTML-sink ban. Every rule here has a fixture in lint-fixtures/ that
// must fail (`pnpm lint:fixtures`).
import tseslint from "typescript-eslint";

const sinks = [
  { property: "innerHTML", message: "HTML sink banned (R-UI-SEC-1): render text with textContent / JSX." },
  { property: "outerHTML", message: "HTML sink banned (R-UI-SEC-1)." },
  { property: "insertAdjacentHTML", message: "HTML sink banned (R-UI-SEC-1)." },
  { object: "document", property: "write", message: "document.write is banned (R-UI-SEC-1)." },
  { object: "document", property: "writeln", message: "document.writeln is banned (R-UI-SEC-1)." },
];

export default tseslint.config(
  { ignores: ["dist/**", "node_modules/**", "test-results/**", "playwright-report/**"] },
  {
    files: ["**/*.{ts,tsx,js,mjs}"],
    languageOptions: {
      parser: tseslint.parser,
      parserOptions: { ecmaFeatures: { jsx: true } },
      globals: { window: "readonly", globalThis: "readonly", document: "readonly", setTimeout: "readonly", setInterval: "readonly" },
    },
    rules: {
      "no-restricted-properties": ["error", ...sinks],
      "no-restricted-syntax": [
        "error",
        { selector: "MemberExpression[property.name=/^(innerHTML|outerHTML|insertAdjacentHTML)$/]", message: "HTML sink banned (R-UI-SEC-1)." },
        { selector: "JSXAttribute[name.name='innerHTML']", message: "Solid's innerHTML prop is banned (R-UI-SEC-1)." },
        { selector: "CallExpression[callee.property.name='setAttribute'][arguments.0.value=/^(on|srcdoc$)/i]", message: "event-handler and srcdoc attributes are banned (R-UI-SEC-1)." },
        { selector: "Property[key.name=/^(innerHTML|outerHTML|srcdoc)$/]", message: "HTML sink banned, also through object spreads and Object.assign (R-UI-SEC-1)." },
        { selector: "JSXAttribute[name.name.name=/^(innerHTML|outerHTML)$/]", message: "Solid's prop:innerHTML is banned (R-UI-SEC-1)." },
        { selector: "AssignmentExpression[left.property.name='srcdoc']", message: "iframe srcdoc is an HTML sink (R-UI-SEC-1)." },
        { selector: "CallExpression[callee.property.name=/^(setHTMLUnsafe|parseHTMLUnsafe|execCommand)$/]", message: "HTML parsing sink banned (R-UI-SEC-1)." },
        { selector: "NewExpression[callee.name='Function']", message: "new Function is banned (R-UI-SEC-1)." },
        { selector: "CallExpression[callee.name=/^(setTimeout|setInterval)$/][arguments.0.type=/^(Literal|TemplateLiteral|BinaryExpression)$/]", message: "string timers are eval (R-UI-SEC-1)." },
        { selector: "CallExpression[callee.object.name=/^(window|globalThis|self)$/][callee.property.name=/^(setTimeout|setInterval)$/][arguments.0.type=/^(Literal|TemplateLiteral|BinaryExpression)$/]", message: "string timers are eval (R-UI-SEC-1)." },
        { selector: "CallExpression[callee.property.name='createContextualFragment']", message: "HTML parsing sink banned (R-UI-SEC-1)." },
        { selector: "CallExpression[callee.property.name='parseFromString']", message: "HTML parsing sink banned (R-UI-SEC-1)." },
      ],
      "no-eval": "error",
      "no-implied-eval": "error",
      "no-new-func": "error",
      "no-script-url": "error",
    },
  },
  {
    // The Trusted Types policy is the one audited place that names the sink.
    files: ["src/tt.ts"],
    rules: { "no-restricted-syntax": "off", "no-restricted-properties": "off" },
  },
);
