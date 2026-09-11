// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { listSreHelmReleases as listHelmReleases } from "./sre-helm.js";
import { schemaDocuments, type ObjectMap, type SchemaExecute } from "./schema-documents.js";
import { stageCoreSchemaDocuments, type SchemaStageOptions } from "./schema-stage.js";

const valueFlags = new Set(["--set", "--set-string", "--set-json", "--set-file", "--set-literal", "--values", "-f"]);
const ignoredValues = new Set(["--timeout", "--history-max", "--description", "--post-renderer", "--post-renderer-args"]);

export function schemaContextExecutor(execute: SchemaExecute, context?: string, kubeconfig?: string): SchemaExecute {
  return (file, args, options) => execute(file, [
    ...args,
    ...(context ? [file === "helm" ? "--kube-context" : "--context", context] : []),
    ...(kubeconfig ? ["--kubeconfig", kubeconfig] : []),
  ], options);
}

export async function renderCoreSchemaChart(execute: SchemaExecute, args: readonly string[]): Promise<{
  run: SchemaExecute; documents: ObjectMap[]; release: string; namespace: string;
}> {
  if (!["install", "upgrade"].includes(args[0])) throw new Error("Core schema preflight requires an install or upgrade invocation");
  const positional: string[] = [];
  const values: string[] = [];
  let namespace = "";
  let context: string | undefined;
  let kubeconfig: string | undefined;
  for (let index = 1; index < args.length; index++) {
    const token = args[index];
    if (!token.startsWith("-")) { positional.push(token); continue; }
    const [flag, inline] = token.split(/=(.*)/s);
    const needsValue = valueFlags.has(flag) || ignoredValues.has(flag)
      || ["--namespace", "-n", "--kube-context", "--kubeconfig"].includes(flag);
    const value = inline ?? (needsValue ? args[++index] : undefined);
    if (needsValue && (!value || value.startsWith("--"))) throw new Error(`Missing ${flag} value for schema preflight`);
    if (flag === "--post-renderer" || flag === "--post-renderer-args") throw new Error("Core schema preflight cannot prove a post-rendered chart");
    if (flag.startsWith("--kube-") && flag !== "--kube-context") throw new Error("Pin non-context Kubernetes transport options in the executor before schema preparation");
    if (valueFlags.has(flag)) {
      if ((flag === "-f" || flag === "--values") && value === "-") throw new Error("Use a values file, not stdin, for the shared schema preflight");
      values.push(flag, value!);
    }
    if (flag === "--namespace" || flag === "-n") namespace = value!;
    if (flag === "--kube-context") context = value;
    if (flag === "--kubeconfig") kubeconfig = value;
  }
  if (positional.length !== 2 || !namespace) throw new Error("Core schema preflight requires an explicit release, chart and namespace");
  const [release, chart] = positional;
  const run = schemaContextExecutor(execute, context, kubeconfig);
  const reuse = !args.includes("--reset-values")
    && (args.includes("--reuse-values") || args.includes("--reset-then-reuse-values") || values.length === 0);
  let input: string | undefined;
  let upgrading = false;
  if (args[0] === "upgrade") {
    const releases: unknown = JSON.parse(await listHelmReleases(run, namespace));
    if (!Array.isArray(releases) || releases.some(item => typeof item?.name !== "string" || item.namespace !== namespace)) {
      throw new Error("Helm release inventory is invalid during schema preparation");
    }
    upgrading = releases.some(item => item.name === release);
    if (upgrading && reuse) {
      const result = await run("helm", ["get", "values", release, "-n", namespace, "-o", "json",
        ...(args.includes("--reuse-values") ? ["--all"] : [])], { stdio: "pipe" });
      const saved: unknown = JSON.parse(result.stdout);
      if (saved !== null && (typeof saved !== "object" || Array.isArray(saved))) throw new Error("Helm values snapshot is invalid");
      input = JSON.stringify(saved ?? {});
    }
  }
  const rendered = await run("helm", ["template", release, chart, "--namespace", namespace, "--include-crds",
    ...(upgrading ? ["--is-upgrade"] : []), ...(input ? ["-f", "-"] : []), ...values],
  { stdio: "pipe", ...(input ? { input } : {}) });
  return { run, documents: schemaDocuments(rendered.stdout), release, namespace };
}

export async function prepareCoreHelmSchemas(execute: SchemaExecute, args: readonly string[]): Promise<void> {
  const { run, documents, release, namespace } = await renderCoreSchemaChart(execute, args);
  await stageCoreSchemaDocuments(run, documents, { release, namespace, ownership: "helm" });
}

/** Template installations share the same lifecycle; subsequent SSA excludes
 * CRDs so it cannot overwrite the reviewed schema owner's fields. */
export async function prepareCoreTemplateSchemas(
  execute: SchemaExecute, rendered: string, options: SchemaStageOptions,
): Promise<string> {
  const documents = schemaDocuments(rendered);
  await stageCoreSchemaDocuments(execute, documents, options);
  return documents.filter((object: ObjectMap) => object.kind !== "CustomResourceDefinition").map(object => JSON.stringify(object)).join("\n---\n");
}

export async function prepareCoreRollbackSchemas(execute: SchemaExecute, release: string, namespace: string): Promise<number> {
  const latest = async () => {
    const history: unknown = JSON.parse((await execute("helm", ["history", release, "-n", namespace, "-o", "json"], { stdio: "pipe" })).stdout);
    if (!Array.isArray(history) || !history.length || history.some(item => !Number.isSafeInteger(item?.revision) || item.revision < 1)) {
      throw new Error("Helm rollback history is invalid");
    }
    return Math.max(...history.map(item => item.revision));
  };
  const current = await latest();
  if (current <= 1) throw new Error("No previous core Helm revision exists");
  const target = current - 1;
  const manifest = async (revision: number) => schemaDocuments((await execute("helm",
    ["get", "manifest", release, "-n", namespace, "--revision", String(revision)], { stdio: "pipe" })).stdout);
  const previous = await manifest(target);
  const active = await manifest(current);
  const retained = new Set(previous.filter(object => object.kind === "CustomResourceDefinition").map(object => object.metadata.name));
  if (active.some(object => object.kind === "CustomResourceDefinition" && !retained.has(object.metadata.name))) {
    throw new Error("Rollback would remove a core CRD; explicit schema/data migration is required");
  }
  await stageCoreSchemaDocuments(execute, previous, { release, namespace, ownership: "helm" });
  if (await latest() !== current) throw new Error("Helm history changed during schema preparation; rollback was not issued");
  return target;
}
