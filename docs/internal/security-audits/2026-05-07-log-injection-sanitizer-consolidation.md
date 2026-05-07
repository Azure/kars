# Security Audit — log-injection sanitizer consolidation

**Date:** 2026-05-07
**PR:** dev → main fast-forward (commit b9e8bd5 / e74faf0)
**Author:** @copilot
**Independent reviewer:** @pallakatos
**Capability scope:**
Tightens the `sanitizeForLog()` helpers in `cli/src/commands/mesh/oauth.ts`
(re-exported as `sanitizeForLog` and used by `cli/src/commands/mesh/auth.ts`)
and the local helper in `cli/src/stepper.ts`. Replaces a chain of
`.replace(/\r/g)` `.replace(/\n/g)` `.replace(/\t/g)` calls (which CodeQL's
`js/log-injection` query did not recognise as a sanitizer) with a single
character-class regex `replace(/[\r\n\t\x1b]/g, " ")`. Also strips the ANSI
escape introducer `\x1b` so attacker-controlled values cannot inject terminal
control sequences into operator logs.

---

## 1. Summary

The CLI logs OAuth flow state and stepper progress that include
attacker-influenced strings (e.g. error messages from upstream identity
providers, peer agent names from registry responses). Without an effective
log-injection sanitizer, an attacker who can influence those strings can
forge log lines or inject ANSI escapes that hide / spoof operator output in
a terminal. This change consolidates two existing sanitizer helpers into a
single regex that CodeQL recognises and additionally strips the ANSI escape
introducer.

## 2. Threat model delta

No new trust boundary. Strengthens the operator-log integrity boundary that
already existed; reduces an `Information Disclosure` / `Tampering` exposure
on log readers (operators / SIEM ingest) introduced by attacker-controlled
strings reaching the `console`.

| STRIDE | New exposure? | Mitigation in this PR |
|---|---|---|
| Spoofing | no | n/a |
| Tampering | reduced | single-regex sanitizer strips `\r \n \t \x1b` |
| Repudiation | reduced | log lines can no longer be split / forged |
| Information Disclosure | no | n/a |
| Denial of Service | no | n/a |
| Elevation of Privilege | no | n/a |

## 3. OWASP mapping

| OWASP item | Applies? | Control in this PR |
|---|---|---|
| LLM05 Improper Output Handling | yes | sanitizer covers logs that may include model / peer output |

## 4. Tests

- `cli` unit tests: 611 pass / 2 skipped (vitest).
- `npx tsc --noEmit` clean.
- CodeQL `js/log-injection` re-scan on next push will confirm the three
  prior findings (oauth.ts:216, stepper.ts:172, stepper.ts:180) are
  cleared.

## 5. Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
