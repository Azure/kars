// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Types shared by all compat specs.
 * Keep this file small — types only, no runtime code.
 */

export interface ProtectedFlow {
  /** Canonical name per internal Phase 1 plan §5.1 */
  id:
    | "kars-dev"
    | "kars-up"
    | "kars-connect"
    | "kars-handoff"
    | "kars-offload"
    | "kars-operator"
    | "agt-interop"
    | "plugin-lifecycle";
  /** Human-readable description */
  summary: string;
}

export const PROTECTED_FLOWS: ProtectedFlow[] = [
  { id: "kars-dev", summary: "kars dev local Docker sandbox lifecycle" },
  { id: "kars-up", summary: "kars up AKS preflight + provision + helm" },
  { id: "kars-connect", summary: "kars connect attach to running sandbox" },
  { id: "kars-handoff", summary: "kars handoff warm handoff between sibling agents" },
  { id: "kars-offload", summary: "kars offload local → AKS cloud offload" },
  { id: "kars-operator", summary: "kars operator headless TUI dashboard" },
  { id: "agt-interop", summary: "OpenClaw → kars inter-agent E2E Signal via router" },
  { id: "plugin-lifecycle", summary: "OpenClaw plugin load + tool registration singleton" },
];
