// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";

export function modelCommand(): Command {
  const cmd = new Command("model");

  cmd.description("Manage the AI model for a sandbox");

  cmd
    .command("set")
    .description("Switch AI model (instant, no restart)")
    .argument("<name>", "Sandbox name")
    .argument("<model>", "Model name (e.g. gpt-4.1, Phi-4, Meta-Llama-3.1-405B-Instruct)")
    .action(async (name: string, model: string) => {
      const { execa } = await import("execa");
      const spinner = ora(`Switching ${name} to ${model}...`).start();

      try {
        // Post-S10/S13: model preference lives on the InferencePolicy CR
        // referenced by spec.inferenceRef.name (NOT spec.inference.model;
        // that field was removed by the schema in S10.A1 and is silently
        // dropped on patch). Patch the InferencePolicy directly.
        const { stdout: refStdout } = await execa("kubectl", [
          "get", "clawsandbox", name,
          "-n", "azureclaw-system",
          "-o", "jsonpath={.spec.inferenceRef.name}",
        ], { stdio: "pipe" });
        const refName = refStdout.trim();
        if (!refName) {
          throw new Error(
            `sandbox '${name}' has no spec.inferenceRef.name — please mint an InferencePolicy first`,
          );
        }
        await execa("kubectl", [
          "patch", "inferencepolicy", refName,
          "-n", "azureclaw-system",
          "--type", "merge",
          "-p", JSON.stringify({
            spec: {
              modelPreference: {
                primary: { provider: "azure-openai", deployment: model },
              },
            },
          }),
        ], { stdio: "pipe" });

        // Mirror the model preference onto the sandbox annotation so
        // `model get` and other read paths stay consistent.
        await execa("kubectl", [
          "annotate", "clawsandbox", name,
          "-n", "azureclaw-system",
          `azureclaw.azure.com/model=${model}`,
          "--overwrite",
        ], { stdio: "pipe" }).catch(() => {});

        // Update the inference router env var on the deployment
        const namespace = `azureclaw-${name}`;
        await execa("kubectl", [
          "set", "env", `deploy/${name}`,
          "-n", namespace,
          "-c", "inference-router",
          `DEFAULT_MODEL=${model}`,
          `AZURE_OPENAI_DEPLOYMENT=${model}`,
        ], { stdio: "pipe" }).catch(() => {});

        spinner.succeed(`${name} now using ${chalk.bold(model)}`);
        console.log(chalk.dim("  Model switch takes effect on next request.\n"));
      } catch (error) {
        spinner.fail("Failed to switch model");
        const message = error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`\nError: ${message}\n`));
        process.exit(1);
      }
    });

  cmd
    .command("get")
    .description("Show current model for a sandbox")
    .argument("<name>", "Sandbox name")
    .action(async (name: string) => {
      const { execa } = await import("execa");
      try {
        // Post-S10/S13: model preference lives on the InferencePolicy CR
        // referenced by spec.inferenceRef.name. Resolve via that ref; fall
        // back to the metadata annotation; finally to legacy spec.inference.
        const { stdout: sbJson } = await execa("kubectl", [
          "get", "clawsandbox", name,
          "-n", "azureclaw-system",
          "-o", "json",
        ], { stdio: "pipe" });
        const obj = JSON.parse(sbJson);
        const refName: string | undefined = obj.spec?.inferenceRef?.name;
        const annotated: string | undefined = obj.metadata?.annotations?.["azureclaw.azure.com/model"];
        let model = "";
        if (refName) {
          try {
            const { stdout: mp } = await execa("kubectl", [
              "get", "inferencepolicy", refName,
              "-n", "azureclaw-system",
              "-o", "jsonpath={.spec.modelPreference.primary.deployment}",
            ], { stdio: "pipe" });
            model = mp.trim();
          } catch { /* fall through */ }
        }
        if (!model) {
          model = annotated || obj.spec?.inference?.model || "gpt-4.1";
        }
        console.log(`\n  ${name}: ${chalk.bold(model)} (Foundry)\n`);
      } catch {
        console.log(chalk.red(`\n  Sandbox '${name}' not found.\n`));
      }
    });

  cmd
    .command("list")
    .description("List available models from Foundry")
    .argument("[name]", "Sandbox name (queries live from Foundry if provided)")
    .action(async (name?: string) => {
      if (name) {
        // Query live from the inference router inside the sandbox
        const { execa } = await import("execa");
        const namespace = `azureclaw-${name}`;
        try {
          const { stdout } = await execa("kubectl", [
            "exec", "-n", namespace, `deploy/${name}`,
            "-c", "inference-router", "--",
            "sh", "-c", "curl -s http://localhost:8443/v1/models",
          ], { stdio: "pipe" });
          const data = JSON.parse(stdout);
          const models = (data.data || []).map((m: any) => m.id).sort();
          console.log(chalk.bold(`\n  Models available on ${name} (${models.length} total):\n`));
          // Group by prefix
          const groups: Record<string, string[]> = {};
          for (const m of models) {
            const prefix = m.split("-")[0];
            (groups[prefix] = groups[prefix] || []).push(m);
          }
          for (const [prefix, ms] of Object.entries(groups).slice(0, 15)) {
            console.log(`  ${prefix}: ${ms.slice(0, 5).join(", ")}${ms.length > 5 ? ` (+${ms.length - 5} more)` : ""}`);
          }
          if (Object.keys(groups).length > 15) {
            console.log(chalk.dim(`  ... and ${Object.keys(groups).length - 15} more groups`));
          }
          console.log(chalk.dim(`\n  Total: ${models.length} models. Switch with: azureclaw model set ${name} <model>\n`));
        } catch {
          console.log(chalk.red(`\n  Could not query models from '${name}'. Is the sandbox running?\n`));
        }
      } else {
        console.log(chalk.bold("\n  Available models (Foundry):\n"));
        console.log("  OpenAI:     gpt-4.1, gpt-4o, gpt-5-mini, o3-mini, o4-mini");
        console.log("  Microsoft:  Phi-4, Phi-4-mini-instruct, Phi-4-reasoning");
        console.log("  Meta:       Llama-3.3-70B-Instruct, Llama-4-Scout");
        console.log("  DeepSeek:   DeepSeek-V3.2, DeepSeek-R1");
        console.log("  Mistral:    Mistral-small-2503, Codestral-2501");
        console.log("  Anthropic:  claude-sonnet-4-5, claude-opus-4-6");
        console.log("  xAI:        grok-3, grok-4-fast-reasoning");
        console.log(chalk.dim("\n  200+ models. Use: azureclaw model list <sandbox> for live query.\n"));
      }
    });

  return cmd;
}
