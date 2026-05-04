// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * `azureclaw convert` — translate between AzureClaw and upstream
 * `agents.x-k8s.io/v1alpha1` Sandbox manifests (YAML in / YAML out).
 *
 * Phase 2 S9.2: real translator. Phase 0 emitted exit 3.
 *
 * Design doc: `docs/internal/sigs-agent-sandbox-compat.md` (mapping table normative;
 * implementation tracks the actual CRD shape in
 * `controller/src/crd.rs:25-405`).
 *
 * **Targets:**
 *   - `clawsandbox`        — upstream `Sandbox` → `ClawSandbox` (lossy inverse)
 *   - `upstream-sandbox`   — `ClawSandbox` → upstream `Sandbox`  (lossy forward)
 *   - `overlay`            — upstream `Sandbox` → fresh `ClawSandbox`
 *                            skeleton with `spec.upstreamCompatibility` set
 *                            (governance overlay only; pod owned by upstream)
 *
 * **Exit codes:**
 *   0 — success (or `--dry-run` would-succeed)
 *   2 — invalid input / wrong source kind / unsupported target / multi-doc YAML
 *   4 — lossy translation refused without `--allow-lossy`
 *
 * **`--allow-lossy`:** when warnings are produced, exit 4 unless this flag is
 * set. Default is hard-fail because silently dropping governance / inference
 * budget / A2A settings is dangerous (rubber-duck S9.2 critique #1).
 *
 * **`--dry-run`:** validate input + run translation but do not print the output
 * manifest. Still exits 4 if real run would (rubber-duck critique #3).
 *
 * Pure helpers exposed via `__test` for vitest; no IO, no kubectl.
 */
import { Command } from "commander";
import { readFileSync } from "node:fs";
import chalk from "chalk";
import { parse as yamlParse, parseAllDocuments, stringify as yamlStringify } from "yaml";

const CLAW_API_VERSION = "azureclaw.azure.com/v1alpha1";
const CLAW_KIND = "ClawSandbox";
const UPSTREAM_API_VERSION = "agents.x-k8s.io/v1alpha1";
const UPSTREAM_KIND = "Sandbox";

type ConvertTarget = "clawsandbox" | "upstream-sandbox" | "overlay";

const TARGETS: ReadonlyArray<ConvertTarget> = [
  "clawsandbox",
  "upstream-sandbox",
  "overlay",
];

function parseTarget(raw: string | undefined): ConvertTarget | undefined {
  if (!raw) return undefined;
  return (TARGETS as readonly string[]).includes(raw)
    ? (raw as ConvertTarget)
    : undefined;
}

interface TranslateResult {
  manifest: Record<string, unknown>;
  warnings: string[];
}

interface ParsedManifest {
  kind: string;
  apiVersion: string;
  manifest: Record<string, unknown>;
}

/**
 * Parse a single-document YAML manifest. Rejects multi-document streams
 * (rubber-duck critique #6) and missing apiVersion / kind sentinel.
 */
function parseManifest(yaml: string): ParsedManifest {
  const docs = parseAllDocuments(yaml).filter(
    (d) => d.contents !== null && d.contents !== undefined,
  );
  if (docs.length === 0) {
    throw new Error("input YAML is empty");
  }
  if (docs.length > 1) {
    throw new Error(
      `input YAML contains ${docs.length} documents; convert accepts exactly one`,
    );
  }
  const raw = yamlParse(yaml) as unknown;
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) {
    throw new Error("input YAML must be a single mapping (object)");
  }
  const obj = raw as Record<string, unknown>;
  const apiVersion = obj.apiVersion;
  const kind = obj.kind;
  if (typeof apiVersion !== "string" || apiVersion === "") {
    throw new Error("input YAML missing apiVersion");
  }
  if (typeof kind !== "string" || kind === "") {
    throw new Error("input YAML missing kind");
  }
  return { kind, apiVersion, manifest: obj };
}

/**
 * Strip server-managed metadata fields (rubber-duck critique #7).
 * Keeps name + namespace + labels + annotations only.
 */
function cleanMetadata(
  raw: unknown,
): { name?: string; namespace?: string; labels?: unknown; annotations?: unknown } {
  if (raw === null || typeof raw !== "object") return {};
  const m = raw as Record<string, unknown>;
  const out: Record<string, unknown> = {};
  if (typeof m.name === "string") out.name = m.name;
  if (typeof m.namespace === "string") out.namespace = m.namespace;
  if (m.labels !== undefined) out.labels = m.labels;
  if (m.annotations !== undefined) out.annotations = m.annotations;
  return out as { name?: string; namespace?: string; labels?: unknown; annotations?: unknown };
}

/**
 * Forward translation: `ClawSandbox` → upstream `Sandbox`.
 *
 * Lossy on AzureClaw-only fields (governance, inference, a2a, agent,
 * azureServices, networkPolicy, upstreamCompatibility). Each emits a warning.
 *
 * Mirrors controller seccomp/runtimeClass logic
 * (`controller/src/reconciler/mod.rs:34-78`):
 *   - `isolation == "confidential"` → `runtimeClassName: kata-vm-isolation` +
 *     `seccompProfile: RuntimeDefault` (Kata VM provides isolation)
 *   - `seccompProfile == "RuntimeDefault"` or empty → RuntimeDefault
 *   - otherwise → `Localhost` with `profiles/<name>.json`
 */
function clawsandboxToUpstreamSandbox(parsed: ParsedManifest): TranslateResult {
  if (parsed.kind !== CLAW_KIND || !parsed.apiVersion.startsWith("azureclaw.azure.com/")) {
    throw new Error(
      `expected source kind=${CLAW_KIND} apiVersion=azureclaw.azure.com/...; got kind=${parsed.kind} apiVersion=${parsed.apiVersion}`,
    );
  }
  const warnings: string[] = [];
  const m = parsed.manifest;
  const meta = cleanMetadata(m.metadata);
  const spec = (m.spec ?? {}) as Record<string, unknown>;

  if (m.status !== undefined) {
    warnings.push("dropped status block (server-managed)");
  }

  const runtime = (spec.runtime ?? {}) as Record<string, unknown>;
  const runtimeKind = typeof runtime.kind === "string" ? runtime.kind : "OpenClaw";
  if (runtimeKind !== "OpenClaw") {
    throw new Error(
      `ClawSandbox.spec.runtime.kind="${runtimeKind}" cannot be converted to upstream Sandbox; only OpenClaw runtime is supported by the upstream sigs/agent-sandbox shape`,
    );
  }
  const openclaw = (runtime.openclaw ?? {}) as Record<string, unknown>;
  const sandbox = (spec.sandbox ?? {}) as Record<string, unknown>;
  const resources = spec.resources;

  const image = typeof openclaw.image === "string" ? openclaw.image : undefined;
  if (!image) {
    throw new Error("ClawSandbox.spec.runtime.openclaw.image required for upstream-sandbox conversion");
  }

  const env = mapToEnvArray((openclaw.extraEnv ?? {}) as Record<string, unknown>);

  const isolation = typeof sandbox.isolation === "string" ? sandbox.isolation : "enhanced";
  const seccompProfileName = typeof sandbox.seccompProfile === "string"
    ? sandbox.seccompProfile
    : "azureclaw-strict";

  let seccompProfile: Record<string, unknown>;
  if (
    isolation === "confidential" ||
    seccompProfileName === "RuntimeDefault" ||
    seccompProfileName === ""
  ) {
    seccompProfile = { type: "RuntimeDefault" };
  } else {
    seccompProfile = {
      type: "Localhost",
      localhostProfile: `profiles/${seccompProfileName}.json`,
    };
  }

  const containerSecurity: Record<string, unknown> = {};
  if (typeof sandbox.readOnlyRootFilesystem === "boolean") {
    containerSecurity.readOnlyRootFilesystem = sandbox.readOnlyRootFilesystem;
  }
  if (typeof sandbox.runAsNonRoot === "boolean") {
    containerSecurity.runAsNonRoot = sandbox.runAsNonRoot;
  }
  if (typeof sandbox.allowPrivilegeEscalation === "boolean") {
    containerSecurity.allowPrivilegeEscalation = sandbox.allowPrivilegeEscalation;
  }
  containerSecurity.seccompProfile = seccompProfile;

  const container: Record<string, unknown> = {
    name: "openclaw",
    image,
  };
  if (env.length > 0) container.env = env;
  if (resources !== undefined) container.resources = resources;
  if (Object.keys(containerSecurity).length > 0) container.securityContext = containerSecurity;

  const podSpec: Record<string, unknown> = { containers: [container] };
  if (isolation === "confidential") {
    podSpec.runtimeClassName = "kata-vm-isolation";
  }

  const upstreamSpec: Record<string, unknown> = {
    podTemplate: { spec: podSpec },
    replicas: 1,
  };

  // Lossy field warnings — keep the strings stable; tests assert each.
  for (const [key, label] of [
    ["inference", "inference (token budget, content safety, model preference)"],
    ["governance", "governance (toolPolicy, AGT enforcement)"],
    ["a2a", "a2a (inbound A2A 1.2 ingress + AP2 commerce caps)"],
    ["agent", "agent (Foundry prompt agent provisioning)"],
    ["azureServices", "azureServices"],
    ["networkPolicy", "networkPolicy (custom egress allow-list)"],
    ["upstreamCompatibility", "upstreamCompatibility (already upstream-shaped)"],
  ] as const) {
    if (spec[key] !== undefined) {
      warnings.push(`dropped spec.${key}: ${label} has no upstream Sandbox analog`);
    }
  }

  return {
    manifest: {
      apiVersion: UPSTREAM_API_VERSION,
      kind: UPSTREAM_KIND,
      metadata: meta,
      spec: upstreamSpec,
    },
    warnings,
  };
}

/**
 * Convert `extraEnv` map (string→string) to upstream container `env` array.
 * Sorted by key for deterministic output (rubber-duck critique #10).
 */
function mapToEnvArray(extraEnv: Record<string, unknown>): Array<{ name: string; value: string }> {
  const keys = Object.keys(extraEnv).sort();
  const out: Array<{ name: string; value: string }> = [];
  for (const k of keys) {
    const v = extraEnv[k];
    if (typeof v === "string") {
      out.push({ name: k, value: v });
    }
  }
  return out;
}

/**
 * Inverse translation: upstream `Sandbox` → `ClawSandbox`.
 *
 * Lossy on shutdownTime, shutdownPolicy, volumes, volumeClaimTemplates,
 * replicas != 1, multi-container pods, pod-level security/scheduling fields,
 * and `valueFrom` env entries (rubber-duck critique #4).
 */
function upstreamSandboxToClawsandbox(parsed: ParsedManifest): TranslateResult {
  if (parsed.kind !== UPSTREAM_KIND || !parsed.apiVersion.startsWith("agents.x-k8s.io/")) {
    throw new Error(
      `expected source kind=${UPSTREAM_KIND} apiVersion=agents.x-k8s.io/...; got kind=${parsed.kind} apiVersion=${parsed.apiVersion}`,
    );
  }
  const warnings: string[] = [];
  const m = parsed.manifest;
  const meta = cleanMetadata(m.metadata);
  const spec = (m.spec ?? {}) as Record<string, unknown>;

  if (m.status !== undefined) {
    warnings.push("dropped status block (server-managed)");
  }

  const podTemplate = spec.podTemplate as Record<string, unknown> | undefined;
  if (!podTemplate || typeof podTemplate !== "object") {
    throw new Error("upstream Sandbox missing spec.podTemplate");
  }
  const podSpec = podTemplate.spec as Record<string, unknown> | undefined;
  if (!podSpec || typeof podSpec !== "object") {
    throw new Error("upstream Sandbox missing spec.podTemplate.spec");
  }
  const containers = podSpec.containers as unknown;
  if (!Array.isArray(containers) || containers.length === 0) {
    throw new Error("upstream Sandbox podTemplate has no containers");
  }
  if (containers.length > 1) {
    warnings.push(
      `pod has ${containers.length} containers; only the first is mapped (others dropped)`,
    );
  }
  const primary = containers[0] as Record<string, unknown>;
  const image = primary.image;
  if (typeof image !== "string" || image === "") {
    throw new Error("upstream Sandbox primary container missing image");
  }

  const { extraEnv, envWarnings } = envArrayToMap(primary.env);
  warnings.push(...envWarnings);

  const runtimeClass = typeof podSpec.runtimeClassName === "string" ? podSpec.runtimeClassName : "";
  let isolation = "enhanced";
  if (runtimeClass === "kata-vm-isolation") {
    isolation = "confidential";
  } else if (runtimeClass !== "" && runtimeClass !== "azureclaw-runc") {
    warnings.push(
      `unknown runtimeClassName="${runtimeClass}" (expected kata-vm-isolation for confidential); defaulting isolation to enhanced`,
    );
  }

  const ctnSec = primary.securityContext as Record<string, unknown> | undefined;
  const sandboxFields: Record<string, unknown> = { isolation };
  if (ctnSec) {
    if (typeof ctnSec.readOnlyRootFilesystem === "boolean") {
      sandboxFields.readOnlyRootFilesystem = ctnSec.readOnlyRootFilesystem;
    }
    if (typeof ctnSec.runAsNonRoot === "boolean") {
      sandboxFields.runAsNonRoot = ctnSec.runAsNonRoot;
    }
    if (typeof ctnSec.allowPrivilegeEscalation === "boolean") {
      sandboxFields.allowPrivilegeEscalation = ctnSec.allowPrivilegeEscalation;
    }
    const seccompName = canonicaliseSeccomp(ctnSec.seccompProfile, warnings, isolation);
    if (seccompName !== undefined) {
      sandboxFields.seccompProfile = seccompName;
    }
  }

  const openclaw: Record<string, unknown> = { image };
  if (Object.keys(extraEnv).length > 0) {
    openclaw.extraEnv = extraEnv;
  }

  const out: Record<string, unknown> = {
    apiVersion: CLAW_API_VERSION,
    kind: CLAW_KIND,
    metadata: meta,
    spec: {
      runtime: {
        kind: "OpenClaw",
        openclaw,
      },
      sandbox: sandboxFields,
    },
  };
  if (primary.resources !== undefined) {
    (out.spec as Record<string, unknown>).resources = primary.resources;
  }

  // Lossy upstream-only field warnings.
  if (spec.shutdownTime !== undefined) warnings.push("dropped spec.shutdownTime (no ClawSandbox analog)");
  if (spec.shutdownPolicy !== undefined) warnings.push("dropped spec.shutdownPolicy (no ClawSandbox analog)");
  if (spec.volumeClaimTemplates !== undefined) warnings.push("dropped spec.volumeClaimTemplates (no ClawSandbox analog)");
  if (spec.replicas !== undefined && spec.replicas !== 1) {
    warnings.push(`dropped spec.replicas=${String(spec.replicas)} (ClawSandbox always 1)`);
  }
  if (podSpec.volumes !== undefined) warnings.push("dropped podTemplate.spec.volumes");
  if (podSpec.initContainers !== undefined) warnings.push("dropped podTemplate.spec.initContainers");
  if (podSpec.serviceAccountName !== undefined) warnings.push("dropped podTemplate.spec.serviceAccountName (controller manages SA)");
  if (podSpec.hostNetwork === true) warnings.push("dropped podTemplate.spec.hostNetwork=true (forbidden by AzureClaw posture)");
  if (podSpec.hostPID === true) warnings.push("dropped podTemplate.spec.hostPID=true (forbidden by AzureClaw posture)");
  if (podSpec.hostIPC === true) warnings.push("dropped podTemplate.spec.hostIPC=true (forbidden by AzureClaw posture)");
  if (podSpec.nodeSelector !== undefined) warnings.push("dropped podTemplate.spec.nodeSelector (controller picks pool from isolation)");
  if (podSpec.affinity !== undefined) warnings.push("dropped podTemplate.spec.affinity");
  if (podSpec.tolerations !== undefined) warnings.push("dropped podTemplate.spec.tolerations");
  if (podSpec.imagePullSecrets !== undefined) warnings.push("dropped podTemplate.spec.imagePullSecrets");

  // podTemplate.metadata.{labels,annotations} (verified against
  // kubernetes-sigs/agent-sandbox@c8c85f5 api/v1alpha1/sandbox_types.go).
  // ClawSandbox does not model pod-level labels/annotations directly — the
  // controller adds its own.
  const podMeta = podTemplate.metadata as Record<string, unknown> | undefined;
  if (podMeta) {
    if (podMeta.labels !== undefined) {
      warnings.push("dropped podTemplate.metadata.labels (controller manages pod labels)");
    }
    if (podMeta.annotations !== undefined) {
      warnings.push("dropped podTemplate.metadata.annotations (controller manages pod annotations)");
    }
  }

  return { manifest: out, warnings };
}

/**
 * Order-preserving env-array → flat-map projection.
 *
 * Per rubber-duck critique #4: walk in order, treat each entry as a fresh
 * assignment. `valueFrom` cannot be represented in a flat map; if it appears
 * for a name, drop any prior literal (no stale-data resurrection) and warn.
 */
function envArrayToMap(raw: unknown): { extraEnv: Record<string, string>; envWarnings: string[] } {
  const envWarnings: string[] = [];
  const extraEnv: Record<string, string> = {};
  if (!Array.isArray(raw)) return { extraEnv, envWarnings };
  // Track which names have ever had a valueFrom assignment (so collision warning is precise).
  const sawValueFrom = new Set<string>();
  for (const entry of raw) {
    if (entry === null || typeof entry !== "object") continue;
    const e = entry as Record<string, unknown>;
    if (typeof e.name !== "string" || e.name === "") continue;
    const name = e.name;
    if (e.valueFrom !== undefined) {
      // valueFrom assignment: cannot be encoded; remove any prior literal so
      // stale data is not resurrected, and warn.
      if (extraEnv[name] !== undefined) delete extraEnv[name];
      sawValueFrom.add(name);
      envWarnings.push(`env "${name}" uses valueFrom; dropped (extraEnv supports literal values only)`);
      continue;
    }
    if (typeof e.value === "string") {
      if (extraEnv[name] !== undefined) {
        envWarnings.push(`env "${name}" set multiple times; last literal wins`);
      } else if (sawValueFrom.has(name)) {
        envWarnings.push(`env "${name}" later overrides a prior valueFrom with a literal`);
      }
      extraEnv[name] = e.value;
    }
  }
  return { extraEnv, envWarnings };
}

/**
 * Canonicalise inverse seccomp:
 *   - `Localhost { localhostProfile: "profiles/X.json" }` → `X`
 *   - `Localhost { localhostProfile: "X.json" }` → `X` + warn (non-canonical path)
 *   - `Localhost { localhostProfile: "X" }` → `X` + warn
 *   - `RuntimeDefault` → undefined (let CRD default)
 *   - confidential isolation + RuntimeDefault → expected, no warning
 */
function canonicaliseSeccomp(
  raw: unknown,
  warnings: string[],
  isolation: string,
): string | undefined {
  if (!raw || typeof raw !== "object") return undefined;
  const sp = raw as Record<string, unknown>;
  const type = sp.type;
  if (type === "RuntimeDefault") {
    if (isolation !== "confidential") {
      warnings.push(
        "seccompProfile.type=RuntimeDefault on non-confidential pod; mapping to ClawSandbox default (azureclaw-strict)",
      );
    }
    return undefined;
  }
  if (type !== "Localhost") {
    warnings.push(`unknown seccompProfile.type="${String(type)}"; using ClawSandbox default`);
    return undefined;
  }
  const path = sp.localhostProfile;
  if (typeof path !== "string" || path === "") {
    warnings.push("seccompProfile.type=Localhost without localhostProfile; using ClawSandbox default");
    return undefined;
  }
  // Canonical: profiles/<name>.json
  const canonicalMatch = path.match(/^profiles\/([^/]+)\.json$/);
  if (canonicalMatch) {
    return canonicalMatch[1];
  }
  // Tolerate <name>.json or bare <name> with warning.
  const dotJsonMatch = path.match(/^([^/]+)\.json$/);
  if (dotJsonMatch) {
    warnings.push(`seccompProfile.localhostProfile="${path}" lacks profiles/ prefix; canonicalising to "${dotJsonMatch[1]}"`);
    return dotJsonMatch[1];
  }
  if (!path.includes("/") && !path.endsWith(".json")) {
    warnings.push(`seccompProfile.localhostProfile="${path}" not in canonical profiles/<name>.json form; treating as bare profile name`);
    return path;
  }
  warnings.push(`seccompProfile.localhostProfile="${path}" is non-canonical; using ClawSandbox default`);
  return undefined;
}

/**
 * Emit a fresh ClawSandbox skeleton overlay-bound to an upstream Sandbox.
 *
 * Output is purely the governance overlay: namespace + name + an
 * `upstreamCompatibility` block. Pod-template fields (image, env, resources,
 * isolation, security) are all absent because the upstream Sandbox CR owns
 * the pod in overlay mode (see ADR-0001 + `controller/src/crd.rs:88-155`).
 *
 * Per rubber-duck critique #5: validates `--sandbox-ref` namespace matches
 * input.
 */
function emitOverlay(
  parsed: ParsedManifest,
  sandboxRef: string,
): TranslateResult {
  if (parsed.kind !== UPSTREAM_KIND || !parsed.apiVersion.startsWith("agents.x-k8s.io/")) {
    throw new Error(
      `--to overlay requires source kind=${UPSTREAM_KIND}; got kind=${parsed.kind} apiVersion=${parsed.apiVersion}`,
    );
  }
  const warnings: string[] = [];
  const meta = cleanMetadata(parsed.manifest.metadata);

  // Parse ref; accept "name" or "ns/name".
  const slash = sandboxRef.indexOf("/");
  const refNs = slash >= 0 ? sandboxRef.slice(0, slash) : undefined;
  const refName = slash >= 0 ? sandboxRef.slice(slash + 1) : sandboxRef;
  if (refName === "") {
    throw new Error(`--sandbox-ref="${sandboxRef}" is empty after namespace`);
  }
  if (refNs !== undefined && meta.namespace !== undefined && refNs !== meta.namespace) {
    throw new Error(
      `--sandbox-ref namespace "${refNs}" does not match input metadata.namespace "${meta.namespace}" (LocalObjectRef is same-namespace)`,
    );
  }

  const out: Record<string, unknown> = {
    apiVersion: CLAW_API_VERSION,
    kind: CLAW_KIND,
    metadata: { name: meta.name, namespace: meta.namespace },
    spec: {
      upstreamCompatibility: {
        sigsAgentSandbox: "overlay",
        upstreamSandboxRef: { name: refName },
      },
    },
  };

  if (parsed.manifest.status !== undefined) {
    warnings.push("dropped input status (server-managed)");
  }
  warnings.push(
    "overlay skeleton has no governance fields; add spec.governance / spec.inference / spec.a2a / spec.agent before applying",
  );

  return { manifest: out, warnings };
}

function formatYaml(manifest: Record<string, unknown>): string {
  return yamlStringify(manifest, { lineWidth: 0 });
}

interface DispatchOptions {
  target: ConvertTarget;
  sandboxRef?: string;
}

function dispatch(parsed: ParsedManifest, opts: DispatchOptions): TranslateResult {
  switch (opts.target) {
    case "clawsandbox":
      return upstreamSandboxToClawsandbox(parsed);
    case "upstream-sandbox":
      return clawsandboxToUpstreamSandbox(parsed);
    case "overlay":
      if (!opts.sandboxRef || opts.sandboxRef === "") {
        throw new Error("--to overlay requires --sandbox-ref=<name|namespace/name>");
      }
      return emitOverlay(parsed, opts.sandboxRef);
  }
}

export function convertCommand(): Command {
  const cmd = new Command("convert");

  cmd
    .description(
      "Translate between ClawSandbox and upstream agents.x-k8s.io/v1alpha1 Sandbox",
    )
    .requiredOption("-f, --file <path>", "Source manifest YAML")
    .option(
      "--to <target>",
      `Target kind (${TARGETS.join(" | ")})`,
      "clawsandbox",
    )
    .option(
      "--sandbox-ref <ns/name>",
      "For --to overlay: reference to an existing Sandbox CR",
    )
    .option(
      "--dry-run",
      "Validate input + run translation but do not emit the converted manifest",
      false,
    )
    .option(
      "--allow-lossy",
      "Proceed even when the translation drops fields with no analog (default: hard-fail)",
      false,
    )
    .addHelpText(
      "after",
      `
Examples:
  $ azureclaw convert -f sandbox.yaml --to clawsandbox > clawsandbox.yaml
  $ azureclaw convert -f clawsandbox.yaml --to upstream-sandbox --allow-lossy
  $ azureclaw convert -f sandbox.yaml --to overlay --sandbox-ref=prod/web

See docs/internal/sigs-agent-sandbox-compat.md for the normative mapping.
`,
    )
    .action(async (rawOpts: Record<string, unknown>) => {
      const opts = rawOpts as {
        file: string;
        to?: string;
        sandboxRef?: string;
        dryRun: boolean;
        allowLossy: boolean;
      };
      const target = parseTarget(opts.to);
      if (!target) {
        console.error(
          chalk.red(`error: --to must be one of: ${TARGETS.join(", ")} (got ${opts.to})`),
        );
        process.exit(2);
      }

      let yaml: string;
      try {
        yaml = readFileSync(opts.file, "utf8");
      } catch (e) {
        console.error(chalk.red(`error: cannot read ${opts.file}: ${(e as Error).message}`));
        process.exit(2);
      }

      let parsed: ParsedManifest;
      try {
        parsed = parseManifest(yaml);
      } catch (e) {
        console.error(chalk.red(`error: ${(e as Error).message}`));
        process.exit(2);
      }

      let result: TranslateResult;
      try {
        result = dispatch(parsed, { target, sandboxRef: opts.sandboxRef });
      } catch (e) {
        console.error(chalk.red(`error: ${(e as Error).message}`));
        process.exit(2);
      }

      // Always print warnings to stderr.
      for (const w of result.warnings) {
        console.error(chalk.yellow(`warn: ${w}`));
      }

      // Lossy refusal (rubber-duck #1, #3) — applies to dry-run too.
      if (result.warnings.length > 0 && !opts.allowLossy) {
        console.error(
          chalk.red(
            `error: translation is lossy (${result.warnings.length} warning(s)); pass --allow-lossy to proceed`,
          ),
        );
        process.exit(4);
      }

      if (!opts.dryRun) {
        process.stdout.write(formatYaml(result.manifest));
      } else {
        console.error(chalk.dim(`dry-run: would emit ${target} manifest (${result.warnings.length} warning(s))`));
      }
      process.exit(0);
    });

  return cmd;
}

export const __test = {
  parseTarget,
  TARGETS,
  parseManifest,
  cleanMetadata,
  clawsandboxToUpstreamSandbox,
  upstreamSandboxToClawsandbox,
  emitOverlay,
  envArrayToMap,
  canonicaliseSeccomp,
  mapToEnvArray,
  formatYaml,
  dispatch,
};
