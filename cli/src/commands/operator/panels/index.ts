// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Public entry-point of the operator-TUI panels module (S14).
 *
 * Re-exports the `Panel` interface, all built-in panels, the
 * registry/layout helpers, and the data-source abstraction. Consumers
 * should prefer `import { ... } from "./panels/index.js"`.
 */
export type {
  Panel,
  PanelRenderOpts,
  ClusterState,
  CrdCondition,
  CrdItem,
  ClawPairingItem,
  McpServerItem,
  ToolPolicyItem,
  InferencePolicyItem,
  A2AAgentItem,
  ClawMemoryItem,
  ClawEvalItem,
  ProviderState,
  ProviderStatusSnapshot,
} from "./types.js";
export { emptyClusterState } from "./types.js";

export { clawSandboxPanel } from "./clawsandbox.js";
export { clawPairingPanel } from "./clawpairing.js";
export { mcpServerPanel } from "./mcpserver.js";
export { toolPolicyPanel } from "./toolpolicy.js";
export { inferencePolicyPanel } from "./inferencepolicy.js";
export { a2aAgentPanel } from "./a2aagent.js";
export { clawMemoryPanel } from "./clawmemory.js";
export { clawEvalPanel } from "./claweval.js";
export { providerStatusPanel } from "./provider_status.js";

export {
  DEFAULT_PANELS,
  PANEL_BY_ID,
  resolvePanels,
  renderDashboard,
  renderCrdSections,
  renderCrdItemDetail,
  type CrdRow,
  type LayoutOpts,
} from "./layout.js";

export {
  type ClusterDataSource,
  FixtureDataSource,
  KubectlDataSource,
} from "./datasource.js";

export { isSensitiveKey, redactValue, redactObject } from "./redact.js";
