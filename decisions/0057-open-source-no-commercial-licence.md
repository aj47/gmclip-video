# 0057 — Open source; no commercial licence

Date: 2026-11-17 · Decided by: AJ · Status: adopted

## Context
gmclip is published as open source. The spec planned a paid licence with licence keys, an entitlement service, watermarked exports for unlicensed use and a credits service.

## Decision
gmclip is licensed MIT OR Apache-2.0. Licence keys, entitlement validation, the export watermark and credits are removed from the spec, with the requirements and kill criteria that existed only for them (R-CRED-1, R-PROTO-5, C3, K8, the pre-order parts of R-MVP-3 and K6). §21 and §21.1, the owner's question and to-do lists, are not in the public copy. Section numbers are unchanged.

## Sections changed
`spec/SPEC.md` §0 D15, §1.3, §3.3, §10.1, §10.6, §10.8, §12.5, §15 (title, §15.0, §15.2, §15.6), §16, §19.1, §19.5, §20, §21; `spec/security-requirements.md`; `spec/threat-model.md`; `decisions/0001-no-calendar-dates.md`.
