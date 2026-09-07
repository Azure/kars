// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import { execa } from "execa";
import { adoptNamespace, inspectNamespaceOwnership } from "../lib/namespace-ownership.js";

export function namespaceCommand(): Command {
  const command = new Command("namespace").description("Inspect and explicitly adopt sandbox namespace ownership");
  command.command("preflight")
    .description("Read-only ownership checks in the current Kubernetes context (no Secrets are read)")
    .action(async () => {
      for (const result of await inspectNamespaceOwnership(execa)) console.log(result);
      console.log("Namespace ownership preflight passed");
    });
  command.command("adopt")
    .description("Explicit administrator adoption of an unclaimed legacy namespace; preserves all workloads and data")
    .argument("<name>", "Existing Sandbox name")
    .requiredOption("--namespace <namespace>", "Namespace containing the KarsSandbox CR")
    .requiredOption("--sandbox-uid <uid>", "Reviewed live KarsSandbox UID")
    .requiredOption("--namespace-uid <uid>", "Reviewed live target namespace UID")
    .action(async (name: string, options: { namespace: string; sandboxUid: string; namespaceUid: string }) => {
      await adoptNamespace(execa, name, options.namespace, options.sandboxUid, options.namespaceUid);
      console.log("Namespace claim recorded; the controller will verify it before reconciliation");
    });
  return command;
}
