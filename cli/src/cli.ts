// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import { upCommand } from "./commands/up.js";
import { devCommand } from "./commands/dev.js";
import { addCommand } from "./commands/add.js";
import { credentialsCommand } from "./commands/credentials.js";
import { configCommand } from "./commands/config.js";
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
import { handoffCommand } from "./commands/handoff.js";
import { meshCommand } from "./commands/mesh.js";
import { pairCommand } from "./commands/pair.js";
import { convertCommand } from "./commands/convert.js";
import { a2aCommand, a2aAgentCommand } from "./commands/a2a.js";
import { attestCommand } from "./commands/attest.js";
import { migrateCommand } from "./commands/migrate.js";
import { toolPolicyCommand } from "./commands/toolpolicy.js";
import { inferencePolicyCommand } from "./commands/inferencepolicy.js";
import { mcpCommand } from "./commands/mcp.js";
import { memoryCommand } from "./commands/memory.js";
import { inspectCommand } from "./commands/inspect.js";
import { auditCommand } from "./commands/audit.js";

export function createCli(): Command {
  const program = new Command();

  program
    .name("kars")
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
  program.addCommand(inspectCommand());

  // Configuration
  program.addCommand(credentialsCommand());
  program.addCommand(configCommand());
  program.addCommand(modelCommand());
  program.addCommand(policyCommand());
  program.addCommand(egressCommand());

  // Observability
  program.addCommand(traceCommand());
  program.addCommand(evalCommand());
  program.addCommand(operatorCommand());
  program.addCommand(auditCommand());

  // Agent mobility
  program.addCommand(handoffCommand());
  program.addCommand(meshCommand());
  program.addCommand(pairCommand());

  // Interop
  program.addCommand(convertCommand());
  program.addCommand(a2aCommand());
  program.addCommand(a2aAgentCommand());
  program.addCommand(migrateCommand());

  // Governance CRDs
  program.addCommand(toolPolicyCommand());
  program.addCommand(inferencePolicyCommand());
  program.addCommand(mcpCommand());
  program.addCommand(memoryCommand());

  // Attestation
  program.addCommand(attestCommand());

  program.addHelpText("after", `
Command groups:
  Lifecycle       up, dev, add, push, destroy
  Operations      connect, status, list, logs, inspect
  Configuration   credentials, model, policy, egress, config
  Observability   trace, eval, operator, audit
  Agent mobility  handoff, mesh, pair
  Interop         convert, a2a, a2a-agent, migrate
  Governance      toolpolicy, inferencepolicy, mcp, memory
  Attestation     attest

Quick start:
  kars up                    # Provision Azure + deploy controller + first sandbox
  kars add my-bot            # Add a sandbox to an existing cluster
  kars operator              # Live TUI dashboard for all sandboxes
  kars <cmd> --help          # Detailed help for any subcommand

Docs: https://github.com/Azure/kars#readme
`);

  return program;
}
