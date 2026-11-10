// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash, createPrivateKey, randomUUID, X509Certificate } from "node:crypto";
import { lstatSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { readBoundedFileSync } from "../../lib/bounded-file.js";
import path from "node:path";
import { assertRetiredRoot, retirementState } from "../../lib/private-activation-retirement.js";
import { canonical, read, type Execute, type PrivateActivation } from "../../lib/private-activation.js";
import { setupWorkspaceProviders, waitForWorkspaceProviders } from "../../lib/workspace-provider-setup.js";
import type { BetaExecute, BetaTools } from "./kind-beta.js";
import type { installLocalCoreChart } from "./local-k8s.js";

type ObjectMap = Record<string, any>;
interface BudgetIdentity { certificate: string; privateKey: string; ca: string }
interface RotationRecord extends BudgetIdentity {
  version: 1; attempt: string; binding: string; namespaceUid: string; secretUid: string;
  baselineKey: string; catalogUid: string; catalogVersion: string; oldCa: string;
}
const catalogName = "kars-inference-budget-contracts";
const serviceName = "kars-inference-budget.kars-system.svc";

function privateDirectory(directory: string): void {
  try { mkdirSync(directory, { mode: 0o700 }); }
  catch (error) { if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error; }
  const info = lstatSync(directory);
  if (!info.isDirectory() || info.isSymbolicLink() || (info.mode & 0o077) !== 0
    || (process.getuid && info.uid !== process.getuid())) throw new Error("Provider setup state requires an owned private directory (mode 0700)");
  const ignore = path.join(directory, ".gitignore");
  try { writeFileSync(ignore, "*\n", { flag: "wx", mode: 0o600 }); }
  catch (error) { if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error; }
}

function publicKey(certificate: string): string {
  return createHash("sha256").update(new X509Certificate(certificate).publicKey.export({ type: "spki", format: "der" })).digest("hex");
}

function validateIdentity(identity: BudgetIdentity, hostname = serviceName): void {
  const leaf = new X509Certificate(identity.certificate), ca = new X509Certificate(identity.ca);
  const valid = (certificate: X509Certificate) => Date.now() >= Date.parse(certificate.validFrom) && Date.now() < Date.parse(certificate.validTo);
  if (leaf.ca || !ca.ca || !valid(leaf) || !valid(ca) || !leaf.verify(ca.publicKey) || !leaf.checkIssued(ca)
    || !leaf.checkPrivateKey(createPrivateKey(identity.privateKey)) || !leaf.checkHost(hostname, { subject: "never" })
    || !leaf.keyUsage?.includes("1.3.6.1.5.5.7.3.1")) throw new Error("Generated Kind budget identity failed certificate validation");
}

function readRotationRecord(filename: string): RotationRecord | undefined {
  let content: string;
  try { content = readBoundedFileSync(filename, 65536, "Kind budget rotation state is not a private bounded regular file", true); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw error;
  }
  const record = JSON.parse(content);
  if (!record || record.version !== 1 || ["attempt", "binding", "namespaceUid", "secretUid", "baselineKey", "catalogUid", "catalogVersion", "oldCa", "certificate", "privateKey", "ca"]
    .some(key => typeof record[key] !== "string" || !record[key].length)) throw new Error("Kind budget rotation state is malformed");
  validateIdentity(record);
  return record;
}

export async function issueKindBudgetIdentity(directory: string, execute: BetaExecute): Promise<BudgetIdentity> {
  return issueKindTlsIdentity(directory, execute, serviceName);
}

export async function issueKindTlsIdentity(directory: string, execute: BetaExecute, hostname: string): Promise<BudgetIdentity> {
  if (hostname.length > 253 || !hostname.split(".").every(label => /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(label))) throw new Error("Invalid Kind TLS hostname");
  privateDirectory(directory);
  const temporary = mkdtempSync(path.join(directory, "issuer-"));
  const file = (name: string) => path.join(temporary, name);
  const run = async (args: string[]) => {
    try { await execute("openssl", args, { stdio: "pipe" }); }
    catch { throw new Error("Kind budget certificate issuance failed; private command output is not displayed"); }
  };
  try {
    writeFileSync(file("ca.cnf"), `[req]\ndistinguished_name=dn\nx509_extensions=ca\nprompt=no\n[dn]\nCN=Kars Kind CA\n[ca]\nbasicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\nnameConstraints=critical,permitted;DNS:${hostname}\n`, { mode: 0o600 });
    writeFileSync(file("server.cnf"), `[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=Kars Kind TLS\n[server]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:${hostname}\n`, { mode: 0o600 });
    for (const name of ["ca", "server"]) await run(["genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:prime256v1", "-out", file(`${name}.key`)]);
    await run(["req", "-new", "-x509", "-sha256", "-days", "365", "-key", file("ca.key"), "-out", file("ca.crt"), "-config", file("ca.cnf")]);
    await run(["req", "-new", "-sha256", "-key", file("server.key"), "-out", file("server.csr"), "-config", file("server.cnf")]);
    await run(["x509", "-req", "-sha256", "-days", "365", "-in", file("server.csr"), "-CA", file("ca.crt"), "-CAkey", file("ca.key"),
      "-set_serial", `0x${randomUUID().replaceAll("-", "")}`, "-out", file("server.crt"), "-extfile", file("server.cnf"), "-extensions", "server"]);
    const identity = { certificate: readFileSync(file("server.crt"), "utf8"), privateKey: readFileSync(file("server.key"), "utf8"), ca: readFileSync(file("ca.crt"), "utf8") };
    validateIdentity(identity, hostname);
    return identity;
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

export async function rotateKindProviderBudget(execute: Execute, activation: PrivateActivation, directory: string,
  issue: () => Promise<BudgetIdentity>): Promise<string> {
  privateDirectory(directory);
  const namespace = await read(execute, "namespace", activation.root.namespace.name);
  const state = retirementState(namespace);
  const budget = activation.root.budgetTls;
  if (activation.root.namespace.name !== "kars-system" || !state || state.phase !== "retired" || !state.baseline || !budget) {
    throw new Error("Kind provider setup requires a genuinely retired budget root and recorded baseline before issuance");
  }
  await assertRetiredRoot(execute, activation, state);
  const secret = await read(execute, "secret", budget.secret.name, budget.namespace.name) as ObjectMap;
  const catalog = await read(execute, "configmap", catalogName, "kars-system") as ObjectMap;
  if (secret.metadata.uid !== budget.secret.uid || secret.type !== "kubernetes.io/tls"
    || secret.metadata.annotations?.["kars.azure.com/inference-budget-tls"] !== "v1"
    || secret.metadata.annotations?.["meta.helm.sh/release-name"]
    || catalog.metadata.annotations?.["meta.helm.sh/release-name"] !== "kars"
    || catalog.metadata.annotations?.["meta.helm.sh/release-namespace"] !== "kars-system"
    || catalog.metadata.labels?.["app.kubernetes.io/managed-by"] !== "Helm"
    || catalog.metadata.annotations?.["kars.azure.com/inference-budget-contracts"] !== "v1"
    || typeof catalog.data?.["ca.crt"] !== "string") throw new Error("Kind setup cannot rotate foreign or chart-managed budget identities");
  const filename = path.join(directory, "budget-rotation.json");
  let record = readRotationRecord(filename);
  if (record) {
    if (record.version !== 1 || record.attempt !== state.attempt || record.binding !== state.binding
      || record.namespaceUid !== activation.root.namespace.uid || record.secretUid !== budget.secret.uid
      || record.baselineKey !== state.baseline.keyDigest || record.catalogUid !== catalog.metadata.uid) {
      throw new Error("Kind budget rotation state belongs to a different retirement; retained all evidence");
    }
    validateIdentity(record);
  } else {
    if (budget.keyDigest !== state.baseline.keyDigest || budget.secret.resourceVersion !== state.baseline.resourceVersion) {
      throw new Error("Budget identity changed outside this setup attempt; preserve it and review the external rotation");
    }
    const identity = await issue();
    validateIdentity(identity);
    if (state.exposedKeys.includes(publicKey(identity.certificate))) throw new Error("Kind budget issuer returned a previously exposed key");
    record = { version: 1, attempt: state.attempt, binding: state.binding, namespaceUid: activation.root.namespace.uid,
      secretUid: budget.secret.uid, baselineKey: state.baseline.keyDigest, catalogUid: catalog.metadata.uid,
      catalogVersion: catalog.metadata.resourceVersion, oldCa: catalog.data["ca.crt"], ...identity };
    writeFileSync(filename, JSON.stringify(record), { mode: 0o600, flag: "wx" });
  }
  const encoded = (value: string) => Buffer.from(value).toString("base64");
  const currentKey = publicKey(Buffer.from(secret.data?.["tls.crt"] ?? "", "base64").toString("utf8"));
  const nextKey = publicKey(record.certificate);
  if (state.exposedKeys.includes(nextKey)) throw new Error("Retained rotation has a previously exposed key");
  if (currentKey !== nextKey) {
    if (currentKey !== state.baseline.keyDigest || secret.metadata.resourceVersion !== state.baseline.resourceVersion) {
      throw new Error("Budget identity changed since retirement; no credential was overwritten");
    }
    await assertRetiredRoot(execute, activation, state);
    await execute(["patch", "secret", budget.secret.name, "-n", budget.namespace.name, "--type=merge", "--patch-file=/dev/stdin"], JSON.stringify({
      metadata: { uid: secret.metadata.uid, resourceVersion: secret.metadata.resourceVersion },
      data: { "tls.crt": encoded(record.certificate), "tls.key": encoded(record.privateKey), ...(secret.data?.["ca.crt"] ? { "ca.crt": encoded(record.ca) } : {}) },
    }));
  } else if (secret.data?.["tls.crt"] !== encoded(record.certificate) || secret.data?.["tls.key"] !== encoded(record.privateKey)) {
    throw new Error("Budget rotation does not match this attempt's exact identity");
  }
  if (catalog.data["ca.crt"] !== record.ca) {
    if (catalog.metadata.resourceVersion !== record.catalogVersion || catalog.data["ca.crt"] !== record.oldCa) {
      throw new Error("Budget public trust changed during setup; no catalog data was overwritten");
    }
    await assertRetiredRoot(execute, activation, state);
    await execute(["patch", "configmap", catalogName, "-n", "kars-system", "--type=merge", "--patch-file=/dev/stdin"], JSON.stringify({
      metadata: { uid: record.catalogUid, resourceVersion: record.catalogVersion }, data: { "ca.crt": record.ca },
    }));
  }
  const updated = await read(execute, "secret", budget.secret.name, budget.namespace.name) as ObjectMap;
  const trust = await read(execute, "configmap", catalogName, "kars-system") as ObjectMap;
  if (updated.metadata.uid !== record.secretUid || updated.data?.["tls.crt"] !== encoded(record.certificate)
    || updated.data?.["tls.key"] !== encoded(record.privateKey) || trust.metadata.uid !== record.catalogUid
    || trust.data?.["ca.crt"] !== record.ca) throw new Error("Kind budget rotation did not persist the exact new identity and public trust");
  return record.ca;
}

export async function completeKindBetaProviderSetup(execute: Execute, directory: string, local: BetaExecute,
  publishCA: (ca: string) => Promise<void>): Promise<void> {
  privateDirectory(directory);
  // Check tooling before a private activation can deliberately pause the root.
  try { await local("openssl", ["version"], { stdio: "pipe" }); }
  catch { throw new Error("Kind provider setup requires OpenSSL for post-retirement budget TLS issuance; no root was paused"); }
  const registration = await read(execute, "karssreregistration", "canonical") as ObjectMap;
  const sandbox = registration.spec?.sandbox, runtime = registration.spec?.runtimeNamespace;
  if (typeof sandbox?.name !== "string" || typeof runtime?.name !== "string") throw new Error("Provider setup requires the installed canonical SRE registration");
  let ca: string | undefined;
  await setupWorkspaceProviders(execute, { namespace: "kars-system", bridgeNamespace: "kars-system", controllerProfile: "service-accounts",
    consumers: [`${runtime.name}/Deployment/${sandbox.name}`] }, async activation => {
    ca = await rotateKindProviderBudget(execute, activation, directory, () => issueKindBudgetIdentity(directory, local));
  });
  // Also converge a resumed attempt whose root was qualified before Core publication.
  const record = !ca ? readRotationRecord(path.join(directory, "budget-rotation.json")) : undefined;
  if (record) {
    const namespace = await read(execute, "namespace", "kars-system") as ObjectMap;
    const state = retirementState(namespace);
    const grant = await read(execute, "karscredentialgrants.kars.azure.com", "workspace", "kars-system") as ObjectMap;
    const budget = grant.spec?.privateActivation?.root?.budgetTls;
    if (grant.spec?.privateActivation?.phase !== "qualified" || !budget || !state
      || state.attempt !== record.attempt || state.binding !== record.binding || state.baseline?.keyDigest !== record.baselineKey
      || namespace.metadata.uid !== record.namespaceUid || budget.secret.uid !== record.secretUid) {
      throw new Error("Retained budget publication does not match the qualified workspace");
    }
    const secret = await read(execute, "secret", budget.secret.name, budget.namespace.name) as ObjectMap;
    const trust = await read(execute, "configmap", catalogName, "kars-system") as ObjectMap;
    if (secret.metadata.uid !== record.secretUid || trust.metadata.uid !== record.catalogUid
      || secret.data?.["tls.crt"] !== Buffer.from(record.certificate).toString("base64")
      || secret.data?.["tls.key"] !== Buffer.from(record.privateKey).toString("base64") || trust.data?.["ca.crt"] !== record.ca) {
      throw new Error("Retained budget publication differs from the installed identity or public trust");
    }
    ca = record.ca;
  }
  if (ca) await publishCA(ca);
  await waitForWorkspaceProviders(execute, "kars-system");
  await execute(["rollout", "status", "deployment/kars-controller", "-n", "kars-system", "--timeout=900s"]);
  await execute(["rollout", "status", `deployment/${sandbox.name}`, "-n", runtime.name, "--timeout=900s"]);
  await execute(["wait", "--for=condition=Ready", "karssreregistration/canonical", "--timeout=900s"]);
}

export async function setupInstalledKindBetaProviders(cluster: string, kubeconfig: string, directory: string,
  chart: string, tools: BetaTools, local: BetaExecute, installCore: typeof installLocalCoreChart): Promise<void> {
  if (!/^[a-z0-9][a-z0-9-]{0,39}$/.test(cluster)) throw new Error("Provider setup requires an explicit Kind cluster name");
  const context = `kind-${cluster}`;
  const execute: Execute = async (args, input) => (await local(tools.kubectl,
    ["--kubeconfig", kubeconfig, "--context", context, ...args], { env: tools.env, stdio: "pipe", ...(input ? { input } : {}) })).stdout;
  const helm = async (args: string[]) => (await local(tools.helm,
    ["--kubeconfig", kubeconfig, "--kube-context", context, ...args], { env: tools.env, stdio: "pipe" })).stdout;
  const nodeName = `${cluster}-control-plane`;
  const container = JSON.parse((await local(tools.runtime, ["inspect", nodeName], { env: tools.env, stdio: "pipe" })).stdout);
  if (!Array.isArray(container) || container.length !== 1 || container[0].Name?.replace(/^\//, "") !== nodeName
    || container[0].State?.Running !== true || container[0].Config?.Labels?.["io.x-k8s.kind.cluster"] !== cluster
    || container[0].Config?.Labels?.["io.x-k8s.kind.role"] !== "control-plane") throw new Error("Provider setup requires the running owned Kind control-plane container");
  const trustedCA = Buffer.from(await execute(["config", "view", "--minify", "--raw", "-o", "jsonpath={.clusters[0].cluster.certificate-authority-data}"]), "base64").toString("utf8");
  const nodeCA = (await local(tools.runtime, ["exec", nodeName, "cat", "/etc/kubernetes/pki/ca.crt"], { env: tools.env, stdio: "pipe" })).stdout;
  if (new X509Certificate(trustedCA).fingerprint256 !== new X509Certificate(nodeCA).fingerprint256) {
    throw new Error("The selected kubeconfig does not trust this Kind cluster; no provider setup was attempted");
  }
  const values = async (): Promise<ObjectMap> => {
    const saved = JSON.parse(await helm(["get", "values", "kars", "-n", "kars-system", "--all", "-o", "json"]));
    if (saved?.global?.installationProfile !== "kind-beta" || saved?.inferenceBudget?.tls?.privateKey
      || !saved?.inferenceBudget?.tlsSecretName) throw new Error("Automatic provider setup is restricted to the existing Kind beta profile with external budget TLS");
    return saved;
  };
  await values();
  privateDirectory(directory);
  const upgrade = async (saved: ObjectMap, fencedControllerNoop: boolean) => {
    if (fencedControllerNoop) {
      const controller = await read(execute, "deployment", "kars-controller", "kars-system") as ObjectMap;
      if (!Number.isInteger(controller.spec?.replicas) || controller.spec.replicas < 0) {
        throw new Error("The current controller replica intent is unavailable; no Core upgrade was attempted");
      }
      saved.controller = { ...saved.controller, replicas: controller.spec.replicas, upgradePrecondition: {
        uid: controller.metadata.uid, resourceVersion: controller.metadata.resourceVersion,
      } };
    }
    const file = path.join(directory, `core-values-${randomUUID()}.json`);
    writeFileSync(file, JSON.stringify(saved), { mode: 0o600, flag: "wx" });
    await installCore(tools.helm, tools.kubectl, cluster, "kars", chart, [file], [], undefined, undefined,
      { kubeconfig, fencedControllerNoop });
  };
  const adopted = await execute(["get", "configmap", "kars-sre-helm-adoption", "-n", "kars-system", "--ignore-not-found", "-o", "json"]);
  const expected = JSON.parse(readFileSync(path.join(chart, "files", "inference-budget-admission.json"), "utf8")
    .replaceAll("__ACCOUNTING_NAMESPACE__", "kars-system")).items
    .find((item: ObjectMap) => item.name === "kars-inference-budget-workloads")?.spec;
  if (!expected?.variables || !expected?.validations) throw new Error("The bundled provider-maintenance policy is missing");
  const admissionCurrent = async () => {
    const live = await read(execute, "validatingadmissionpolicy", "kars-inference-budget-workloads") as ObjectMap;
    return canonical(live.spec?.variables ?? []) === canonical(expected.variables)
      && canonical(live.spec?.validations ?? []) === canonical(expected.validations);
  };
  if (!adopted.trim() || !await admissionCurrent()) {
    const saved = await values();
    saved.schemaHook = { ...saved.schemaHook, enabled: false };
    await upgrade(saved, true);
    if (!await admissionCurrent()) throw new Error("Core did not publish the provider-maintenance admission policy");
  }
  await completeKindBetaProviderSetup(execute, directory, local, async ca => {
    const saved = await values();
    if (saved.inferenceBudget.caBundle === ca) return;
    saved.inferenceBudget.caBundle = ca;
    saved.schemaHook = { ...saved.schemaHook, enabled: false };
    await upgrade(saved, true);
    if ((await values()).inferenceBudget.caBundle !== ca) throw new Error("Core did not persist the rotated public budget trust");
  });
}
