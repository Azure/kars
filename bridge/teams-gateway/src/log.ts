// kars Bridge — Teams Gateway: structured logging with automatic secret redaction.

export type LogLevel = "info" | "warn" | "error" | "debug";

const REDACT_PATTERNS = [
  /secret/i,
  /token/i,
  /authorization/i,
  /password/i,
  /bearer/i,
  /credential/i,
];

function redactValue(key: string, value: unknown): unknown {
  if (typeof value !== "string") return value;
  if (REDACT_PATTERNS.some((p) => p.test(key))) return "[REDACTED]";
  return value;
}

function sanitize(obj: Record<string, unknown>): Record<string, unknown> {
  const result: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(obj)) {
    result[key] = redactValue(key, value);
  }
  return result;
}

export function log(
  level: LogLevel,
  message: string,
  meta?: Record<string, unknown>
): void {
  const entry = {
    ts: new Date().toISOString(),
    level,
    msg: message,
    ...(meta ? sanitize(meta) : {}),
  };
  const line = JSON.stringify(entry);
  if (level === "error") {
    process.stderr.write(line + "\n");
  } else {
    process.stdout.write(line + "\n");
  }
}
