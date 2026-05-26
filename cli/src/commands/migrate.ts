// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Phase 2 S9.1 — `kars migrate` mode-switch CLI subcommand.
//
// Operator-facing tool to flip a `KarsSandbox` between the four
// upstream-compatibility modes shipped by S8 (controller-side
// OverlayMode / TranslateMode / ObserveMode + the default Native /
// "off" mode). The command is a thin wrapper around `kubectl patch`:
// pure helpers compute a JSON merge patch and a human transition
// summary; the orchestrator pre-fetches the current spec, prints a
// before/after, and applies the patch.
//
// Real workflow this unlocks (the day-zero adoption story discussed
// in S11.1):
//
//   # operator already has an upstream sigs.k8s.io/agent-sandbox
//   # `Sandbox` CR called `legacy-agent`; wants to bolt Kars
//   # governance on without rewriting it:
//   $ kubectl apply -f legacy-karssandbox.yaml      # native by default
//   $ kars migrate to-overlay legacy --upstream-ref legacy-agent
//   ✓ legacy: native → overlay (upstream sandbox 'legacy-agent')
//
//   # later: customer wants pure Kars, drop the upstream
//   $ kars migrate from-overlay legacy
//   ✓ legacy: overlay → native
//
// **No new CRD field, no controller change.** The OverlayMode
// reconciler logic landed in S8 (PR #57); this slice ships the
// operator-facing command that drives it. Reuse-first by design.
//
// Sub-slice S9.2 (PR #63) shipped a real `kars convert` (YAML
// translator from upstream agent-sandbox shapes).
//
// Sub-slice S9.3 (this file's `from-kagent` subcommand) ships the
// kagent CR → KarsSandbox translator: pure helpers live in
// `cli/src/migrate/from_kagent.ts`.

import { Command } from "commander";
import chalk from "chalk";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import * as path from "node:path";
import yaml from "yaml";
import {
  InvalidInputError,
  type KubeResource,
  type TranslateOptions,
  type Warning,
  translate as translateFromKagent,
} from "../migrate/from_kagent.js";

/** The four upstream-compatibility modes the controller accepts.
 *  Keep in sync with `controller/src/crd.rs`
 *  `UpstreamCompatibilityConfig.sigs_agent_sandbox` doc comment. */
export const MIGRATE_MODES = ["off", "observe", "translate", "overlay"] as const;
export type MigrateMode = (typeof MIGRATE_MODES)[number];

/** Display name for a mode — what the operator sees in transition
 *  summaries. "off" reads better as "native" since that's how the
 *  product positions the default. */
export function modeDisplay(m: MigrateMode | null | undefined): string {
  if (!m || m === "off") return "native";
  return m;
}

/** Validates a mode-switch request before any kubectl call.
 *  Returns an array of human-readable error strings; empty array
 *  means valid. */
export function validateMode(
  mode: MigrateMode,
  upstreamRef: string | undefined,
): string[] {
  const errs: string[] = [];
  if (!MIGRATE_MODES.includes(mode)) {
    errs.push(
      `invalid mode '${mode}'; expected one of: ${MIGRATE_MODES.join(", ")}`,
    );
  }
  if (mode === "overlay" && !upstreamRef) {
    errs.push(
      "--upstream-ref <name> is required for 'overlay' mode " +
        "(the upstream sigs.k8s.io/agent-sandbox CR that owns the Pod)",
    );
  }
  if (mode !== "overlay" && upstreamRef) {
    errs.push(
      `--upstream-ref is only meaningful for 'overlay' mode (got mode='${mode}')`,
    );
  }
  if (upstreamRef !== undefined && upstreamRef.length === 0) {
    errs.push("--upstream-ref must be a non-empty name");
  }
  return errs;
}

/** Builds the JSON merge patch that flips the sandbox to `mode`.
 *  Pure — no IO. The patch always sets `sigsAgentSandbox` so the
 *  controller's pre-flight sees a deterministic value, and it
 *  *removes* `upstreamSandboxRef` (sets to `null`) when leaving
 *  overlay mode so a stale ref cannot strand the sandbox.
 *
 *  Shape (always rooted at `spec.upstreamCompatibility`):
 *
 *    overlay:    { sigsAgentSandbox: "overlay",   upstreamSandboxRef: { name } }
 *    translate:  { sigsAgentSandbox: "translate", upstreamSandboxRef: null }
 *    observe:    { sigsAgentSandbox: "observe",   upstreamSandboxRef: null }
 *    off:        { sigsAgentSandbox: "off",       upstreamSandboxRef: null }
 *
 *  JSON merge patch (RFC 7396) treats `null` as "delete the field",
 *  which matches the controller's `Option<LocalObjectRef>`
 *  `skip_serializing_if = "Option::is_none"` round-trip semantics. */
export function buildModePatch(
  mode: MigrateMode,
  upstreamRef: string | undefined,
): { spec: { upstreamCompatibility: Record<string, unknown> } } {
  const upstreamCompatibility: Record<string, unknown> = {
    sigsAgentSandbox: mode,
    upstreamSandboxRef: mode === "overlay" ? { name: upstreamRef } : null,
  };
  return { spec: { upstreamCompatibility } };
}

interface CurrentMode {
  mode: MigrateMode;
  upstreamRef: string | null;
}

/** Reads the relevant fields off the current KarsSandbox spec.
 *  Defaults match the controller: missing config is "off" mode. */
export function readCurrentMode(spec: unknown): CurrentMode {
  const s = spec as Record<string, unknown> | undefined;
  const uc = s?.upstreamCompatibility as Record<string, unknown> | undefined;
  const rawMode = uc?.sigsAgentSandbox;
  const mode: MigrateMode =
    typeof rawMode === "string" && (MIGRATE_MODES as readonly string[]).includes(rawMode)
      ? (rawMode as MigrateMode)
      : "off";
  const ref = uc?.upstreamSandboxRef as Record<string, unknown> | undefined;
  const upstreamRef = typeof ref?.name === "string" ? (ref.name as string) : null;
  return { mode, upstreamRef };
}

/** Describes the transition in human terms. Returns the message and
 *  a `noop` flag so the orchestrator can skip the kubectl call. */
export function summariseTransition(
  current: CurrentMode,
  target: { mode: MigrateMode; upstreamRef: string | undefined },
): { message: string; noop: boolean } {
  const cur = modeDisplay(current.mode);
  const tgt = modeDisplay(target.mode);
  const refSuffix = target.upstreamRef
    ? ` (upstream sandbox '${target.upstreamRef}')`
    : "";

  const sameMode = current.mode === target.mode;
  const sameRef =
    (current.upstreamRef ?? null) === (target.upstreamRef ?? null);
  if (sameMode && sameRef) {
    return {
      message: `${cur} mode${refSuffix} (already in target state)`,
      noop: true,
    };
  }
  return { message: `${cur} → ${tgt}${refSuffix}`, noop: false };
}

async function runPatch(
  name: string,
  namespace: string,
  patch: object,
): Promise<void> {
  const { execa } = await import("execa");
  await execa(
    "kubectl",
    [
      "patch",
      "karssandbox",
      name,
      "-n",
      namespace,
      "--type=merge",
      "-p",
      JSON.stringify(patch),
    ],
    { stdio: "pipe" },
  );
}

async function fetchCurrentSpec(
  name: string,
  namespace: string,
): Promise<unknown> {
  const { execa } = await import("execa");
  const { stdout } = await execa(
    "kubectl",
    ["get", "karssandbox", name, "-n", namespace, "-o", "json"],
    { stdio: "pipe" },
  );
  const cr = JSON.parse(stdout) as { spec?: unknown };
  return cr.spec;
}

interface ActionOptions {
  namespace: string;
  dryRun: boolean;
  format: "human" | "json";
}

async function runMigrate(
  name: string,
  target: { mode: MigrateMode; upstreamRef: string | undefined },
  opts: ActionOptions,
): Promise<void> {
  const errs = validateMode(target.mode, target.upstreamRef);
  if (errs.length > 0) {
    process.stderr.write(chalk.red(errs.map((e) => `✗ ${e}`).join("\n") + "\n"));
    process.exit(2);
  }

  const patch = buildModePatch(target.mode, target.upstreamRef);

  if (opts.dryRun) {
    if (opts.format === "json") {
      console.log(JSON.stringify({ patch, dryRun: true }, null, 2));
    } else {
      console.log(chalk.bold("DRY RUN — patch that would be applied:"));
      console.log(JSON.stringify(patch, null, 2));
      console.log(chalk.dim(`\nApply with: kars migrate ... (omit --dry-run)`));
    }
    return;
  }

  let current: CurrentMode;
  try {
    const spec = await fetchCurrentSpec(name, opts.namespace);
    current = readCurrentMode(spec);
  } catch (err) {
    process.stderr.write(
      chalk.red(`✗ failed to read sandbox '${name}' in '${opts.namespace}': `) +
        ((err as Error).message ?? String(err)) +
        "\n",
    );
    process.exit(1);
  }

  const transition = summariseTransition(current, target);

  if (opts.format === "json") {
    console.log(
      JSON.stringify(
        {
          sandbox: name,
          namespace: opts.namespace,
          before: { mode: current.mode, upstreamRef: current.upstreamRef },
          after: { mode: target.mode, upstreamRef: target.upstreamRef ?? null },
          noop: transition.noop,
          patch: transition.noop ? null : patch,
        },
        null,
        2,
      ),
    );
  } else {
    console.log(chalk.bold(`\n  ${name}: `) + transition.message);
  }

  if (transition.noop) return;

  try {
    await runPatch(name, opts.namespace, patch);
    if (opts.format !== "json") {
      console.log(chalk.green(`  ✓ patched`));
      console.log(chalk.dim(`  controller will reconcile the new mode shortly`));
    }
  } catch (err) {
    process.stderr.write(
      chalk.red(`✗ kubectl patch failed: `) +
        ((err as Error).message ?? String(err)) +
        "\n",
    );
    process.exit(1);
  }
}

function commonOptions(cmd: Command): Command {
  return cmd
    .option(
      "-n, --namespace <ns>",
      "Namespace where the KarsSandbox CR lives",
      "kars-system",
    )
    .option(
      "--dry-run",
      "Print the JSON merge patch without applying",
      false,
    )
    .option(
      "--format <fmt>",
      "Output format: 'human' (default) or 'json'",
      "human",
    );
}

export function migrateCommand(): Command {
  const cmd = new Command("migrate")
    .description(
      "Switch a KarsSandbox between upstream-compatibility modes " +
        "(native / overlay / translate / observe). Wraps a kubectl " +
        "patch with validation, before/after summary, and dry-run.",
    );

  commonOptions(
    cmd
      .command("to-overlay <name>")
      .description(
        "Flip to overlay mode: Kars provides governance overlay " +
          "(namespace, NetworkPolicy, ConfigMaps); upstream sigs.k8s.io/" +
          "agent-sandbox CR owns the Pod. Requires --upstream-ref.",
      )
      .requiredOption(
        "--upstream-ref <name>",
        "Name of the upstream Sandbox CR in the same namespace",
      ),
  ).action(
    async (
      name: string,
      options: {
        namespace: string;
        dryRun: boolean;
        format: string;
        upstreamRef: string;
      },
    ) => {
      await runMigrate(
        name,
        { mode: "overlay", upstreamRef: options.upstreamRef },
        {
          namespace: options.namespace,
          dryRun: options.dryRun,
          format: options.format === "json" ? "json" : "human",
        },
      );
    },
  );

  commonOptions(
    cmd
      .command("from-overlay <name>")
      .description(
        "Leave overlay mode and revert to native Kars " +
          "(controller resumes Pod / Service / NetworkPolicy ownership).",
      ),
  ).action(
    async (
      name: string,
      options: { namespace: string; dryRun: boolean; format: string },
    ) => {
      await runMigrate(
        name,
        { mode: "off", upstreamRef: undefined },
        {
          namespace: options.namespace,
          dryRun: options.dryRun,
          format: options.format === "json" ? "json" : "human",
        },
      );
    },
  );

  for (const m of ["translate", "observe", "native"] as const) {
    const subMode: MigrateMode = m === "native" ? "off" : m;
    commonOptions(
      cmd
        .command(`to-${m} <name>`)
        .description(
          m === "translate"
            ? "Accept upstream SandboxClaim semantics on inbound (P1 schema-only)."
            : m === "observe"
              ? "Mirror status of an upstream Sandbox CR; no overlay."
              : "Reset to default native mode (Kars owns the workload).",
        ),
    ).action(
      async (
        name: string,
        options: { namespace: string; dryRun: boolean; format: string },
      ) => {
        await runMigrate(
          name,
          { mode: subMode, upstreamRef: undefined },
          {
            namespace: options.namespace,
            dryRun: options.dryRun,
            format: options.format === "json" ? "json" : "human",
          },
        );
      },
    );
  }

  cmd
    .command("from-kagent <input>")
    .description(
      "Translate a kagent.dev/v1alpha2 Agent YAML into an Kars " +
        "resource bundle (KarsSandbox + InferencePolicy + ToolPolicies). " +
        "Use '-' to read from stdin. Hard-fails on lossy translation " +
        "by default; pass --allow-lossy to waive.",
    )
    .option(
      "-n, --namespace <ns>",
      "Override metadata.namespace on emitted resources (warns on mismatch with input).",
    )
    .option(
      "--isolation <mode>",
      "KarsSandbox isolation mode: standard | enhanced | confidential (default: enhanced).",
      "enhanced",
    )
    .option(
      "--image <image>",
      "Override spec.runtime.openclaw.image (required to make Declarative agents runnable; ignored for BYO).",
    )
    .option(
      "--allow-lossy",
      "Waive the hard-fail on lossy translation. Warnings are still printed to stderr.",
      false,
    )
    .option("--out-dir <dir>", "Write each emitted resource to <dir>/<kind>-<name>.yaml.")
    .option("--force", "With --out-dir, overwrite existing files.", false)
    .option(
      "--format <fmt>",
      "Output format when writing to stdout: 'yaml' (multi-doc, default) or 'json' (List).",
      "yaml",
    )
    .option("--dry-run", "Print summary + warnings; emit no resources.", false)
    .action(
      async (
        input: string,
        options: {
          namespace?: string;
          isolation: string;
          image?: string;
          allowLossy: boolean;
          outDir?: string;
          force: boolean;
          format: string;
          dryRun: boolean;
        },
      ) => {
        await runFromKagent(input, options);
      },
    );

  return cmd;
}

// ---- from-kagent runner ----------------------------------------------------

interface FromKagentOptions {
  namespace?: string;
  isolation: string;
  image?: string;
  allowLossy: boolean;
  outDir?: string;
  force: boolean;
  format: string;
  dryRun: boolean;
}

async function runFromKagent(
  input: string,
  options: FromKagentOptions,
): Promise<void> {
  const text = await readInput(input);
  const docs = yaml.parseAllDocuments(text);
  const nonEmpty = docs.filter((d) => d.contents !== null);
  if (nonEmpty.length === 0) {
    process.stderr.write(`error: input contains no YAML documents\n`);
    process.exit(2);
  }
  if (nonEmpty.length > 1) {
    process.stderr.write(
      `error: input contains ${nonEmpty.length} YAML documents; from-kagent expects exactly one Agent\n`,
    );
    process.exit(2);
  }
  const doc = nonEmpty[0]!;
  if (doc.errors.length > 0) {
    for (const err of doc.errors) process.stderr.write(`error: ${err.message}\n`);
    process.exit(2);
  }
  const json = doc.toJS();

  const isolation = validateIsolation(options.isolation);
  const opts: TranslateOptions = {
    namespace: options.namespace,
    isolation,
    image: options.image,
  };

  let result;
  try {
    result = translateFromKagent(json, opts);
  } catch (e) {
    if (e instanceof InvalidInputError) {
      process.stderr.write(`error: ${e.message}\n`);
      process.exit(2);
    }
    throw e;
  }

  for (const w of result.warnings) printWarning(w);

  const lossy = result.warnings.length > 0;
  if (lossy && !options.allowLossy) {
    process.stderr.write(
      chalk.red(
        `error: translation is lossy (${result.warnings.length} warnings). Pass --allow-lossy to waive.\n`,
      ),
    );
    process.exit(4);
  }

  // Summary line for both dry-run and real output.
  const s = result.summary;
  process.stderr.write(
    chalk.gray(
      `summary: ${s.agentType} agent '${s.namespace}/${s.sandboxName}'; ` +
        `runnable=${s.runnable}; toolPolicies=${s.toolPolicyCount}; ` +
        `inferencePolicies=${s.inferencePolicyCount}\n`,
    ),
  );

  // S12.g — when the migrated bundle includes an egress allowlist on a
  // KarsSandbox, point the operator at the sign-by-default + GitOps
  // emit-manifest flow.
  const sandboxesWithEgress = result.resources.filter(
    (r) =>
      r.kind === "KarsSandbox" &&
      Array.isArray(
        ((r.spec as Record<string, unknown>)?.networkPolicy as
          | Record<string, unknown>
          | undefined)?.allowedEndpoints,
      ),
  );
  if (sandboxesWithEgress.length > 0) {
    process.stderr.write(
      chalk.hex("#0078D4")(
        `\nNext step: the migrated bundle includes an egress allowlist. ` +
          `Sign and emit a GitOps manifest with:\n`,
      ),
    );
    for (const r of sandboxesWithEgress) {
      const sbName = r.metadata.name;
      const sbNs = r.metadata.namespace;
      process.stderr.write(
        chalk.gray(
          `  kars egress ${sbName} --namespace ${sbNs} --enforce ` +
            `--emit-manifest ./gitops/${sbName}-allowlist.yaml\n`,
        ),
      );
    }
    process.stderr.write(
      chalk.gray(
        `(Signing is default-on; pass --no-sign to opt out — note the controller will refuse unsigned artifacts in authoritative mode.)\n`,
      ),
    );
  }

  if (options.dryRun) return;

  if (options.outDir) {
    await writeBundleToDir(result.resources, options.outDir, options.force);
    process.stderr.write(
      chalk.green(`✓ wrote ${result.resources.length} resources to ${options.outDir}\n`),
    );
    return;
  }

  if (options.format === "json") {
    process.stdout.write(
      `${JSON.stringify(
        {
          apiVersion: "v1",
          kind: "List",
          items: result.resources,
        },
        null,
        2,
      )}\n`,
    );
  } else {
    const docs = result.resources
      .map((r) => yaml.stringify(r, { lineWidth: 0 }))
      .join("---\n");
    process.stdout.write(docs);
  }
}

async function readInput(target: string): Promise<string> {
  if (target === "-") {
    const chunks: Buffer[] = [];
    for await (const c of process.stdin) chunks.push(c as Buffer);
    return Buffer.concat(chunks).toString("utf8");
  }
  return readFile(target, "utf8");
}

function validateIsolation(
  raw: string,
): "standard" | "enhanced" | "confidential" {
  if (raw === "standard" || raw === "enhanced" || raw === "confidential") {
    return raw;
  }
  process.stderr.write(
    `error: --isolation must be one of standard|enhanced|confidential (got '${raw}')\n`,
  );
  process.exit(2);
}

function printWarning(w: Warning): void {
  const tag = w.severity === "error" ? chalk.red("✗") : chalk.yellow("!");
  process.stderr.write(`${tag} ${chalk.cyan(w.path)}: ${w.message}\n`);
}

async function writeBundleToDir(
  resources: KubeResource[],
  dir: string,
  force: boolean,
): Promise<void> {
  await mkdir(dir, { recursive: true });
  const seen = new Set<string>();
  for (const r of resources) {
    const fname = `${r.kind.toLowerCase()}-${r.metadata.name}.yaml`;
    if (seen.has(fname)) {
      process.stderr.write(
        `error: duplicate output filename '${fname}' (kind+name collision)\n`,
      );
      process.exit(2);
    }
    seen.add(fname);
    const fpath = path.join(dir, fname);
    const flag = force ? "w" : "wx";
    try {
      await writeFile(fpath, yaml.stringify(r, { lineWidth: 0 }), { flag });
    } catch (e) {
      const code = (e as NodeJS.ErrnoException).code;
      if (code === "EEXIST") {
        process.stderr.write(
          `error: ${fpath} already exists (use --force to overwrite)\n`,
        );
        process.exit(2);
      }
      throw e;
    }
  }
}

export const __test = {
  validateMode,
  buildModePatch,
  readCurrentMode,
  summariseTransition,
  modeDisplay,
};
