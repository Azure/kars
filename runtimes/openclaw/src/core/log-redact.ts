// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Log redaction + sanitization helpers — extracted from plugin.ts in
// S15.f.1 to give plugin.ts headroom under the §4.2 800-LOC cap.
//
// Two related concerns live here:
//   - `redactSecrets` strips obvious secret material (PEM blocks,
//     bearer tokens, JWTs, named credentials) from log strings.
//     Bounded char classes + length limits keep the regex away from
//     catastrophic-backtracking territory (CWE-1333).
//   - `sanitizeLog` strips ANSI escapes / collapses newlines so
//     user-influenced strings cannot break log structure.

// Redact values that look like secrets (tokens, bearer headers, API keys,
// PEM blocks, AzureClaw pairing tokens) before they reach console.* sinks.
// Applied in plugin.ts `_log.info/warn` and at other logging sinks that
// accept interpolated strings. Exported for unit-testing.
export function redactSecrets(m: string): string {
  return String(m)
    // PEM private/public key blocks — redact the full block. Bounded char
    // classes + length limits so this regex cannot exhibit catastrophic
    // backtracking (CWE-1333 ReDoS).
    .replace(/-----BEGIN [A-Z ]{1,40}-----[\s\S]{0,8192}?-----END [A-Z ]{1,40}-----/g, "-----BEGIN ***REDACTED***-----")
    // AzureClaw one-time pairing tokens (azcp_<version>_<base64>)
    .replace(/\bazcp_\d+_[A-Za-z0-9_\-=]+/g, "azcp_***")
    // HTTP Bearer / Basic auth headers
    .replace(/\b(Bearer|Basic)\s+[A-Za-z0-9._\-+/=]+/gi, "$1 ***")
    // JWTs (three dot-separated base64url segments, first starts with eyJ)
    .replace(/\beyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\b/g, "***JWT***")
    // Generic "keyword: value" style (api_key, token, secret, password, authorization,
    // pairing_token, handoff_token, admin_token, invite_code, access_token, refresh_token)
    .replace(
      /\b((?:api[_-]?key|access[_-]?token|refresh[_-]?token|handoff[_-]?token|admin[_-]?token|pairing[_-]?token|invite[_-]?code|token|secret|password|authorization)["':=\s]{1,4})["']?([A-Za-z0-9._\-+/=]{8,})["']?/gi,
      "$1***",
    );
}

// Sanitize user-influenced strings before logging — strip ANSI escapes
// and collapse newlines.
export function sanitizeLog(s: string, maxLen = 500): string {
  return s.replace(/\x1b\[[0-9;]*[a-zA-Z]/g, "").replace(/[\r\n]+/g, " ").slice(0, maxLen);
}
