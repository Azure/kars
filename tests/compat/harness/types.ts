// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Types shared by all compat specs.
 * Keep this file small — types only, no runtime code.
 */

export interface ProtectedFlow {
  /** Canonical name per internal Phase 1 plan §5.1 */
  id:
    | "azureclaw-dev"
    | "azureclaw-up"
    | "azureclaw-connect"
    | "azureclaw-handoff"
    | "azureclaw-offload"
    | "azureclaw-operator"
    | "agt-interop"
    | "plugin-lifecycle";
  /** Human-readable description */
  summary: string;
}

export const PROTECTED_FLOWS: ProtectedFlow[] = [
  { id: "azureclaw-dev", summary: "azureclaw dev local Docker sandbox lifecycle" },
  { id: "azureclaw-up", summary: "azureclaw up AKS preflight + provision + helm" },
  { id: "azureclaw-connect", summary: "azureclaw connect attach to running sandbox" },
  { id: "azureclaw-handoff", summary: "azureclaw handoff warm handoff between sibling agents" },
  { id: "azureclaw-offload", summary: "azureclaw offload local → AKS cloud offload" },
  { id: "azureclaw-operator", summary: "azureclaw operator headless TUI dashboard" },
  { id: "agt-interop", summary: "OpenClaw → AzureClaw inter-agent E2E Signal via router" },
  { id: "plugin-lifecycle", summary: "OpenClaw plugin load + tool registration singleton" },
];
