// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// commands/update.ts — `kars update`: explicitly check npm for a newer
// `@kars-runtime/cli`, show the changelog, and (interactively) install it.
//
// This is the on-demand counterpart to the passive end-of-run notice wired into
// every invocation (lib/update-check.ts). It always hits the network (bypassing
// the 24h cache) and always offers to install when a newer version exists.

import { Command } from "commander";
import chalk from "chalk";
import { cliVersion } from "../lib/version.js";
import {
  CLI_PACKAGE,
  checkForCliUpdate,
  renderUpdateNotice,
} from "../lib/update-check.js";

export function updateCommand(): Command {
  const cmd = new Command("update");

  cmd
    .description(`Check for and install a newer ${CLI_PACKAGE}`)
    .option("--check", "Only check; never prompt or install")
    .option("--yes", "Install the latest version without prompting")
    .action(async (options: { check?: boolean; yes?: boolean }) => {
      const info = await checkForCliUpdate({ force: true, withChangelog: true });

      if (!info) {
        console.error(
          chalk.green(`\n  ✓ ${CLI_PACKAGE} is up to date `) +
            chalk.dim(`(v${cliVersion()}).\n`),
        );
        return;
      }

      if (options.check) {
        // Report-only: print the notice without the install prompt.
        await renderUpdateNotice(info, { offerInstall: false });
        // Non-zero so scripts/CI can detect "an update is available".
        process.exitCode = 1;
        return;
      }

      if (options.yes) {
        const { runGlobalInstall } = await import("../lib/update-check.js");
        await renderUpdateNotice(info, { offerInstall: false });
        await runGlobalInstall();
        return;
      }

      await renderUpdateNotice(info, {
        offerInstall: !!process.stdout.isTTY && !!process.stdin.isTTY,
      });
    });

  return cmd;
}
