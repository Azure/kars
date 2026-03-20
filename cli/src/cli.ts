import { Command } from "commander";
import { upCommand } from "./commands/up.js";
import { devCommand } from "./commands/dev.js";
import { onboardCommand } from "./commands/onboard.js";
import { connectCommand } from "./commands/connect.js";
import { statusCommand } from "./commands/status.js";
import { logsCommand } from "./commands/logs.js";
import { modelCommand } from "./commands/model.js";
import { traceCommand } from "./commands/trace.js";
import { policyCommand } from "./commands/policy.js";
import { approveCommand } from "./commands/approve.js";
import { destroyCommand } from "./commands/destroy.js";

export function createCli(): Command {
  const program = new Command();

  program
    .name("azureclaw")
    .description(
      "Run AI agents safely on Azure. One command to go from zero to production."
    )
    .version("0.1.0-alpha.1");

  // Primary commands
  program.addCommand(upCommand());
  program.addCommand(devCommand());
  program.addCommand(onboardCommand());

  // Sandbox commands
  program.addCommand(connectCommand());
  program.addCommand(statusCommand());
  program.addCommand(logsCommand());
  program.addCommand(modelCommand());
  program.addCommand(traceCommand());
  program.addCommand(policyCommand());
  program.addCommand(approveCommand());
  program.addCommand(destroyCommand());

  return program;
}
