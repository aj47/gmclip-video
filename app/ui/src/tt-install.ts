// Imported first by main.tsx so the Trusted Types policy exists before any module that
// builds a Solid template is evaluated.
import { installTrustedTypes } from "./tt";

document.documentElement.dataset.tt = installTrustedTypes();
