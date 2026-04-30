// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";

export function evalCommand(): Command {
  const cmd = new Command("eval");

  cmd
    .description("Run Foundry evaluations against a sandbox agent")
    .argument("<name>", "Sandbox name")
    .option("--model <model>", "Model to evaluate", "gpt-4.1")
    .option("--evaluator <id>", "Evaluator ID (e.g., relevance, coherence, fluency)")
    .option("--dataset <path>", "JSONL dataset file with test cases")
    .option("--list-evaluators", "List available evaluators in the project")
    .option("--list-runs", "List existing evaluation runs")
    .action(async (name: string, options) => {
      const blue = chalk.hex("#0078D4");
      const bold = chalk.bold;

      try {
        const { execa } = await import("execa");

        // Resolve the pod and namespace
        const namespace = `azureclaw-${name}`;
        let podName: string;

        try {
          const { stdout } = await execa("kubectl", [
            "get", "pods", "-n", namespace,
            "-o", "jsonpath={.items[0].metadata.name}",
          ], { stdio: "pipe" });
          podName = stdout.trim();
        } catch {
          console.error(chalk.red(`No sandbox '${name}' found. Run 'azureclaw up' first.`));
          process.exit(1);
        }

        const V = "api-version=2025-11-15-preview";

        // Helper: exec curl inside the pod via the inference router
        async function foundryGet(path: string): Promise<unknown> {
          const { stdout } = await execa("kubectl", [
            "exec", "-n", namespace, podName, "-c", "openclaw", "--",
            "curl", "-s", `http://localhost:8443/${path}?${V}`,
          ], { stdio: "pipe" });
          return JSON.parse(stdout);
        }

        async function foundryPost(path: string, body: unknown): Promise<unknown> {
          const { stdout } = await execa("kubectl", [
            "exec", "-n", namespace, podName, "-c", "openclaw", "--",
            "curl", "-s", "-X", "POST",
            `http://localhost:8443/${path}?${V}`,
            "-H", "Content-Type: application/json",
            "-d", JSON.stringify(body),
          ], { stdio: "pipe" });
          return JSON.parse(stdout);
        }

        // List evaluators
        if (options.listEvaluators) {
          console.log(blue("\n  Foundry Evaluators\n"));
          const data = await foundryGet("evaluators") as { value?: Array<{ id: string; displayName?: string; description?: string }> };
          const evaluators = data.value || [];
          if (evaluators.length === 0) {
            console.log("  No evaluators found in the project.");
          } else {
            for (const e of evaluators) {
              console.log(`  ${chalk.green("•")} ${bold(e.id)} — ${e.displayName || e.description || ""}`);
            }
          }
          console.log(`\n  Total: ${evaluators.length} evaluators\n`);
          return;
        }

        // List eval runs
        if (options.listRuns) {
          console.log(blue("\n  Evaluation Runs\n"));
          const data = await foundryGet("openai/evals") as { data?: Array<{ id: string; name?: string; metadata?: Record<string, string> }> };
          const evals = data.data || [];
          if (evals.length === 0) {
            console.log("  No evaluation runs found.");
          } else {
            for (const e of evals) {
              console.log(`  ${chalk.green("•")} ${bold(e.id)} ${e.name ? `(${e.name})` : ""}`);
            }
          }
          console.log(`\n  Total: ${evals.length} runs\n`);
          return;
        }

        // Create and run an evaluation
        if (!options.dataset) {
          console.error(chalk.red("--dataset <path> is required to run an evaluation."));
          console.log("\nUsage examples:");
          console.log(`  ${bold("azureclaw eval my-agent --list-evaluators")}  — list available evaluators`);
          console.log(`  ${bold("azureclaw eval my-agent --dataset test.jsonl --evaluator relevance")}  — run eval`);
          console.log(`  ${bold("azureclaw eval my-agent --list-runs")}  — list past runs`);
          process.exit(1);
        }

        const { readFileSync } = await import("fs");
        const datasetContent = readFileSync(options.dataset, "utf-8").trim();
        const lines = datasetContent.split("\n").filter(Boolean);

        console.log(blue(`\n  Running evaluation on sandbox '${name}'\n`));
        console.log(`  Model:     ${bold(options.model)}`);
        console.log(`  Evaluator: ${bold(options.evaluator || "default")}`);
        console.log(`  Dataset:   ${bold(options.dataset)} (${lines.length} test cases)`);
        console.log(`  Pod:       ${bold(podName)}`);
        console.log();

        // Create the eval via OpenAI Evals API
        const evalBody: Record<string, unknown> = {
          name: `azureclaw-eval-${Date.now()}`,
          data_source_config: {
            type: "custom",
            item_schema: {
              type: "object",
              properties: {
                input: { type: "string" },
                expected: { type: "string" },
              },
              required: ["input"],
            },
            include_sample_schema: true,
          },
          testing_criteria: [] as Array<{ type: string; model?: string; input?: Array<{ role: string; content: string }> }>,
        };

        // Add evaluator criteria if specified
        if (options.evaluator) {
          (evalBody.testing_criteria as Array<unknown>).push({
            type: "label_model",
            model: options.model,
            input: [
              { role: "system", content: `You are an evaluator. Score the response for ${options.evaluator} on a scale of 1-5.` },
              { role: "user", content: "Input: {{item.input}}\nResponse: {{sample.output_text}}\nExpected: {{item.expected}}" },
            ],
            labels: ["1", "2", "3", "4", "5"],
            passing_labels: ["4", "5"],
          });
        }

        const result = await foundryPost("openai/evals", evalBody) as { id?: string; error?: { message: string } };

        if (result.error) {
          console.error(chalk.red(`  Eval creation failed: ${result.error.message}`));
          process.exit(1);
        }

        console.log(chalk.green(`  ✓ Evaluation created: ${result.id}`));
        console.log(`\n  Run 'azureclaw eval ${name} --list-runs' to check status.`);
      } catch (error) {
        console.error(chalk.red(`Error: ${error instanceof Error ? error.message : String(error)}`));
        process.exit(1);
      }
    });

  return cmd;
}
