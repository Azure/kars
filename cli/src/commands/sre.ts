// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import { execa } from "execa";

/**
 * `kars sre` — manage the built-in kars-sre agent.
 *
 * Subcommands:
 *   install      — enable the chart's sre.yaml template (helm upgrade --set sre.enabled=true)
 *   uninstall    — disable it (helm upgrade --set sre.enabled=false)
 *   status       — show the sre KarsSandbox CR's state (kubectl get karssandbox sre)
 *   talk         — alias for `kars connect sre` (open the WebUI)
 *
 * Design: docs/blueprints/07-kars-sre-proposal.md
 */
export function sreCommand(): Command {
  const cmd = new Command("sre");
  cmd.description("Manage the built-in kars-sre agent (Kubernetes SRE on the cluster)");

  cmd
    .command("install")
    .description("Enable the kars-sre agent on the current cluster")
    .option(
      "--release <name>",
      "Helm release name to patch (defaults to 'kars')",
      "kars",
    )
    .option(
      "--namespace <ns>",
      "Helm release namespace (defaults to 'kars-system')",
      "kars-system",
    )
    .option(
      "--context <name>",
      "kubectl context to use (defaults to current-context)",
    )
    .option(
      "--model <name>",
      "Azure OpenAI deployment / model name for the SRE agent (defaults to gpt-4.1)",
    )
    .option(
      "--wait",
      "Wait for the sre sandbox to reach Running (default true)",
      true,
    )
    .action(async (options: {
      release: string;
      namespace: string;
      context?: string;
      model?: string;
      wait: boolean;
    }) => {
      const helmArgs = [
        "upgrade",
        options.release,
        "deploy/helm/kars",
        "--namespace", options.namespace,
        "--reuse-values",
        "--set", "sre.enabled=true",
      ];
      if (options.model) helmArgs.push("--set", `sre.model=${options.model}`);
      if (options.context) helmArgs.push("--kube-context", options.context);

      console.log(chalk.cyan("▸ enabling kars-sre via helm upgrade --reuse-values…"));
      console.log(chalk.gray(`  helm ${helmArgs.join(" ")}`));
      try {
        await execa("helm", helmArgs, { stdio: "inherit" });
      } catch (err) {
        console.error(chalk.red("✗ helm upgrade failed"));
        process.exit(1);
      }
      console.log(chalk.green("✓ chart patched"));

      if (options.wait) {
        const kctxArgs = options.context ? ["--context", options.context] : [];
        console.log(chalk.cyan("▸ waiting for kars-sre namespace to appear…"));
        for (let i = 0; i < 60; i++) {
          try {
            await execa("kubectl", [...kctxArgs, "get", "ns", "kars-sre"], { stdio: "ignore" });
            console.log(chalk.green("✓ kars-sre namespace exists"));
            break;
          } catch {
            await new Promise((r) => setTimeout(r, 1000));
          }
        }
        console.log(chalk.cyan("▸ waiting for sre sandbox to reach Running (up to 180s)…"));
        try {
          await execa(
            "kubectl",
            [
              ...kctxArgs,
              "-n", "kars-sre",
              "wait",
              "--for=condition=Available",
              "deploy/sre",
              "--timeout=180s",
            ],
            { stdio: "inherit" },
          );
          console.log(chalk.green("✓ kars-sre is ready"));
          console.log("");
          console.log(`  ${chalk.bold("Next:")}  ${chalk.cyan("kars sre talk")}    (open the WebUI)`);
          console.log(`         ${chalk.cyan("kars sre status")}  (CR + pod state)`);
        } catch {
          console.warn(chalk.yellow("⚠ sre sandbox did not become Available within 180s"));
          console.warn(chalk.yellow("  Run `kars sre status` to inspect."));
          process.exit(1);
        }
      }
    });

  cmd
    .command("uninstall")
    .description("Disable the kars-sre agent (the namespace + RBAC are torn down by the controller)")
    .option("--release <name>", "Helm release name", "kars")
    .option("--namespace <ns>", "Helm release namespace", "kars-system")
    .option("--context <name>", "kubectl context to use")
    .action(async (options: { release: string; namespace: string; context?: string }) => {
      const helmArgs = [
        "upgrade",
        options.release,
        "deploy/helm/kars",
        "--namespace", options.namespace,
        "--reuse-values",
        "--set", "sre.enabled=false",
      ];
      if (options.context) helmArgs.push("--kube-context", options.context);

      console.log(chalk.cyan("▸ disabling kars-sre via helm upgrade --reuse-values…"));
      try {
        await execa("helm", helmArgs, { stdio: "inherit" });
      } catch {
        console.error(chalk.red("✗ helm upgrade failed"));
        process.exit(1);
      }
      console.log(chalk.green("✓ kars-sre disabled; controller will garbage-collect the sandbox + namespace"));
    });

  cmd
    .command("status")
    .description("Show the sre KarsSandbox CR + pod state")
    .option("--context <name>", "kubectl context to use")
    .action(async (options: { context?: string }) => {
      const kctxArgs = options.context ? ["--context", options.context] : [];
      console.log(chalk.bold.cyan("── KarsSandbox sre (kars-system) ──"));
      try {
        await execa("kubectl", [...kctxArgs, "-n", "kars-system", "get", "karssandbox", "sre"], { stdio: "inherit" });
      } catch {
        console.error(chalk.yellow("⚠ KarsSandbox sre not found — run `kars sre install` first."));
        process.exit(1);
      }
      console.log("");
      console.log(chalk.bold.cyan("── pods (kars-sre namespace) ──"));
      try {
        await execa("kubectl", [...kctxArgs, "-n", "kars-sre", "get", "pod"], { stdio: "inherit" });
      } catch {
        console.warn(chalk.yellow("⚠ kars-sre namespace not yet provisioned"));
      }
    });

  cmd
    .command("talk")
    .description("Open the kars-sre WebUI (alias for `kars connect sre`)")
    .option("--context <name>", "kubectl context to use")
    .option("--port <port>", "Local port for WebUI port-forward", "18790")
    .action(async (options: { context?: string; port: string }) => {
      const args = ["connect", "sre", "--web", "--port", options.port];
      if (options.context) args.push("--context", options.context);
      console.log(chalk.cyan(`▸ kars connect sre (WebUI on http://localhost:${options.port})…`));
      try {
        await execa("kars", args, { stdio: "inherit" });
      } catch {
        console.error(chalk.red("✗ failed to connect — try `kars sre status` to verify the sandbox is Running"));
        process.exit(1);
      }
    });

  return cmd;
}
