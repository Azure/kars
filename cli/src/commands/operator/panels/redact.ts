/**
 * Secret-redaction helpers for the operator TUI panels (S14).
 *
 * Per plan §0.2: TUI is read-only and **must not** render secret data raw.
 * ConfigMap / env values whose keys match common credential patterns
 * collapse to `<present>` / `<missing>` strings; values are never echoed.
 */

const SECRET_KEY_PATTERN = /(KEY|TOKEN|SECRET|PASSWORD|CREDENTIAL|JWKS|PRIVATE)/i;

/** Returns true when a config-key name should never be rendered raw. */
export function isSensitiveKey(key: string): boolean {
  return SECRET_KEY_PATTERN.test(key);
}

/** Render a value safely — sensitive keys collapse to a presence marker. */
export function redactValue(key: string, value: string | undefined): string {
  if (isSensitiveKey(key)) {
    if (value === undefined || value === "") return "<missing>";
    return "<present>";
  }
  return value ?? "";
}

/** Reduce an arbitrary object to a redacted preview (key=value list). */
export function redactObject(obj: Record<string, unknown>): string[] {
  const out: string[] = [];
  for (const [k, v] of Object.entries(obj)) {
    const display = isSensitiveKey(k)
      ? (v === undefined || v === null || v === "" ? "<missing>" : "<present>")
      : (typeof v === "string" ? v : JSON.stringify(v));
    out.push(`${k}=${display}`);
  }
  return out;
}
