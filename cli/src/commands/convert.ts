/**
 * `azureclaw convert` — translate between AzureClaw and upstream
 * agents.x-k8s.io/v1alpha1 Sandbox manifests.
 *
 * Phase 0: command surface only. No conversion logic yet. Real
 * translation arrives in Phase 2 alongside `azureclaw migrate`
 * (see docs/implementation-plan.md §2.2 + §8 item 4).
 *
 * Exit codes:
 *   0 — conversion would succeed (dry-run) or succeeded
 *   2 — input invalid / unsupported target
 *   3 — feature not yet implemented at this phase
 *
 * Design doc (normative mapping table):
 *   docs/sigs-agent-sandbox-compat.md §4
 */
import { Command } from "commander";
import chalk from "chalk";

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
      "Parse inputs and validate the plan, but do not emit the converted manifest",
      false,
    )
    .option(
      "--allow-lossy",
      "Proceed even when the inverse translation drops AzureClaw-only fields",
      false,
    )
    .addHelpText(
      "after",
      `
Examples:
  $ azureclaw convert -f sandbox.yaml --to clawsandbox
  $ azureclaw convert -f clawsandbox.yaml --to upstream-sandbox
  $ azureclaw convert -f clawsandbox.yaml --to overlay --sandbox-ref=prod/web

See docs/sigs-agent-sandbox-compat.md for the normative mapping table.
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
          chalk.red(
            `error: --to must be one of: ${TARGETS.join(", ")} (got ${opts.to})`,
          ),
        );
        process.exit(2);
      }
      if (target === "overlay" && !opts.sandboxRef) {
        console.error(
          chalk.red(
            "error: --to overlay requires --sandbox-ref=<namespace/name>",
          ),
        );
        process.exit(2);
      }

      console.error(
        chalk.yellow(
          "convert: not yet implemented (Phase 2 deliverable). " +
            "This Phase 0 skeleton exists to lock in the CLI surface; " +
            "no conversion is performed.",
        ),
      );
      console.error(
        chalk.dim(
          `  input: ${opts.file}\n` +
            `  target: ${target}${opts.sandboxRef ? ` (ref=${opts.sandboxRef})` : ""}\n` +
            `  dry-run: ${opts.dryRun}\n` +
            `  allow-lossy: ${opts.allowLossy}`,
        ),
      );
      console.error(
        chalk.dim(
          "  See docs/sigs-agent-sandbox-compat.md for the Phase 2 mapping.",
        ),
      );
      process.exit(3);
    });

  return cmd;
}

export const __test = { parseTarget, TARGETS };
