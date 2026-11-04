// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { McpServer } from "../../lib/types/operator";

type ManagedStatus = Pick<McpServer,
  "phase" | "status_current" | "status_reason" | "status_message" | "spec" | "mode"
>;

export function mcpGuidance(server: ManagedStatus) {
  const managed = server.spec ? Boolean(server.spec.managed) : server.mode === "Managed";
  if (!managed || server.status_current !== true) return null;

  if (server.phase === "Ready") {
    return {
      title: "Connection verified",
      detail: "MCP initialization and tool discovery passed. Kars periodically rechecks the service; this does not yet verify an agent-driven tool task.",
    };
  }
  if (server.phase === "Pending") {
    if (server.status_message === "The explicitly configured pull Secret is absent from the managed MCP namespace") {
      return {
        title: "Operator action needed: image access",
        detail: "Ask your cluster operator to restore the configured image-pull Secret in the managed MCP namespace. Kars rechecks automatically, but waiting alone will not supply the missing credential.",
      };
    }
    if (server.status_reason === "ManagedMcpPending") {
      return {
        title: "Waiting for managed service startup",
        detail: "Kars rechecks the deployment automatically, then verifies the MCP connection and tool catalog before marking it Ready. If this persists, ask your operator to check pod startup, image access and available capacity.",
      };
    }
  }
  if (server.phase === "Degraded") {
    if (server.status_reason === "ManagedMcpUnqualified" && (
      server.status_message === "Managed MCP initialize transport failure"
      || server.status_message === "Managed MCP tools/list transport failure"
      || server.status_message === "Managed MCP probe response transport failure"
      || server.status_message === "Managed MCP probe exceeded its total deadline"
    )) {
      return {
        title: "Connection check failed — automatic retries enabled",
        detail: "The service may still be starting or may be unreachable. The controller is configured to retry failed checks about once a minute; it stays unavailable until verification passes. You do not need to remove and re-add it. If failures continue, ask your operator to check workload logs, the Service and network access.",
      };
    }
    return {
      title: "Setup needs attention",
      detail: "Kars rechecks automatically, but this error may require a configuration or operator fix. Review the controller details below; the service stays unavailable until verification passes.",
    };
  }
  return null;
}

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
