import { Command } from "commander";
import { upCommand } from "./commands/up.js";
import { devCommand } from "./commands/dev.js";
import { initCommand } from "./commands/init.js";
import { onboardCommand } from "./commands/onboard.js";
import { launchCommand } from "./commands/launch.js";
import { connectCommand } from "./commands/connect.js";
import { statusCommand } from "./commands/status.js";
import { logsCommand } from "./commands/logs.js";
import { modelCommand } from "./commands/model.js";
import { traceCommand } from "./commands/trace.js";
import { costsCommand } from "./commands/costs.js";
import { policyCommand } from "./commands/policy.js";
import { destroyCommand } from "./commands/destroy.js";

export function createCli(): Command {
  const program = new Command();

  program
    .name("azureclaw")
    .description(
      "Run AI agents safely on Azure. One command to go from zero to production."
    )
    .version("0.1.0-alpha.1");

  // Primary commands (most users need only these)
  program.addCommand(upCommand());
  program.addCommand(devCommand());

  // Advanced infrastructure commands
  program.addCommand(initCommand());
  program.addCommand(onboardCommand());

  // Sandbox commands
  program.addCommand(launchCommand());
  program.addCommand(connectCommand());
  program.addCommand(statusCommand());
  program.addCommand(logsCommand());
  program.addCommand(modelCommand());
  program.addCommand(traceCommand());
  program.addCommand(costsCommand());
  program.addCommand(policyCommand());
  program.addCommand(destroyCommand());

  return program;
}
