// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { isIP } from "node:net";
import { pathToFileURL } from "node:url";
import { preparePendingCoreHelmSchemas, type CoreSchemaHookOptions } from "./lib/core-helm-schema-hook.js";
import type { SchemaExecute } from "./lib/schema-documents.js";

async function inClusterExecute(namespace: string) {
  const host = process.env.KUBERNETES_SERVICE_HOST ?? "";
  const port = process.env.KUBERNETES_SERVICE_PORT ?? "";
  if (isIP(host) !== 4 || !/^[1-9][0-9]*$/.test(port) || Number(port) > 65535) {
    throw new Error("Schema hook requires the reviewed in-cluster IPv4 API endpoint");
  }
  const account = "/var/run/secrets/kubernetes.io/serviceaccount";
  const [actualNamespace, token, ca] = await Promise.all([
    readFile(`${account}/namespace`, "utf8"), readFile(`${account}/token`, "utf8"), readFile(`${account}/ca.crt`, "utf8"),
  ]);
  if (actualNamespace.trim() !== namespace || !token.trim() || !ca.trim()) {
    throw new Error("Schema hook requires its own projected service-account credentials");
  }
  const directory = await mkdtemp("/tmp/kars-schema-");
  const config = `${directory}/config.json`;
  try {
    // An explicit kubeconfig prevents client-go from falling back to local defaults.
    await writeFile(config, JSON.stringify({ apiVersion: "v1", kind: "Config",
      clusters: [{ name: "in-cluster", cluster: { server: `https://${host}:${port}`, "certificate-authority": `${account}/ca.crt` } }],
      users: [{ name: "schema-hook", user: { tokenFile: `${account}/token` } }],
      contexts: [{ name: "schema-hook", context: { cluster: "in-cluster", user: "schema-hook", namespace } }],
      "current-context": "schema-hook" }), { mode: 0o600, flag: "wx" });
  } catch (error) {
    await rm(directory, { recursive: true, force: true });
    throw error;
  }
  const execute: SchemaExecute = (file, args, options) => new Promise((resolve, reject) => {
    if (!["helm", "kubectl"].includes(file)) {
      reject(new Error("Schema hook only executes Helm and kubectl"));
      return;
    }
    const child = execFile(file, ["--kubeconfig", config, ...args], { encoding: "utf8", timeout: options.timeout,
      env: { PATH: process.env.PATH, HOME: "/tmp", XDG_CACHE_HOME: "/tmp/cache", HELM_DRIVER: "secret" },
      maxBuffer: 8 * 1024 * 1024, killSignal: "SIGKILL", windowsHide: true }, (error, stdout, stderr) => {
      if (error) reject(Object.assign(error, { stderr, exitCode: typeof error.code === "number" ? error.code : undefined }));
      else resolve({ stdout });
    });
    child.stdin?.on("error", error => { child.kill("SIGKILL"); reject(error); });
    child.stdin?.end(options.input);
  });
  return { execute, close: () => rm(directory, { recursive: true, force: true }) };
}

export async function runSchemaHook(args: readonly string[], execute?: SchemaExecute) {
  const allowed = new Set(["--release", "--namespace", "--revision", "--operation", "--installer-helm-version", "--timeout-ms"]);
  const values = new Map<string, string>();
  for (let index = 0; index < args.length; index += 2) {
    const flag = args[index];
    const value = args[index + 1];
    if (!allowed.has(flag) || !value || value.startsWith("--") || values.has(flag)) {
      throw new Error("Schema hook accepts each exact release option once and no additional flags");
    }
    values.set(flag, value);
  }
  if ([...allowed].filter(flag => flag !== "--timeout-ms").some(flag => !values.has(flag))
    || !/^[1-9][0-9]*$/.test(values.get("--revision")!)
    || (values.has("--timeout-ms") && !/^[1-9][0-9]*$/.test(values.get("--timeout-ms")!))) {
    throw new Error("Schema hook requires exact release, namespace, integer revision, operation and installer version");
  }
  const options: CoreSchemaHookOptions = { release: values.get("--release")!, namespace: values.get("--namespace")!,
    revision: Number(values.get("--revision")), operation: values.get("--operation") as CoreSchemaHookOptions["operation"],
    installerHelmVersion: values.get("--installer-helm-version")!,
    timeoutMs: values.has("--timeout-ms") ? Number(values.get("--timeout-ms")) : 120_000 };
  if (execute) return preparePendingCoreHelmSchemas(execute, options);
  const subprocesses = await inClusterExecute(options.namespace);
  try {
    return await preparePendingCoreHelmSchemas(subprocesses.execute, options);
  } finally {
    await subprocesses.close();
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  runSchemaHook(process.argv.slice(2)).then(result => console.log(JSON.stringify(result))).catch(() => {
    // Never echo Helm manifests, subprocess output, credentials or command arguments into Job logs.
    console.error("Core schema staging failed. Helm must not proceed; no cluster cleanup, force or automatic write retry was attempted.");
    process.exitCode = 1;
  });
}
