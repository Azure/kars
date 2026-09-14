// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import { execa } from "execa";
import { requireBundledAsset } from "../lib/repo-assets.js";
import { prepareCoreHelmSchemas, schemaContextExecutor } from "../lib/core-helm-schemas.js";

export function schemasCommand(): Command {
  const command = new Command("schemas").description("Prepare exact core CRDs and published schemas before installing admission");
  const collect = (value: string, values: string[]) => [...values, value];
  command.command("prepare")
    .requiredOption("--release <name>", "Intended core Helm/template release")
    .requiredOption("--namespace <namespace>", "Intended core release namespace")
    .option("--chart <path>", "Exact chart used for the following installation; defaults to the bundled core chart")
    .option("--ownership <mode>", "helm or template; never adopts foreign/unmarked CRDs", "helm")
    .option("--context <name>", "Exact Kubernetes context")
    .option("-f, --values <file>", "Chart values file", collect, [])
    .option("--set <value>", "Chart value override", collect, [])
    .option("--set-string <value>", "Chart string override", collect, [])
    .option("--reuse-values", "Mirror a Helm upgrade that reuses computed release values")
    .option("--reset-then-reuse-values", "Mirror a Helm upgrade that overlays saved user values on new defaults")
    .option("--atomic", "Verify the same automatic rollback safety as the following Helm operation")
    .option("--rollback-on-failure", "Helm 4 automatic rollback safety; equivalent to --atomic for preparation")
    .option("--timeout <seconds>", "Bounded establishment/discovery deadline (1-600)", "120")
    .option("--check", "Read-only verification of already staged owned schemas")
    .action(async options => {
      if (!["helm", "template"].includes(options.ownership)) throw new Error("--ownership must be helm or template");
      const seconds = Number(options.timeout);
      if (!Number.isInteger(seconds) || seconds < 1 || seconds > 600) throw new Error("--timeout must be 1-600 seconds");
      if (options.values.includes("-")) throw new Error("--values requires a file, not stdin");
      if (options.reuseValues && options.resetThenReuseValues) throw new Error("Select one Helm values reuse mode");
      const reuse = options.reuseValues || options.resetThenReuseValues;
      if (reuse && options.ownership !== "helm") throw new Error("Values reuse requires Helm ownership");
      if ((options.atomic || options.rollbackOnFailure) && options.ownership !== "helm") throw new Error("Automatic rollback requires Helm ownership");
      const execute = schemaContextExecutor((file, args, settings) => execa(file, args, settings), options.context);
      const result = await prepareCoreHelmSchemas(execute, [options.ownership === "helm" ? "upgrade" : "install",
        "--install", options.release, options.chart ?? requireBundledAsset("deploy/helm/kars"),
        "--namespace", options.namespace, ...(options.reuseValues ? ["--reuse-values"] : []),
        ...(options.resetThenReuseValues ? ["--reset-then-reuse-values"] : []),
        ...(options.atomic ? ["--atomic"] : []), ...(options.rollbackOnFailure ? ["--rollback-on-failure"] : []),
        ...options.values.flatMap((file: string) => ["-f", file]),
        ...options.set.flatMap((value: string) => ["--set", value]),
        ...options.setString.flatMap((value: string) => ["--set-string", value])], {
        release: options.release, namespace: options.namespace, ownership: options.ownership,
        checkOnly: Boolean(options.check), timeoutMs: seconds * 1000,
      });
      console.log(JSON.stringify({ ...result, release: options.release, namespace: options.namespace, ownership: options.ownership }));
    });
  return command;
}
