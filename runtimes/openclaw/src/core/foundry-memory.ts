// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { callPlatformTool } from "./router-client.js";

export const REVIEWED_MEMORY_SCHEMA = {
  type: "object", additionalProperties: false,
  properties: {
    operation: { type: "string", enum: ["search", "update"] },
    text: { type: "string", description: "Memory text to update, or search query." },
    query: { type: "string", description: "Search query alias; empty queries recall within the authorized scope." },
    scope: { type: ["string", "null"], minLength: 1, description: "Omit or use null for the router-selected private scope. Never invent a mission scope or the literal 'private'. A string must be an exact explicitly granted shared scope." },
    top_k: { type: "integer", minimum: 1, maximum: Number.MAX_SAFE_INTEGER },
  }, required: ["operation"],
};

export async function callFoundryMemory(args: Record<string, unknown>, signal?: AbortSignal): Promise<Record<string, any>> {
  const forwarded = { ...args };
  if (forwarded.scope === null) delete forwarded.scope;
  const result = await callPlatformTool("foundry.memory", forwarded, 60000, signal);
  if (result.isError) throw new Error(result.text);
  const value = JSON.parse(result.text);
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Governed memory returned an invalid result");
  }
  return value;
}

export function memoryUpdateSummary(value: Record<string, any>): string {
  if (value.status === "completed") return "completed";
  if (value.status === "queued" || value.status === "in_progress") {
    return `${value.status} (update ${value.update_id}; persistence not yet confirmed)`;
  }
  throw new Error("Governed memory update did not confirm an operation status");
}
