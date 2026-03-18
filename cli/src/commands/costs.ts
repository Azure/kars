import { Command } from "commander";
import chalk from "chalk";

export function costsCommand(): Command {
  const cmd = new Command("costs");

  cmd
    .description("Show compute + inference cost breakdown for a sandbox")
    .argument("<name>", "Sandbox name")
    .option("--period <period>", "Time period: today, week, month", "today")
    .action(async (name: string, options) => {
      // TODO: Query Azure Cost Management + inference router metrics

      console.log(
        chalk.bold(`\n💰 Costs for ${name} (${options.period})\n`)
      );
      console.log("  ┌──────────────────────────────────────┐");
      console.log("  │  Compute (AKS pod)      $0.42        │");
      console.log("  │  Inference (gpt-4.1)    $1.87        │");
      console.log("  │    Input tokens:  124,500 ($0.62)    │");
      console.log("  │    Output tokens:  83,200 ($1.25)    │");
      console.log("  │  ──────────────────────────────────  │");
      console.log(
        `  │  Total                  ${chalk.bold("$2.29")}        │`
      );
      console.log("  └──────────────────────────────────────┘");
      console.log(
        chalk.dim(
          "\n  Daily budget: $10.00 (77% remaining)\n"
        )
      );
    });

  return cmd;
}
