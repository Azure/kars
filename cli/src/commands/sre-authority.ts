// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import { execa } from "execa";
import { requireBundledAsset } from "../lib/repo-assets.js";
import { enroll, preview, requireRegistrar, retire, waitForAuthority, type Execute } from "../lib/sre-authority.js";
import { stageSource } from "../lib/sre-source.js";
import { stageAuthority } from "../lib/sre-stage.js";

function executor(context?: string): Execute {
  return (file, args, options) => execa(file, [
    ...(context ? [file === "helm" ? "--kube-context" : "--context", context] : []), ...args,
  ], options);
}

export function authorityCommand(): Command {
  const command = new Command("authority").description("Stage, review, enroll and retire cluster-authorized SRE privacy");
  const common = (name: string) => command.command(name)
    .option("--namespace <namespace>", "Controller/release namespace", "kars-system")
    .option("--release <release>", "Owning Helm release", "kars")
    .option("--context <context>", "Kubernetes context");
  common("stage")
    .description("Explicit registrar staging: install authority APIs/controller while retaining legacy grants unchanged")
    .requiredOption("--controller-image <image>", "Qualified prerequisite controller repository:tag")
    .requiredOption("--router-image <image>", "Qualified prerequisite router repository:tag")
    .option("--dry-run", "Server-side preview without deployment changes")
    .action(async options => {
      const execute = executor(options.context);
      await stageAuthority(execute,requireBundledAsset("deploy/helm/kars"),options.namespace,options.release,
        options.controllerImage,options.routerImage,!!options.dryRun);
      console.log(options.dryRun
        ? "Authority stage server-side preview completed without deployment changes; no controller or schema migration was applied."
        : "Authority controller staged. Preview and explicitly enroll the exact SRE source/grants before normal upgrades.");
    });
  common("preview").description("Read exact enrollment identities and legacy grants; no mutations")
    .action(async options => {
      const spec = await preview(executor(options.context),options.namespace,options.release);
      console.log(JSON.stringify(spec,null,2));
      for (const binding of spec.legacyBindings) {
        console.log(`--binding '${binding.kind}/${binding.namespace ?? ""}/${binding.name}=${binding.uid}@${binding.resourceVersion}'`);
      }
      if (spec.legacyConsumer) console.log(`--consumer '${spec.legacyConsumer.uid}@${spec.legacyConsumer.resourceVersion}'`);
    });
  common("stage-source").description("Atomically create a genuinely new, unprivileged SRE source and wait for its exact claim")
    .option("--model <model>", "SRE model deployment")
    .action(async options => {
      const execute=executor(options.context);
      await requireRegistrar(execute);
      const result=await execute("helm",["template",options.release,requireBundledAsset("deploy/helm/kars"),
        "--namespace",options.namespace,"--show-only","templates/sre.yaml",
        "--set","sre.enabled=true","--set","azure.workloadIdentity.clientId=dummy",
        ...(options.model?["--set-string",`sre.model=${options.model}`]:[])],{stdio:"pipe"});
      const created=await stageSource(execute,result.stdout,options.namespace,options.release);
      console.log(`Created and claimed source: --sandbox-uid ${created.uid} --namespace-uid ${created.namespaceUid}`);
    });
  common("enroll").description("Enroll only reviewed source/namespace and grant UIDs under cluster registrar authority")
    .requiredOption("--sandbox-uid <uid>", "Reviewed canonical Sandbox UID")
    .requiredOption("--namespace-uid <uid>", "Reviewed claimed runtime namespace UID")
    .option("--binding <review>", "Exact binding review from preview; repeat per binding", (value: string, all: string[]) => [...all,value], [])
    .option("--consumer <uid@resourceVersion>", "Reviewed legacy SRE Deployment")
    .option("--registration-uid <uid>", "Required when updating an existing registration")
    .option("--resource-version <version>", "Required when updating an existing registration")
    .option("--dry-run", "Print the enrollment without writing it")
    .action(async options => {
      const execute = executor(options.context);
      const spec = await preview(execute,options.namespace,options.release);
      console.log(await enroll(execute,spec,options,!!options.dryRun));
    });
  common("migrate").description("Wait for the controller to complete the explicitly enrolled migration")
    .action(async options => {
      await requireRegistrar(executor(options.context));
      await waitForAuthority(executor(options.context),"Ready");
      console.log("SRE authority Ready: legacy credentials denied and private renewable identity configured.");
    });
  common("retire").description("Disable and retire a reviewed registration before SRE uninstall")
    .requiredOption("--registration-uid <uid>", "Reviewed registration UID")
    .requiredOption("--resource-version <version>", "Reviewed registration resourceVersion")
    .action(async options => {
      await retire(executor(options.context),options.registrationUid,options.resourceVersion);
      console.log("SRE authority Retired; owned private grants and credentials are revoked.");
    });
  return command;
}
