// Phase 2 S11 — `azureclaw attest <name>` CLI subcommand.
//
// **Read surface only.** Phase 2 ships the *consumer* that prints whatever
// attestation evidence the controller already records on the cluster;
// Phase 3 lands the actual signed reconcile audit chain (cosign-signed
// receipts, AGT AuditLogger receipt IDs, verifiable signatures).
//
// What this command surfaces today:
//
//   1. Spec hash — deterministic SHA-256 over a canonicalised JSON of the
//      `ClawSandbox.spec` (recursive key-sort, no whitespace). The same
//      hash recipe is used by the `versionHash` fields shipped on the
//      five Phase 2 policy CRDs (McpServer / ToolPolicy / A2AAgent /
//      InferencePolicy / ClawEval — all use SHA-256 over canonical
//      serde-json), so a future signed audit chain can compose them
//      without rewriting the hashing layer.
//   2. Observed-generation lineage — `metadata.generation` vs
//      `status.observedGeneration` plus `status.phase`. Lets operators
//      tell "spec applied" from "spec accepted but not yet reconciled".
//   3. SSA field-owner map — the unique `manager` names from the
//      ClawSandbox CR's `metadata.managedFields` plus a per-manager
//      `fields-owned` count. Surfaces "who edited this object last"
//      without dumping the full SSA tree.
//   4. Referenced policy versions — for every policy CRD referenced by
//      `ClawSandbox.spec` (ToolPolicy, InferencePolicy, A2AAgent — and
//      indirectly the McpServers that ToolPolicy references), the
//      command resolves the referenced object and prints its
//      `status.versionHash` if present, plus the binding ConfigMap name.
//   5. Reconcile trace ID — best-effort lookup from the sandbox-namespace
//      Deployment's `azureclaw.azure.com/last-trace-id` annotation.
//      Phase 2 controller does not yet stamp this annotation; the field
//      prints `(Phase 3)` when absent. The lookup is in place so flipping
//      the controller to emit it does not require a CLI change.
//   6. AGT audit-receipt id + verifiable signature — `(Phase 3)` today;
//      the print scaffolding exists so the eventual emitter does not
//      require a CLI change.
//
// The output is **deterministic and machine-grep-able** under
// `--format json` (see `formatJson`); the human pretty-printer is best-
// effort.

import { Command } from "commander";
import chalk from "chalk";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";

const POLICY_CR_KINDS: ReadonlyArray<{
  kind: string;
  plural: string;
  refField: string;
}> = [
  // S13: spec-level refs.
  { kind: "ToolPolicy", plural: "toolpolicies", refField: "toolPolicyRef" },
  {
    kind: "InferencePolicy",
    plural: "inferencepolicies",
    refField: "inferenceRef",
  },
  {
    kind: "InferencePolicy",
    plural: "inferencepolicies",
    refField: "inferencePolicyRef",
  },
  { kind: "A2AAgent", plural: "a2aagents", refField: "a2aAgentRef" },
];

interface AttestationReport {
  apiVersion: "azureclaw.azure.com/v1alpha1-attest";
  kind: "Attestation";
  generatedAt: string;
  sandbox: {
    name: string;
    namespace: string;
    generation: number | null;
    observedGeneration: number | null;
    phase: string | null;
    specHash: string;
    specHashAlgorithm: "sha256-canonical-json";
  };
  fieldOwners: Array<{ manager: string; fieldsOwned: number }>;
  policyVersions: Array<{
    kind: string;
    name: string;
    namespace: string;
    versionHash: string | null;
    bindingConfigMap: string | null;
  }>;
  reconcileTraceId: string | null;
  agtAuditReceiptId: string | null;
  signature: string | null;
  baselineDiff?: AttestationDiff;
}

/** Typed deltas surfaced when `--baseline` is supplied. Each variant is
 *  the smallest unit a CI gate / change-control reviewer cares about:
 *  one human-meaningful change per delta. */
export type AttestationDelta =
  | { type: "specHash"; before: string; after: string }
  | { type: "phase"; before: string | null; after: string | null }
  | {
      type: "policyVersionHash";
      kind: string;
      name: string;
      before: string | null;
      after: string | null;
    }
  | { type: "policyAdded"; kind: string; name: string }
  | { type: "policyRemoved"; kind: string; name: string }
  | { type: "fieldOwnerAdded"; manager: string }
  | { type: "fieldOwnerRemoved"; manager: string };

export interface AttestationDiff {
  baseline: { generatedAt: string; specHash: string };
  current: { generatedAt: string; specHash: string };
  deltas: AttestationDelta[];
  drift: boolean;
}

/** Recursive key-sort + minimal serialisation. Numbers/strings/booleans
 *  pass through unchanged; arrays preserve order; objects emit keys in
 *  ASCII-sorted order. Mirrors `serde_json` `BTreeMap` round-trip used
 *  by every Phase 2 `versionHash` computation. */
export function canonicalJson(value: unknown): string {
  if (value === null || value === undefined) return "null";
  if (typeof value === "number" || typeof value === "boolean") {
    return JSON.stringify(value);
  }
  if (typeof value === "string") return JSON.stringify(value);
  if (Array.isArray(value)) {
    return "[" + value.map(canonicalJson).join(",") + "]";
  }
  if (typeof value === "object") {
    const keys = Object.keys(value as Record<string, unknown>).sort();
    const parts = keys.map(
      (k) => JSON.stringify(k) + ":" + canonicalJson((value as Record<string, unknown>)[k]),
    );
    return "{" + parts.join(",") + "}";
  }
  return "null";
}

export function specHash(spec: unknown): string {
  const canon = canonicalJson(spec ?? {});
  return "sha256:" + createHash("sha256").update(canon).digest("hex");
}

/** Reduces `metadata.managedFields` (one entry per (manager, operation,
 *  subresource, time) tuple) into a `manager → field-count` summary.
 *  Field counts come from the `fieldsV1` tree leaf count. */
export function summariseFieldOwners(
  managedFields: ReadonlyArray<{
    manager?: string;
    fieldsV1?: unknown;
    subresource?: string;
  }> | undefined,
): Array<{ manager: string; fieldsOwned: number }> {
  if (!managedFields || managedFields.length === 0) return [];
  const counts = new Map<string, number>();
  for (const e of managedFields) {
    const m = e.manager ?? "(unknown)";
    const n = countLeaves(e.fieldsV1);
    counts.set(m, (counts.get(m) ?? 0) + n);
  }
  return [...counts.entries()]
    .map(([manager, fieldsOwned]) => ({ manager, fieldsOwned }))
    .sort((a, b) => a.manager.localeCompare(b.manager));
}

function countLeaves(tree: unknown): number {
  if (tree === null || tree === undefined) return 0;
  if (typeof tree !== "object") return 1;
  if (Array.isArray(tree)) {
    return tree.reduce<number>((acc, v) => acc + countLeaves(v), 0);
  }
  const obj = tree as Record<string, unknown>;
  let n = 0;
  for (const k of Object.keys(obj)) {
    // SSA's fieldsV1 encodes path components via `f:foo`, `k:{"…"}`,
    // `v:…`, `i:…`. Leaf entries are empty objects (`{}`) under those
    // keys; we count those as one field each. Non-leaf entries recurse.
    const child = obj[k];
    if (
      child !== null &&
      typeof child === "object" &&
      !Array.isArray(child) &&
      Object.keys(child as object).length === 0
    ) {
      n += 1;
    } else {
      n += countLeaves(child);
    }
  }
  return n || 1;
}

/** Extracts the policy-CR refs declared on the `ClawSandbox.spec`. The
 *  refs are looked up at four shapes the Phase 2 CRDs use:
 *  - `spec.toolPolicyRef.name`            → ToolPolicy
 *  - `spec.inferencePolicyRef.name`       → InferencePolicy
 *  - `spec.a2aAgentRef.name`              → A2AAgent
 *  - `spec.governance.toolPolicy.ref`     → legacy ToolPolicy ref
 *  Unknown refs are ignored. */
export function extractPolicyRefs(spec: unknown): Array<{
  kind: string;
  name: string;
}> {
  const out: Array<{ kind: string; name: string }> = [];
  if (!spec || typeof spec !== "object") return out;
  const s = spec as Record<string, unknown>;
  for (const { kind, refField } of POLICY_CR_KINDS) {
    const r = s[refField];
    if (
      r !== null &&
      typeof r === "object" &&
      typeof (r as Record<string, unknown>).name === "string"
    ) {
      out.push({ kind, name: (r as Record<string, unknown>).name as string });
    }
  }
  const gov = s.governance as Record<string, unknown> | undefined;
  if (gov && typeof gov === "object") {
    // S13: same-namespace `toolPolicyRef.name` shape.
    const tpr = gov.toolPolicyRef as Record<string, unknown> | undefined;
    if (tpr && typeof tpr === "object" && typeof tpr.name === "string") {
      out.push({ kind: "ToolPolicy", name: tpr.name });
    }
    // Legacy `governance.toolPolicy.ref: string` shape — pre-S13.
    const tp = gov.toolPolicy as Record<string, unknown> | undefined;
    if (tp && typeof tp === "object" && typeof tp.ref === "string") {
      out.push({ kind: "ToolPolicy", name: tp.ref });
    }
  }
  // De-duplicate (S13 may reference the same ToolPolicy via two paths
  // during the rollout window).
  const seen = new Set<string>();
  return out.filter((r) => {
    const k = `${r.kind}/${r.name}`;
    if (seen.has(k)) return false;
    seen.add(k);
    return true;
  });
}

async function buildReport(name: string, opts: { namespace: string }): Promise<AttestationReport> {
  const { execa } = await import("execa");
  // `ClawSandbox` is cluster-scoped via the controller registration but
  // historically lives in `azureclaw-system`; the operator passes
  // `--namespace` to override.
  const { stdout } = await execa("kubectl", [
    "get",
    "clawsandbox",
    name,
    "-n",
    opts.namespace,
    "-o",
    "json",
  ], { stdio: "pipe" });
  const cr = JSON.parse(stdout) as {
    metadata?: {
      name?: string;
      generation?: number;
      managedFields?: Array<{ manager?: string; fieldsV1?: unknown; subresource?: string }>;
    };
    spec?: unknown;
    status?: { phase?: string; observedGeneration?: number };
  };

  const sbNs = `azureclaw-${name}`;
  const policyRefs = extractPolicyRefs(cr.spec);
  const policyVersions = await Promise.all(
    policyRefs.map(async (ref) => {
      const plural = POLICY_CR_KINDS.find((k) => k.kind === ref.kind)!.plural;
      try {
        const { stdout: out } = await execa("kubectl", [
          "get",
          plural,
          ref.name,
          "-n",
          sbNs,
          "-o",
          "json",
        ], { stdio: "pipe" });
        const obj = JSON.parse(out) as {
          status?: { versionHash?: string; bindingConfigMap?: string };
        };
        return {
          kind: ref.kind,
          name: ref.name,
          namespace: sbNs,
          versionHash: obj.status?.versionHash ?? null,
          bindingConfigMap: obj.status?.bindingConfigMap ?? null,
        };
      } catch {
        return {
          kind: ref.kind,
          name: ref.name,
          namespace: sbNs,
          versionHash: null,
          bindingConfigMap: null,
        };
      }
    }),
  );

  let traceId: string | null = null;
  try {
    const { stdout: out } = await execa("kubectl", [
      "get",
      "deploy",
      name,
      "-n",
      sbNs,
      "-o",
      "jsonpath={.metadata.annotations.azureclaw\\.azure\\.com/last-trace-id}",
    ], { stdio: "pipe" });
    traceId = out.trim() || null;
  } catch {
    /* deployment may not exist (overlay mode) — leave null */
  }

  return {
    apiVersion: "azureclaw.azure.com/v1alpha1-attest",
    kind: "Attestation",
    generatedAt: new Date().toISOString(),
    sandbox: {
      name,
      namespace: opts.namespace,
      generation: cr.metadata?.generation ?? null,
      observedGeneration: cr.status?.observedGeneration ?? null,
      phase: cr.status?.phase ?? null,
      specHash: specHash(cr.spec),
      specHashAlgorithm: "sha256-canonical-json",
    },
    fieldOwners: summariseFieldOwners(cr.metadata?.managedFields),
    policyVersions,
    reconcileTraceId: traceId,
    agtAuditReceiptId: null,
    signature: null,
  };
}

export function formatJson(report: AttestationReport): string {
  return JSON.stringify(report, null, 2);
}

/** Compares two attestation reports and emits one delta per
 *  human-meaningful change. Pure function — no IO, no time. The returned
 *  `drift` flag is true iff at least one delta is present, which is what
 *  drives the CLI exit code (2 on drift, 0 on match). */
export function diffAttestations(
  baseline: AttestationReport,
  current: AttestationReport,
): AttestationDiff {
  const deltas: AttestationDelta[] = [];

  if (baseline.sandbox.specHash !== current.sandbox.specHash) {
    deltas.push({
      type: "specHash",
      before: baseline.sandbox.specHash,
      after: current.sandbox.specHash,
    });
  }

  if (baseline.sandbox.phase !== current.sandbox.phase) {
    deltas.push({
      type: "phase",
      before: baseline.sandbox.phase,
      after: current.sandbox.phase,
    });
  }

  // Policy refs are matched on (kind, name). Order-insensitive.
  const policyKey = (p: { kind: string; name: string }) => `${p.kind}/${p.name}`;
  const baseByKey = new Map(baseline.policyVersions.map((p) => [policyKey(p), p]));
  const currByKey = new Map(current.policyVersions.map((p) => [policyKey(p), p]));
  for (const [k, bp] of baseByKey) {
    const cp = currByKey.get(k);
    if (!cp) {
      deltas.push({ type: "policyRemoved", kind: bp.kind, name: bp.name });
      continue;
    }
    if (bp.versionHash !== cp.versionHash) {
      deltas.push({
        type: "policyVersionHash",
        kind: bp.kind,
        name: bp.name,
        before: bp.versionHash,
        after: cp.versionHash,
      });
    }
  }
  for (const [k, cp] of currByKey) {
    if (!baseByKey.has(k)) {
      deltas.push({ type: "policyAdded", kind: cp.kind, name: cp.name });
    }
  }

  // Field-owner *set* comparison only. Field counts fluctuate on every
  // SSA write so are noisy; presence/absence of a manager is the
  // signal CI gates actually want ("a new actor touched this object").
  const baseManagers = new Set(baseline.fieldOwners.map((f) => f.manager));
  const currManagers = new Set(current.fieldOwners.map((f) => f.manager));
  for (const m of baseManagers) {
    if (!currManagers.has(m)) deltas.push({ type: "fieldOwnerRemoved", manager: m });
  }
  for (const m of currManagers) {
    if (!baseManagers.has(m)) deltas.push({ type: "fieldOwnerAdded", manager: m });
  }

  return {
    baseline: {
      generatedAt: baseline.generatedAt,
      specHash: baseline.sandbox.specHash,
    },
    current: {
      generatedAt: current.generatedAt,
      specHash: current.sandbox.specHash,
    },
    deltas,
    drift: deltas.length > 0,
  };
}

/** Loads + validates a previously-emitted attestation file. Returns
 *  `null` if the file does not exist (CLI exits with code 3); throws on
 *  parse / shape errors (CLI surfaces and exits non-zero). */
export async function loadBaseline(path: string): Promise<AttestationReport | null> {
  let raw: string;
  try {
    raw = await readFile(path, "utf-8");
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === "ENOENT") return null;
    throw err;
  }
  const parsed = JSON.parse(raw) as Partial<AttestationReport>;
  if (
    parsed.apiVersion !== "azureclaw.azure.com/v1alpha1-attest" ||
    parsed.kind !== "Attestation" ||
    !parsed.sandbox ||
    typeof parsed.sandbox.specHash !== "string"
  ) {
    throw new Error(
      `baseline ${path} is not a valid AzureClaw attestation ` +
        `(expected apiVersion=azureclaw.azure.com/v1alpha1-attest, kind=Attestation)`,
    );
  }
  return parsed as AttestationReport;
}

function describeDelta(d: AttestationDelta): string {
  switch (d.type) {
    case "specHash":
      return `spec hash drifted (${shortHash(d.before)} → ${shortHash(d.after)})`;
    case "phase":
      return `phase changed (${d.before ?? "(unset)"} → ${d.after ?? "(unset)"})`;
    case "policyVersionHash":
      return `${d.kind} '${d.name}' versionHash drifted (${shortHash(d.before)} → ${shortHash(d.after)})`;
    case "policyAdded":
      return `${d.kind} '${d.name}' added since baseline`;
    case "policyRemoved":
      return `${d.kind} '${d.name}' removed since baseline`;
    case "fieldOwnerAdded":
      return `new SSA manager touched the object: '${d.manager}'`;
    case "fieldOwnerRemoved":
      return `SSA manager no longer present: '${d.manager}'`;
  }
}

function shortHash(h: string | null): string {
  if (!h) return "(none)";
  return h.length > 14 ? h.slice(0, 14) + "…" : h;
}

export function formatHuman(report: AttestationReport): string {
  const blue = chalk.hex("#0078D4");
  const dim = chalk.dim;
  const phaseColour =
    report.sandbox.phase === "Running"
      ? chalk.green
      : report.sandbox.phase === "Overlay"
      ? chalk.cyan
      : report.sandbox.phase === "Degraded"
      ? chalk.red
      : chalk.yellow;
  const lines: string[] = [];
  lines.push(blue(`\n  AzureClaw · Attestation\n`));
  lines.push(`  ${chalk.bold("Sandbox:")}             ${report.sandbox.name}`);
  lines.push(`  ${chalk.bold("Namespace:")}           ${report.sandbox.namespace}`);
  lines.push(
    `  ${chalk.bold("Phase:")}               ${phaseColour(report.sandbox.phase ?? "(unknown)")}`,
  );
  lines.push(
    `  ${chalk.bold("Generation:")}          ${report.sandbox.generation ?? "?"} ` +
      dim(`(observed: ${report.sandbox.observedGeneration ?? "?"})`),
  );
  lines.push(`  ${chalk.bold("Spec hash:")}           ${report.sandbox.specHash}`);
  lines.push(`  ${dim(`  algorithm: ${report.sandbox.specHashAlgorithm}`)}`);

  lines.push(`\n  ${chalk.bold("Field-owner map (SSA):")}`);
  if (report.fieldOwners.length === 0) {
    lines.push(`    ${dim("(no managedFields recorded)")}`);
  } else {
    for (const { manager, fieldsOwned } of report.fieldOwners) {
      lines.push(`    ${manager.padEnd(36)} ${dim(`fields: ${fieldsOwned}`)}`);
    }
  }

  lines.push(`\n  ${chalk.bold("Policy versions:")}`);
  if (report.policyVersions.length === 0) {
    lines.push(`    ${dim("(no policy CRDs referenced from spec)")}`);
  } else {
    for (const p of report.policyVersions) {
      const v = p.versionHash ?? dim("(not reconciled)");
      const cm = p.bindingConfigMap ? dim(` → ${p.bindingConfigMap}`) : "";
      lines.push(`    ${p.kind.padEnd(18)} ${p.name.padEnd(20)} ${v}${cm}`);
    }
  }

  lines.push(`\n  ${chalk.bold("Reconcile trace ID:")}  ${report.reconcileTraceId ?? dim("(Phase 3)")}`);
  lines.push(
    `  ${chalk.bold("AGT receipt ID:")}      ${report.agtAuditReceiptId ?? dim("(Phase 3)")}`,
  );
  lines.push(`  ${chalk.bold("Signature:")}           ${report.signature ?? dim("(Phase 3)")}`);

  if (report.baselineDiff) {
    const d = report.baselineDiff;
    lines.push(`\n  ${chalk.bold("Baseline diff:")}`);
    lines.push(
      `    ${dim(`baseline: ${d.baseline.generatedAt}  (spec ${shortHash(d.baseline.specHash)})`)}`,
    );
    lines.push(
      `    ${dim(`current:  ${d.current.generatedAt}  (spec ${shortHash(d.current.specHash)})`)}`,
    );
    if (!d.drift) {
      lines.push(`    ${chalk.green("✓")} no drift detected`);
    } else {
      for (const delta of d.deltas) {
        lines.push(`    ${chalk.red("✗")} ${describeDelta(delta)}`);
      }
      lines.push(
        `    ${chalk.red.bold(`DRIFT: ${d.deltas.length} delta(s) — exit code 2`)}`,
      );
    }
  }

  lines.push("");
  return lines.join("\n");
}

export function attestCommand(): Command {
  const cmd = new Command("attest");
  cmd
    .description(
      "Print a deterministic attestation receipt for a sandbox " +
        "(spec hash, SSA field owners, referenced policy versions, " +
        "reconcile trace; signature/AGT receipt land in Phase 3). " +
        "Pass --baseline <file> to diff against a previously-saved " +
        "attestation; exits 2 on drift, 3 on missing baseline.",
    )
    .argument("<name>", "Sandbox name")
    .option(
      "-n, --namespace <ns>",
      "Namespace where the ClawSandbox CR lives",
      "azureclaw-system",
    )
    .option("--format <fmt>", "Output format: 'human' (default) or 'json'", "human")
    .option(
      "--baseline <path>",
      "Path to a previously-emitted attestation JSON. " +
        "When set, diff is appended to output and exit code reflects drift " +
        "(0=match, 2=drift, 3=baseline file missing).",
    )
    .action(
      async (
        name: string,
        options: { namespace: string; format: string; baseline?: string },
      ) => {
        const report = await buildReport(name, { namespace: options.namespace });

        if (options.baseline) {
          const baseline = await loadBaseline(options.baseline);
          if (!baseline) {
            process.stderr.write(
              chalk.red(`✗ baseline file not found: ${options.baseline}\n`),
            );
            process.exit(3);
          }
          report.baselineDiff = diffAttestations(baseline, report);
        }

        if (options.format === "json") {
          console.log(formatJson(report));
        } else {
          console.log(formatHuman(report));
        }

        if (report.baselineDiff?.drift) {
          process.exit(2);
        }
      },
    );
  return cmd;
}

export const __test = {
  canonicalJson,
  specHash,
  summariseFieldOwners,
  extractPolicyRefs,
  formatJson,
  formatHuman,
  diffAttestations,
  loadBaseline,
  describeDelta,
};
