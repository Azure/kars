// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Shared helpers for the governance-CRD CLI commands
 * (`toolpolicy`, `inferencepolicy`, `mcp`, `a2a-agent`).
 *
 * Pure functions only — no execa, no I/O. Tests import directly.
 */

import * as YAML from "yaml";

export const CRD_API_VERSION = "kars.azure.com/v1alpha1";

// DNS-1123 subdomain for object names.
const DNS_1123_RE = /^[a-z0-9]([-a-z0-9]*[a-z0-9])?$/;

export function validateName(name: string): string[] {
  const errs: string[] = [];
  if (!name || name.length === 0) {
    errs.push("name is required");
    return errs;
  }
  if (name.length > 63) {
    errs.push(`name must be at most 63 characters (got ${name.length})`);
  }
  if (!DNS_1123_RE.test(name)) {
    errs.push(
      `name '${name}' is not a valid DNS-1123 label ` +
        "(lowercase alphanumeric and '-', must start and end with alphanumeric)",
    );
  }
  return errs;
}

/**
 * Parse a list of `key=value` strings into a record. Repeats overwrite.
 * Empty input returns `{}`. Throws on malformed entries.
 */
export function parseKVPairs(items: string[] | undefined): Record<string, string> {
  const out: Record<string, string> = {};
  if (!items) return out;
  for (const raw of items) {
    const eq = raw.indexOf("=");
    if (eq <= 0 || eq === raw.length - 1) {
      throw new Error(`expected 'key=value' but got '${raw}'`);
    }
    const k = raw.slice(0, eq).trim();
    const v = raw.slice(eq + 1).trim();
    if (!k) throw new Error(`empty key in '${raw}'`);
    out[k] = v;
  }
  return out;
}

export interface CRObject {
  apiVersion: string;
  kind: string;
  metadata: { name: string; namespace: string };
  spec: Record<string, unknown>;
}

export function buildCR(
  kind: string,
  name: string,
  namespace: string,
  spec: Record<string, unknown>,
): CRObject {
  return {
    apiVersion: CRD_API_VERSION,
    kind,
    metadata: { name, namespace },
    spec,
  };
}

export function toYaml(obj: unknown): string {
  return YAML.stringify(obj);
}

/** Parse YAML *or* JSON content (autodetect by leading char). */
export function parseSpecFile(content: string): Record<string, unknown> {
  const trimmed = content.trim();
  if (!trimmed) {
    throw new Error("spec file is empty");
  }
  let parsed: unknown;
  if (trimmed.startsWith("{")) {
    parsed = JSON.parse(trimmed);
  } else {
    parsed = YAML.parse(trimmed);
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new Error("spec file must contain a YAML/JSON object");
  }
  const obj = parsed as Record<string, unknown>;
  // Accept either a raw spec block ({appliesTo: ...}) or a full CR
  // ({spec: {...}}).
  if ("spec" in obj && typeof obj.spec === "object" && obj.spec !== null) {
    return obj.spec as Record<string, unknown>;
  }
  return obj;
}

/**
 * Strip undefined values recursively. YAML.stringify happily emits
 * `key: null` when the value is `undefined`, which is *not* what we
 * want for optional CRD fields — kube admission treats `null` as
 * "explicit unset" (RFC 7396) and we want the field omitted entirely.
 */
export function stripUndefined<T>(value: T): T {
  if (Array.isArray(value)) {
    return value
      .map((v) => stripUndefined(v))
      .filter((v) => v !== undefined) as unknown as T;
  }
  if (value && typeof value === "object") {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
      if (v === undefined) continue;
      out[k] = stripUndefined(v);
    }
    return out as unknown as T;
  }
  return value;
}

/** Format an RFC-3339 timestamp as a short relative age (e.g. "3d", "5m"). */
export function formatAge(creationTimestamp: string | undefined, now: Date = new Date()): string {
  if (!creationTimestamp) return "<unknown>";
  const t = Date.parse(creationTimestamp);
  if (Number.isNaN(t)) return "<unknown>";
  let secs = Math.max(0, Math.floor((now.getTime() - t) / 1000));
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 48) return `${hrs}h`;
  const days = Math.floor(hrs / 24);
  return `${days}d`;
}

/** Pad a string to the given width (or return unchanged if already wider). */
export function padCol(s: string, w: number): string {
  if (s.length >= w) return s + "  ";
  return s + " ".repeat(w - s.length) + "  ";
}

export interface TableRow {
  cells: string[];
}

export function formatTable(headers: string[], rows: TableRow[]): string {
  const widths = headers.map((h, i) =>
    Math.max(h.length, ...rows.map((r) => (r.cells[i] ?? "").length)),
  );
  const fmt = (cells: string[]) =>
    cells.map((c, i) => padCol(c, widths[i] ?? 0)).join("").trimEnd();
  const out: string[] = [fmt(headers)];
  for (const r of rows) out.push(fmt(r.cells));
  return out.join("\n");
}
