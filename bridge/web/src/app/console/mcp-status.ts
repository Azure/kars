// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { McpServer } from "../../lib/types/operator";

export function mcpStatus(server: Pick<McpServer,
  "phase" | "status_current" | "status_reason" | "status_message"
>) {
  if (server.status_current !== true) {
    return {
      label: server.status_current === false ? "Reconciling" : "Unverified",
      tone: "warn" as const,
      reason: null,
      detail: server.status_current === false
        ? "Waiting for a current controller readiness report. Earlier status is not proof that this installation is ready."
        : "Controller readiness details are unavailable from this Bridge API.",
      ready: false,
    };
  }
  return {
    label: server.phase ?? "Pending",
    tone: server.phase === "Ready" ? "ok" as const
      : server.phase === "Degraded" ? "danger" as const : "warn" as const,
    reason: server.status_reason ?? null,
    detail: server.status_message || "The controller has not provided a readiness explanation.",
    ready: server.phase === "Ready",
  };
}
