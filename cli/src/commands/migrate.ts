// Phase 2 S9.1 — `azureclaw migrate` mode-switch CLI subcommand.
//
// Operator-facing tool to flip a `ClawSandbox` between the four
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
//   # `Sandbox` CR called `legacy-agent`; wants to bolt AzureClaw
//   # governance on without rewriting it:
//   $ kubectl apply -f legacy-clawsandbox.yaml      # native by default
//   $ azureclaw migrate to-overlay legacy --upstream-ref legacy-agent
//   ✓ legacy: native → overlay (upstream sandbox 'legacy-agent')
//
//   # later: customer wants pure AzureClaw, drop the upstream
//   $ azureclaw migrate from-overlay legacy
//   ✓ legacy: overlay → native
//
// **No new CRD field, no controller change.** The OverlayMode
// reconciler logic landed in S8 (PR #57); this slice ships the
// operator-facing command that drives it. Reuse-first by design.
//
// Sub-slice S9.2 (separate PR) ships `migrate from-kagent` (kagent CR
// → ClawSandbox translator) + a real `azureclaw convert` (YAML
// translator from upstream agent-sandbox shapes). Those are heavier
// translators; this slice keeps the surface tight on mode-switch.

import { Command } from "commander";
import chalk from "chalk";

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

/** Reads the relevant fields off the current ClawSandbox spec.
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
      "clawsandbox",
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
    ["get", "clawsandbox", name, "-n", namespace, "-o", "json"],
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
      console.log(chalk.dim(`\nApply with: azureclaw migrate ... (omit --dry-run)`));
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
      "Namespace where the ClawSandbox CR lives",
      "azureclaw-system",
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
      "Switch a ClawSandbox between upstream-compatibility modes " +
        "(native / overlay / translate / observe). Wraps a kubectl " +
        "patch with validation, before/after summary, and dry-run.",
    );

  commonOptions(
    cmd
      .command("to-overlay <name>")
      .description(
        "Flip to overlay mode: AzureClaw provides governance overlay " +
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
        "Leave overlay mode and revert to native AzureClaw " +
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
              : "Reset to default native mode (AzureClaw owns the workload).",
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

  return cmd;
}

export const __test = {
  validateMode,
  buildModePatch,
  readCurrentMode,
  summariseTransition,
  modeDisplay,
};
