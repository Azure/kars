import { Command } from "commander";
import { upCommand } from "./commands/up.js";
import { devCommand } from "./commands/dev.js";
import { addCommand } from "./commands/add.js";
import { credentialsCommand } from "./commands/credentials.js";
import { connectCommand } from "./commands/connect.js";
import { statusCommand } from "./commands/status.js";
import { listCommand } from "./commands/list.js";
import { logsCommand } from "./commands/logs.js";
import { pushCommand } from "./commands/push.js";
import { modelCommand } from "./commands/model.js";
import { traceCommand } from "./commands/trace.js";
import { policyCommand } from "./commands/policy.js";
import { egressCommand } from "./commands/egress.js";
import { destroyCommand } from "./commands/destroy.js";
import { evalCommand } from "./commands/eval.js";
import { operatorCommand } from "./commands/operator.js";

export function createCli(): Command {
  const program = new Command();

  program
    .name("azureclaw")
    .description(
      "Run AI agents safely on Azure. One command to go from zero to production."
    )
    .version("0.1.0-alpha.1");

  // Lifecycle
  program.addCommand(upCommand());
  program.addCommand(devCommand());
  program.addCommand(addCommand());
  program.addCommand(pushCommand());
  program.addCommand(destroyCommand());

  // Operations
  program.addCommand(connectCommand());
  program.addCommand(statusCommand());
  program.addCommand(listCommand());
  program.addCommand(logsCommand());

  // Configuration
  program.addCommand(credentialsCommand());
  program.addCommand(modelCommand());
  program.addCommand(policyCommand());
  program.addCommand(egressCommand());

  // Observability
  program.addCommand(traceCommand());
  program.addCommand(evalCommand());
  program.addCommand(operatorCommand());

  return program;
}
