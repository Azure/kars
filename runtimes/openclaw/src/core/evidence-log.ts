// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";
import { appendFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";

const DEFAULT_WORKSPACE = "/sandbox/.openclaw/workspace";
let activeScope: string | null = null;

export type EvidenceEvent = Record<string, unknown> & {
  event: string;
};

function workspaceRoot(): string {
  return process.env.KARS_WORKSPACE_ROOT || DEFAULT_WORKSPACE;
}

export function beginEvidenceScope(scope: string): void {
  activeScope = scope.replace(/[^a-zA-Z0-9._-]/g, "_").slice(0, 96) || "run";
}

export function endEvidenceScope(): void {
  activeScope = null;
}

export function evidenceDigest(value: unknown): string {
  const text = typeof value === "string" ? value : JSON.stringify(value ?? null);
  return `sha256:${createHash("sha256").update(text).digest("hex")}`;
}

export function evidencePreview(value: unknown, max = 240): string {
  const text = (typeof value === "string" ? value : JSON.stringify(value ?? null))
    .replace(/\s+/g, " ")
    .trim();
  return text.length <= max ? text : `${text.slice(0, max - 1)}…`;
}

function appendEvidence(file: string, event: EvidenceEvent): void {
  if (!activeScope) return;
  try {
    const path = join(workspaceRoot(), "artifacts", `.run-${activeScope}`, file);
    mkdirSync(dirname(path), { recursive: true });
    appendFileSync(path, `${JSON.stringify({
      at: new Date().toISOString(),
      agent: process.env.SANDBOX_NAME || process.env.HOSTNAME || "unknown",
      ...event,
    })}\n`, { encoding: "utf8", mode: 0o600 });
  } catch {
    // Evidence capture must never break the governed task path.
  }
}

export function appendCollaborationEvent(event: EvidenceEvent): void {
  appendEvidence("collaboration.jsonl", event);
}
