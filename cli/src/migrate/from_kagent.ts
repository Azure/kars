// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Phase 2 S9.3 — `azureclaw migrate from-kagent` translator.
//
// Pure translator (no I/O). Reads a kagent.dev/v1alpha2 `Agent` YAML
// document and emits a deterministic AzureClaw resource bundle:
//
//   1. ClawSandbox            (always)
//   2. InferencePolicy        (only if spec.declarative.modelConfig set)
//   3. ToolPolicy             (one per (mcpServer, toolName) pair)
//
// Hard-fails on lossy translation by default; --allow-lossy waives.
// Mirrors the conventions established by S9.2 (`convert`).
//
// Upstream kagent CRD shape verified directly against
// kagent-dev/kagent @ 90212ab go/api/v1alpha2/agent_types.go.
//
// Target CRDs verified directly against:
//   - controller/src/crd.rs (ClawSandbox)
//   - controller/src/inference_policy.rs (InferencePolicy)
//   - controller/src/tool_policy.rs (ToolPolicy)
//
// Aspirational mappings explicitly REJECTED per rubber-duck pre-impl pass:
//   - ClawAgentIdentity (does not exist as a CRD; Phase 4 per
//     docs/internal/internal-boundaries.md:28). The plan line 210
//     mentioning it is overridden by repo reality per slice rule §0.2#7.
//   - McpServer auto-emission (we cannot reconstruct upstream MCP
//     server endpoints from a `TypedReference`).
//   - InferencePolicy enforcement from `modelConfig` (kagent ModelConfig
//     is a separate CRD; we preserve provenance only).

import { createHash } from "node:crypto";

export const KAGENT_API_VERSION = "kagent.dev/v1alpha2";
export const KAGENT_KIND = "Agent";

export const AZURECLAW_GROUP = "azureclaw.azure.com";
export const AZURECLAW_VERSION = "v1alpha1";
export const SANDBOX_LABEL_KEY = `${AZURECLAW_GROUP}/sandbox`;

const PROVENANCE_FROM_KEY = `${AZURECLAW_GROUP}/migrated-from`;
const PROVENANCE_AGENT_KEY = `${AZURECLAW_GROUP}/kagent-agent`;
const KAGENT_DESCRIPTION_KEY = `${AZURECLAW_GROUP}/kagent-description`;
const KAGENT_DESCRIPTION_TRUNCATED_KEY = `${AZURECLAW_GROUP}/kagent-description-truncated`;
const KAGENT_MODEL_CONFIG_KEY = `${AZURECLAW_GROUP}/kagent-model-config`;
const KAGENT_TOOL_REF_KEY = `${AZURECLAW_GROUP}/kagent-tool-ref`;

const DESCRIPTION_CAP_BYTES = 4096;
const DNS_NAME_MAX = 63;
const HASH_SUFFIX_LEN = 6;

export type Severity = "warn" | "error";

export interface Warning {
  severity: Severity;
  message: string;
  /// Dotted JSON path of the source field (e.g. `spec.declarative.memory`).
  path: string;
}

export interface KubeResource {
  apiVersion: string;
  kind: string;
  metadata: {
    name: string;
    namespace: string;
    labels?: Record<string, string>;
    annotations?: Record<string, string>;
  };
  spec: Record<string, unknown>;
}

export interface TranslateOptions {
  /// Override metadata.namespace.
  namespace?: string;
  /// Override sandbox isolation; default `enhanced`.
  isolation?: "standard" | "enhanced" | "confidential";
  /// Override the OpenClaw image. Required (or implied) for Declarative
  /// agents which kagent runs via its own ADK runtime.
  image?: string;
}

export interface TranslateResult {
  warnings: Warning[];
  resources: KubeResource[];
  summary: {
    sandboxName: string;
    namespace: string;
    agentType: "Declarative" | "BYO" | "unknown";
    runnable: boolean;
    toolPolicyCount: number;
    inferencePolicyCount: number;
  };
}

// ---- helpers ---------------------------------------------------------------

function isObj(v: unknown): v is Record<string, unknown> {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

function asString(v: unknown): string | undefined {
  return typeof v === "string" ? v : undefined;
}

function asArray(v: unknown): unknown[] {
  return Array.isArray(v) ? v : [];
}

function warn(path: string, message: string): Warning {
  return { severity: "warn", message, path };
}

/** Sanitize a free-form identifier into a DNS-1123 label-safe lowercase string. */
export function sanitizeDnsName(raw: string): string {
  let s = raw.toLowerCase().replace(/[^a-z0-9-]+/g, "-");
  s = s.replace(/-+/g, "-").replace(/^-+|-+$/g, "");
  return s.length === 0 ? "x" : s;
}

export function hashSuffix(parts: readonly string[]): string {
  const h = createHash("sha256").update(parts.join("\u0000")).digest("hex");
  return h.slice(0, HASH_SUFFIX_LEN);
}

/**
 * Generate a deterministic, DNS-1123-safe ToolPolicy name of length ≤ 63
 * from the (sandbox, mcpServer, tool, sourceIndex) tuple. Hash is computed
 * over the *unsanitized* tuple to avoid collisions when two distinct
 * inputs sanitize identically.
 */
export function generateToolPolicyName(
  sandbox: string,
  mcpServer: string,
  tool: string,
  sourceIndex: number,
): string {
  const suffix = `-${hashSuffix([sandbox, mcpServer, tool, String(sourceIndex)])}`;
  const reserve = suffix.length;
  const prefix = sanitizeDnsName(`${sandbox}-${mcpServer}-${tool}`);
  const trimmed = prefix.slice(0, DNS_NAME_MAX - reserve).replace(/-+$/, "");
  const result = `${trimmed.length === 0 ? "x" : trimmed}${suffix}`;
  // Ensure we end on alphanumeric (suffix is hex → always alnum).
  return result;
}

const SERVER_MANAGED_META_KEYS = new Set([
  "uid",
  "resourceVersion",
  "generation",
  "creationTimestamp",
  "deletionTimestamp",
  "deletionGracePeriodSeconds",
  "managedFields",
  "ownerReferences",
  "finalizers",
  "selfLink",
]);

const KUBECTL_ANNOTATION_PREFIXES = ["kubectl.kubernetes.io/"];

function copyLabels(raw: unknown): Record<string, string> | undefined {
  if (!isObj(raw)) return undefined;
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(raw)) {
    if (typeof v === "string") out[k] = v;
  }
  return Object.keys(out).length > 0 ? out : undefined;
}

function copyAnnotations(raw: unknown): Record<string, string> | undefined {
  if (!isObj(raw)) return undefined;
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(raw)) {
    if (typeof v !== "string") continue;
    if (KUBECTL_ANNOTATION_PREFIXES.some((p) => k.startsWith(p))) continue;
    out[k] = v;
  }
  return Object.keys(out).length > 0 ? out : undefined;
}

/** Returns a metadata block stripped of server-managed fields. */
export function cleanMetadata(raw: unknown): {
  name?: string;
  namespace?: string;
  labels?: Record<string, string>;
  annotations?: Record<string, string>;
} {
  if (!isObj(raw)) return {};
  const out: {
    name?: string;
    namespace?: string;
    labels?: Record<string, string>;
    annotations?: Record<string, string>;
  } = {};
  if (typeof raw.name === "string") out.name = raw.name;
  if (typeof raw.namespace === "string") out.namespace = raw.namespace;
  const labels = copyLabels(raw.labels);
  if (labels) out.labels = labels;
  const annotations = copyAnnotations(raw.annotations);
  if (annotations) out.annotations = annotations;
  // Server-managed keys deliberately dropped (see SERVER_MANAGED_META_KEYS).
  for (const k of Object.keys(raw)) {
    if (SERVER_MANAGED_META_KEYS.has(k)) continue;
  }
  return out;
}

/** Order-aware env projection mirroring convert.ts S9.2 semantics. */
export function envArrayToMap(
  raw: unknown[],
  pathPrefix: string,
  warnings: Warning[],
): Record<string, string> {
  const out: Record<string, string> = {};
  const seenLiteral = new Set<string>();
  for (let i = 0; i < raw.length; i++) {
    const entry = raw[i];
    if (!isObj(entry)) continue;
    const name = asString(entry.name);
    if (!name) continue;
    if (entry.valueFrom !== undefined) {
      if (out[name] !== undefined) {
        warnings.push(
          warn(
            `${pathPrefix}[${i}]`,
            `env '${name}': prior literal dropped because later entry uses valueFrom (unsupported)`,
          ),
        );
        delete out[name];
      } else {
        warnings.push(
          warn(
            `${pathPrefix}[${i}]`,
            `env '${name}': valueFrom is not supported by ClawSandbox.spec.runtime.openclaw.extraEnv (dropped)`,
          ),
        );
      }
      continue;
    }
    if (typeof entry.value !== "string") continue;
    if (seenLiteral.has(name)) {
      warnings.push(
        warn(`${pathPrefix}[${i}]`, `env '${name}' redefined; later value wins`),
      );
    }
    out[name] = entry.value;
    seenLiteral.add(name);
  }
  return out;
}

// ---- description handling --------------------------------------------------

function projectDescription(
  description: string | undefined,
  warnings: Warning[],
): { annotation?: string; truncated?: boolean } {
  if (!description) return {};
  const bytes = Buffer.byteLength(description, "utf8");
  if (bytes <= DESCRIPTION_CAP_BYTES) {
    return { annotation: description };
  }
  warnings.push(
    warn(
      "spec.description",
      `description ${bytes} bytes > ${DESCRIPTION_CAP_BYTES}; truncated`,
    ),
  );
  // Truncate to byte budget; safe for ASCII, and reasonable for UTF-8
  // (we may strip a partial char trailer — fine for an annotation).
  const buf = Buffer.from(description, "utf8").subarray(
    0,
    DESCRIPTION_CAP_BYTES,
  );
  return { annotation: buf.toString("utf8"), truncated: true };
}

// ---- core translate --------------------------------------------------------

export function translate(
  input: unknown,
  opts: TranslateOptions,
): TranslateResult {
  const warnings: Warning[] = [];

  if (!isObj(input)) {
    throw new InvalidInputError("input is not a YAML mapping");
  }
  const apiVersion = asString(input.apiVersion);
  const kind = asString(input.kind);
  if (apiVersion !== KAGENT_API_VERSION) {
    throw new InvalidInputError(
      `apiVersion '${apiVersion ?? ""}' is not '${KAGENT_API_VERSION}'`,
    );
  }
  if (kind !== KAGENT_KIND) {
    throw new InvalidInputError(
      `kind '${kind ?? ""}' is not '${KAGENT_KIND}'`,
    );
  }

  const meta = cleanMetadata(input.metadata);
  const sandboxName = meta.name;
  if (!sandboxName) {
    throw new InvalidInputError("metadata.name is required");
  }
  const inputNs = meta.namespace ?? "default";
  const namespace = opts.namespace ?? inputNs;
  if (opts.namespace && opts.namespace !== inputNs) {
    warnings.push(
      warn(
        "metadata.namespace",
        `overriding input namespace '${inputNs}' with --namespace '${opts.namespace}'`,
      ),
    );
  }

  const spec = isObj(input.spec) ? input.spec : {};
  const agentType = asString(spec.type);
  if (agentType !== "Declarative" && agentType !== "BYO") {
    throw new InvalidInputError(
      `spec.type must be 'Declarative' or 'BYO' (got ${agentType ?? "unset"})`,
    );
  }

  const description = asString(spec.description);
  const descProj = projectDescription(description, warnings);

  // ---- ClawSandbox ---------------------------------------------------------
  const sandboxLabels: Record<string, string> = { ...meta.labels };
  // Inject deterministic AzureClaw-owned label so emitted ToolPolicies
  // can match the sandbox via spec.appliesTo.sandboxMatchLabels. Refuse
  // a conflicting pre-existing value.
  if (
    sandboxLabels[SANDBOX_LABEL_KEY] !== undefined &&
    sandboxLabels[SANDBOX_LABEL_KEY] !== sandboxName
  ) {
    throw new InvalidInputError(
      `metadata.labels['${SANDBOX_LABEL_KEY}'] already set to '${sandboxLabels[SANDBOX_LABEL_KEY]}', conflicts with sandbox name '${sandboxName}'`,
    );
  }
  sandboxLabels[SANDBOX_LABEL_KEY] = sandboxName;

  const sandboxAnns: Record<string, string> = { ...meta.annotations };
  sandboxAnns[PROVENANCE_FROM_KEY] = `${KAGENT_API_VERSION} ${KAGENT_KIND}`;
  sandboxAnns[PROVENANCE_AGENT_KEY] = `${inputNs}/${sandboxName}`;
  if (descProj.annotation) sandboxAnns[KAGENT_DESCRIPTION_KEY] = descProj.annotation;
  if (descProj.truncated) sandboxAnns[KAGENT_DESCRIPTION_TRUNCATED_KEY] = "true";

  const isolation = opts.isolation ?? "enhanced";

  // Image resolution ---------------------------------------------------------
  let image: string | undefined;
  if (agentType === "BYO") {
    const byo = isObj(spec.byo) ? spec.byo : {};
    const byoDeploy = isObj(byo.deployment) ? byo.deployment : {};
    image = asString(byoDeploy.image);
    if (!image) {
      throw new InvalidInputError(
        "spec.byo.deployment.image is required for BYO agents",
      );
    }
    if (byoDeploy.cmd !== undefined) {
      warnings.push(
        warn("spec.byo.deployment.cmd", "container cmd override not supported (dropped)"),
      );
    }
    if (Array.isArray(byoDeploy.args) && byoDeploy.args.length > 0) {
      warnings.push(
        warn("spec.byo.deployment.args", "container args override not supported (dropped)"),
      );
    }
  } else if (agentType === "Declarative") {
    if (opts.image) {
      image = opts.image;
    } else {
      warnings.push(
        warn(
          "spec.declarative",
          "Declarative agents use the kagent ADK runtime image; AzureClaw does not bundle that runtime. Pass --image to override; the emitted ClawSandbox is NOT runnable as-is",
        ),
      );
    }
  }

  // Deployment shared spec ---------------------------------------------------
  const deploySpec =
    agentType === "BYO"
      ? isObj((isObj(spec.byo) ? spec.byo : {}).deployment)
        ? (isObj((spec.byo as Record<string, unknown>).deployment)
            ? ((spec.byo as Record<string, unknown>).deployment as Record<string, unknown>)
            : {})
        : {}
      : isObj((isObj(spec.declarative) ? spec.declarative : {}).deployment)
        ? ((isObj(spec.declarative)
            ? ((spec.declarative as Record<string, unknown>).deployment as Record<
                string,
                unknown
              >)
            : {}) as Record<string, unknown>)
        : {};

  const extraEnv =
    Array.isArray(deploySpec.env) && deploySpec.env.length > 0
      ? envArrayToMap(
          deploySpec.env as unknown[],
          `spec.${agentType === "BYO" ? "byo" : "declarative"}.deployment.env`,
          warnings,
        )
      : undefined;

  // Lossy deployment fields --------------------------------------------------
  const deployBase = `spec.${agentType === "BYO" ? "byo" : "declarative"}.deployment`;
  for (const [k, msg] of [
    ["replicas", "controller manages Deployment replicas (dropped)"],
    ["imagePullSecrets", "imagePullSecrets not exposed via ClawSandbox (dropped)"],
    ["volumes", "pod-level volumes not exposed via ClawSandbox (dropped)"],
    ["volumeMounts", "container-level volumeMounts not exposed (dropped)"],
    ["imagePullPolicy", "imagePullPolicy not exposed (dropped)"],
    ["tolerations", "pod tolerations not exposed via ClawSandbox (dropped)"],
    ["affinity", "pod affinity not exposed via ClawSandbox (dropped)"],
    ["nodeSelector", "nodeSelector not exposed via ClawSandbox (dropped)"],
    ["securityContext", "container securityContext is controller-managed (dropped)"],
    ["podSecurityContext", "podSecurityContext is controller-managed (dropped)"],
    ["serviceAccountName", "ServiceAccount is controller-managed (dropped)"],
    ["serviceAccountConfig", "ServiceAccount config is controller-managed (dropped)"],
  ] as const) {
    if (deploySpec[k] !== undefined && deploySpec[k] !== null) {
      warnings.push(warn(`${deployBase}.${k}`, msg));
    }
  }

  // Resources passthrough ----------------------------------------------------
  let resources: Record<string, unknown> | undefined;
  if (isObj(deploySpec.resources)) {
    resources = {};
    if (deploySpec.resources.requests !== undefined)
      resources.requests = deploySpec.resources.requests;
    if (deploySpec.resources.limits !== undefined)
      resources.limits = deploySpec.resources.limits;
    if (Object.keys(resources).length === 0) resources = undefined;
  }

  // Network policy from kagent sandbox.network.allowedDomains ----------------
  let networkPolicy: Record<string, unknown> | undefined;
  if (isObj(spec.sandbox)) {
    const net = isObj(spec.sandbox.network) ? spec.sandbox.network : undefined;
    const allowed = net && Array.isArray(net.allowedDomains) ? net.allowedDomains : [];
    const endpoints: { host: string }[] = [];
    for (let i = 0; i < allowed.length; i++) {
      const d = allowed[i];
      if (typeof d !== "string" || d.length === 0) continue;
      if (d.includes("*")) {
        warnings.push(
          warn(
            `spec.sandbox.network.allowedDomains[${i}]`,
            `wildcard domain '${d}' has no documented ClawSandbox.networkPolicy semantics; passed through verbatim — verify behaviour before relying on it`,
          ),
        );
      }
      endpoints.push({ host: d });
    }
    if (endpoints.length > 0) {
      networkPolicy = {
        defaultDeny: true,
        approvalRequired: true,
        allowedEndpoints: endpoints,
      };
    }
  }

  // Other lossy top-level kagent fields --------------------------------------
  if (Array.isArray(spec.skills) || isObj(spec.skills)) {
    warnings.push(
      warn(
        "spec.skills",
        "kagent skills (OCI/git refs) are not installed by AzureClaw S9.3; migrated agent may not function without manual remediation",
      ),
    );
  }
  if (spec.allowedNamespaces !== undefined) {
    warnings.push(
      warn(
        "spec.allowedNamespaces",
        "Gateway-API cross-namespace pattern not modeled by AzureClaw (dropped)",
      ),
    );
  }

  // Declarative-only lossy fields --------------------------------------------
  if (agentType === "Declarative" && isObj(spec.declarative)) {
    const d = spec.declarative;
    for (const [k, msg] of [
      ["systemMessage", "system message not modeled by ClawSandbox (use spec.agent.instructions if Foundry agent is configured)"],
      ["systemMessageFrom", "system message source ref not modeled (dropped)"],
      ["promptTemplate", "prompt template processing not modeled (dropped)"],
      ["runtime", "kagent ADK runtime selector (python/go) is not applicable"],
      ["stream", "stream flag is router-side and not modeled here (dropped)"],
      ["executeCodeBlocks", "code execution flag not modeled (dropped)"],
      ["memory", "memory binding is Phase 4 ClawMemory; preserved only as warning"],
      ["context", "context compaction config not modeled (dropped)"],
      ["a2aConfig", "A2A skill list not faithfully translatable to ClawSandbox.spec.a2a (dropped)"],
    ] as const) {
      if (d[k] !== undefined) warnings.push(warn(`spec.declarative.${k}`, msg));
    }
  }

  // ---- ToolPolicies --------------------------------------------------------
  const toolPolicies: KubeResource[] = [];
  let governanceEnabled = false;

  const tools = agentType === "Declarative" && isObj(spec.declarative)
    ? asArray((spec.declarative as Record<string, unknown>).tools)
    : [];

  for (let i = 0; i < tools.length; i++) {
    const tool = tools[i];
    if (!isObj(tool)) continue;
    const tType = asString(tool.type);
    const path = `spec.declarative.tools[${i}]`;

    if (tType === "Agent") {
      if (!isObj(tool.agent) || !asString((tool.agent as Record<string, unknown>).name)) {
        throw new InvalidInputError(
          `${path}.agent: 'name' is required for type=Agent`,
        );
      }
      warnings.push(
        warn(path, "agent-as-tool not supported by AzureClaw ToolPolicy (dropped)"),
      );
      continue;
    }
    if (tType !== "McpServer") {
      throw new InvalidInputError(`${path}.type must be 'McpServer' or 'Agent'`);
    }

    const mcp = isObj(tool.mcpServer) ? tool.mcpServer : undefined;
    if (!mcp) {
      throw new InvalidInputError(`${path}.mcpServer is required for type=McpServer`);
    }
    const mcpName = asString(mcp.name);
    if (!mcpName) {
      throw new InvalidInputError(`${path}.mcpServer.name is required`);
    }

    if (Array.isArray(tool.headersFrom) && tool.headersFrom.length > 0) {
      warnings.push(
        warn(`${path}.headersFrom`, "request-header propagation not modeled by ToolPolicy (dropped)"),
      );
    }
    if (Array.isArray(mcp.allowedHeaders) && mcp.allowedHeaders.length > 0) {
      warnings.push(
        warn(
          `${path}.mcpServer.allowedHeaders`,
          "allowedHeaders not modeled by ToolPolicy (dropped)",
        ),
      );
    }

    const requireApproval = new Set(
      asArray(mcp.requireApproval).filter((x): x is string => typeof x === "string"),
    );

    // Provenance annotation pointing at the original TypedReference.
    const ref = {
      apiGroup: asString(mcp.apiGroup) ?? "",
      kind: asString(mcp.kind) ?? "",
      name: mcpName,
    };
    const refAnn = JSON.stringify(ref);

    // Warn that the referenced McpServer must already exist.
    warnings.push(
      warn(
        `${path}.mcpServer`,
        `Tool references kagent McpServer '${mcpName}'; AzureClaw will not auto-create it — ensure an equivalent AzureClaw McpServer CR exists`,
      ),
    );

    const rawNames = asArray(mcp.toolNames).filter(
      (x): x is string => typeof x === "string" && x.length > 0,
    );

    const toolNames = rawNames.length > 0
      ? Array.from(new Set(rawNames)).sort()
      : ["*"];
    if (rawNames.length === 0) {
      warnings.push(
        warn(
          `${path}.mcpServer.toolNames`,
          "toolNames omitted; emitting one wildcard ToolPolicy (matches all tools from this McpServer)",
        ),
      );
    }

    for (const toolName of toolNames) {
      const tpName = generateToolPolicyName(sandboxName, mcpName, toolName, i);
      const requiresApproval =
        toolName !== "*" && requireApproval.has(toolName);

      const tpSpec: Record<string, unknown> = {
        appliesTo: {
          tool: toolName,
          mcpServer: mcpName,
          sandboxMatchLabels: { [SANDBOX_LABEL_KEY]: sandboxName },
        },
      };
      if (requiresApproval) {
        tpSpec.approval = { mode: "always" };
      }

      toolPolicies.push({
        apiVersion: `${AZURECLAW_GROUP}/${AZURECLAW_VERSION}`,
        kind: "ToolPolicy",
        metadata: {
          name: tpName,
          namespace,
          labels: { [SANDBOX_LABEL_KEY]: sandboxName },
          annotations: {
            [PROVENANCE_FROM_KEY]: `${KAGENT_API_VERSION} ${KAGENT_KIND}`,
            [PROVENANCE_AGENT_KEY]: `${inputNs}/${sandboxName}`,
            [KAGENT_TOOL_REF_KEY]: refAnn,
          },
        },
        spec: tpSpec,
      });
      governanceEnabled = true;
    }
  }

  // Stable bundle order: Sandbox, InferencePolicy, ToolPolicies (sorted).
  toolPolicies.sort((a, b) => a.metadata.name.localeCompare(b.metadata.name));

  // ---- ClawSandbox spec assembly ------------------------------------------
  const sbSpec: Record<string, unknown> = {
    sandbox: { isolation },
  };
  if (image) {
    sbSpec.runtime = {
      kind: "OpenClaw",
      openclaw: { image, ...(extraEnv ? { extraEnv } : {}) },
    };
  } else if (extraEnv) {
    sbSpec.runtime = {
      kind: "OpenClaw",
      openclaw: { extraEnv },
    };
  }
  if (resources) sbSpec.resources = resources;
  if (networkPolicy) sbSpec.networkPolicy = networkPolicy;
  // S13: ClawSandbox.spec.inferenceRef is required. Emit a sibling
  // InferencePolicy CR named `<sandbox>-inference` and reference it.
  sbSpec.inferenceRef = { name: `${sandboxName}-inference` };
  if (governanceEnabled) {
    sbSpec.governance = {
      enabled: true,
      // S13: same-namespace reference to a top-level ToolPolicy CR
      // (per-tool ToolPolicy CRs are still emitted below for fine-grained
      // approval/rate-limit scoping; the sandbox's ref points at the
      // synthetic `<sandbox>-toolpolicy` aggregator).
      toolPolicyRef: { name: `${sandboxName}-toolpolicy` },
    };
  }

  const clawsandbox: KubeResource = {
    apiVersion: `${AZURECLAW_GROUP}/${AZURECLAW_VERSION}`,
    kind: "ClawSandbox",
    metadata: {
      name: sandboxName,
      namespace,
      labels: sandboxLabels,
      annotations: sandboxAnns,
    },
    spec: sbSpec,
  };

  // ---- InferencePolicy ----------------------------------------------------
  // S13: ClawSandbox.spec.inferenceRef is required, so always emit a
  // sibling `<sandbox>-inference` InferencePolicy CR. When the upstream
  // kagent Agent declared a `modelConfig`, preserve it as provenance.
  const inferencePolicies: KubeResource[] = [];
  {
    const ipAnnotations: Record<string, string> = {
      [PROVENANCE_FROM_KEY]: `${KAGENT_API_VERSION} ${KAGENT_KIND}`,
      [PROVENANCE_AGENT_KEY]: `${inputNs}/${sandboxName}`,
    };
    if (agentType === "Declarative" && isObj(spec.declarative)) {
      const modelConfig = asString((spec.declarative as Record<string, unknown>).modelConfig);
      if (modelConfig && modelConfig.length > 0) {
        warnings.push(
          warn(
            "spec.declarative.modelConfig",
            `kagent ModelConfig '${modelConfig}' is preserved only as an InferencePolicy annotation; AzureClaw inference provider/model are not configured from it`,
          ),
        );
        ipAnnotations[KAGENT_MODEL_CONFIG_KEY] = modelConfig;
      }
    }
    inferencePolicies.push({
      apiVersion: `${AZURECLAW_GROUP}/${AZURECLAW_VERSION}`,
      kind: "InferencePolicy",
      metadata: {
        name: `${sandboxName}-inference`,
        namespace,
        labels: { [SANDBOX_LABEL_KEY]: sandboxName },
        annotations: ipAnnotations,
      },
      spec: {
        appliesTo: { sandboxName },
      },
    });
  }

  // S13: when governance is on, emit a synthetic top-level
  // `<sandbox>-toolpolicy` aggregator (the per-tool CRs above stay; this
  // one is what `ClawSandbox.spec.governance.toolPolicyRef` points at).
  if (governanceEnabled) {
    toolPolicies.push({
      apiVersion: `${AZURECLAW_GROUP}/${AZURECLAW_VERSION}`,
      kind: "ToolPolicy",
      metadata: {
        name: `${sandboxName}-toolpolicy`,
        namespace,
        labels: { [SANDBOX_LABEL_KEY]: sandboxName },
        annotations: {
          [PROVENANCE_FROM_KEY]: `${KAGENT_API_VERSION} ${KAGENT_KIND}`,
          [PROVENANCE_AGENT_KEY]: `${inputNs}/${sandboxName}`,
        },
      },
      spec: {
        appliesTo: { sandboxName },
      },
    });
    toolPolicies.sort((a, b) => a.metadata.name.localeCompare(b.metadata.name));
  }

  const resourcesOut: KubeResource[] = [
    clawsandbox,
    ...inferencePolicies,
    ...toolPolicies,
  ];

  return {
    warnings,
    resources: resourcesOut,
    summary: {
      sandboxName,
      namespace,
      agentType,
      runnable: image !== undefined,
      toolPolicyCount: toolPolicies.length,
      inferencePolicyCount: inferencePolicies.length,
    },
  };
}

export class InvalidInputError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "InvalidInputError";
  }
}

export const __test = {
  sanitizeDnsName,
  hashSuffix,
  generateToolPolicyName,
  cleanMetadata,
  envArrayToMap,
  projectDescription,
  translate,
};
