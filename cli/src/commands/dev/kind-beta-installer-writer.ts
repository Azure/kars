// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { randomUUID, X509Certificate } from "node:crypto";
import { lstatSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { canonical, read, reviewed, type Execute } from "../../lib/private-activation.js";
import { waitForWorkspaceProviders } from "../../lib/workspace-provider-setup.js";
import type { BetaExecute, BetaTools } from "./kind-beta.js";

export type KindBetaInstallerWriter = { name: string; uid: string };
const namespace = "kars-system", name = "kars-provider-installer", storeName = "kars-inference-providers";
const grantResource = "karscredentialgrants.kars.azure.com";
const subject = `system:serviceaccount:${namespace}:${name}`;
const patchArgs = ["patch", "secret", storeName, "-n", namespace, "--type=json", "--patch-file=/dev/stdin"];
type ObjectMap = Record<string, any>;

function accountIdentity(account: ObjectMap, writer?: KindBetaInstallerWriter): KindBetaInstallerWriter {
  const identity = reviewed(account);
  if (!identity.uid.trim() || account.apiVersion !== "v1" || account.kind !== "ServiceAccount" || identity.name !== name
    || account.metadata.namespace !== namespace || account.automountServiceAccountToken !== false
    || (account.secrets?.length ?? 0) !== 0 || writer && (writer.name !== name || writer.uid !== identity.uid)) {
    throw new Error("Kind provider installer ServiceAccount identity changed or is unsafe");
  }
  return { name, uid: identity.uid };
}

/** Create, never adopt, an independent identity; the grant controller owns all writer RBAC. */
export async function createKindBetaInstallerWriter(execute: Execute): Promise<KindBetaInstallerWriter> {
  const account = JSON.parse(await execute(["create", "-f", "-", "-o", "json"], JSON.stringify({
    apiVersion: "v1", kind: "ServiceAccount", metadata: { name, namespace }, automountServiceAccountToken: false,
  })));
  return accountIdentity(account);
}

async function authority(execute: Execute, writer: KindBetaInstallerWriter) {
  accountIdentity(await read(execute, "serviceaccount", name, namespace), writer);
  const grant = await read(execute, grantResource, "workspace", namespace) as ObjectMap;
  const store = await read(execute, "secret", storeName, namespace) as ObjectMap;
  const workspace = await read(execute, "namespace", namespace) as ObjectMap;
  const writers = grant.spec?.writers?.filter((entry: ObjectMap) => entry.namespace === namespace && entry.name === name);
  const stores = grant.spec?.integrationStores?.filter((entry: ObjectMap) => entry.secret?.name === storeName);
  if (grant.metadata.name !== "workspace" || grant.metadata.namespace !== namespace || grant.spec?.enabled !== true
    || workspace.metadata.name !== namespace || grant.spec.workspaceUid !== workspace.metadata.uid
    || grant.status?.phase !== "Ready" || !Number.isInteger(grant.metadata.generation) || grant.metadata.generation < 1
    || grant.status.observedGeneration !== grant.metadata.generation || grant.status.integrationError
    || ["Ready", "WriterReady", "IntegrationReady", "PrivateConsumptionReady"].some(type =>
      !grant.status.conditions?.some((condition: ObjectMap) => condition.type === type && condition.status === "True"))
    || writers?.length !== 1 || writers[0].uid !== writer.uid
    || store.metadata.name !== storeName || store.metadata.namespace !== namespace || store.type !== "Opaque"
    || store.metadata.annotations?.["kars.azure.com/credential-store-grant-uid"] !== grant.metadata.uid
    || stores?.length !== 1 || stores[0].secret.uid !== store.metadata.uid || stores[0].purpose !== "providers"
    || Object.keys(store.data ?? {}).length || Object.keys(store.stringData ?? {}).length) {
    throw new Error("Kind provider installer authority changed; existing credentials were preserved");
  }
  return { grantUid: grant.metadata.uid as string, generation: grant.metadata.generation as number,
    spec: canonical(grant.spec), workspaceUid: workspace.metadata.uid as string,
    storeUid: store.metadata.uid as string, storeVersion: store.metadata.resourceVersion as string };
}

function publicCluster(server: string, certificate: string): { server: string; "certificate-authority-data": string } {
  try {
    const endpoint = new URL(server);
    if (server !== server.trim() || endpoint.protocol !== "https:" || endpoint.username || endpoint.password
      || endpoint.search || endpoint.hash || server.includes("?") || server.includes("#")
      || endpoint.pathname !== "/" || !["127.0.0.1", "localhost", "[::1]"].includes(endpoint.hostname)
      // URL parsing must not silently turn alternative IP encodings into a loopback address.
      || !/^https:\/\/(?:127\.0\.0\.1|localhost|\[::1\])(?::[0-9]+)?\/?$/.test(server)) throw new Error();
    if (!certificate || Buffer.from(certificate, "base64").toString("base64") !== certificate) throw new Error();
    const pem = Buffer.from(certificate, "base64").toString("utf8");
    if (!/^-----BEGIN CERTIFICATE-----\r?\n[A-Za-z0-9+/=\r\n]+\r?\n-----END CERTIFICATE-----\s*$/.test(pem)) throw new Error();
    const ca = new X509Certificate(pem);
    if (!ca.ca || Date.now() < Date.parse(ca.validFrom) || Date.now() >= Date.parse(ca.validTo)
      || !ca.checkIssued(ca) || !ca.verify(ca.publicKey)) throw new Error();
    return { server, "certificate-authority-data": certificate };
  } catch { throw new Error("Kind provider installer requires an HTTPS loopback API endpoint and a valid Kind X509 CA"); }
}

function tokenResponse(raw: string, writer: KindBetaInstallerWriter, storeUid: string): { token: string; expires: number } {
  try {
    const response = JSON.parse(raw), token = response.status?.token;
    const timestamp = response.status?.expirationTimestamp;
    if (response.apiVersion !== "authentication.k8s.io/v1" || response.kind !== "TokenRequest"
      || typeof token !== "string" || token.length > 32768 || !/^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/.test(token)
      || typeof timestamp !== "string" || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z$/.test(timestamp)) throw new Error();
    const [header, payload] = token.split(".").slice(0, 2).map(part => JSON.parse(Buffer.from(part, "base64url").toString("utf8")));
    const expires = Date.parse(timestamp), now = Date.now(), kubernetes = payload["kubernetes.io"];
    if (!Number.isFinite(expires) || expires <= now || expires > now + 600_000
      || !["RS256", "ES256", "ES384", "ES512"].includes(header.alg)
      || !Number.isInteger(payload.exp) || Math.abs(payload.exp * 1000 - expires) >= 1000
      || !Number.isInteger(payload.iat) || payload.iat * 1000 > now + 30_000 || payload.exp - payload.iat > 600
      || payload.exp <= payload.iat || payload.sub !== subject || kubernetes?.namespace !== namespace
      || kubernetes.serviceaccount?.name !== name || kubernetes.serviceaccount?.uid !== writer.uid
      || kubernetes.secret?.name !== storeName || kubernetes.secret?.uid !== storeUid) throw new Error();
    return { token, expires };
  } catch { throw new Error("Kind provider installer received an invalid or unbounded short-lived TokenRequest response"); }
}

function privateDirectory(directory: string): void {
  try { mkdirSync(directory, { mode: 0o700 }); }
  catch (error) { if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error; }
  const info = lstatSync(directory);
  if (!info.isDirectory() || info.isSymbolicLink() || (info.mode & 0o777) !== 0o700
    || !process.getuid || info.uid !== process.getuid()) throw new Error("Kind provider installer requires an owned private directory (mode 0700)");
}

function validatePatch(args: string[], input: string | undefined, current: Awaited<ReturnType<typeof authority>>): void {
  try {
    if (canonical(args) !== canonical(patchArgs) || typeof input !== "string") throw new Error();
    const patch = JSON.parse(input);
    if (!Array.isArray(patch) || patch.length !== 3 || canonical(patch.slice(0, 2)) !== canonical([
      { op: "test", path: "/metadata/uid", value: current.storeUid },
      { op: "test", path: "/metadata/resourceVersion", value: current.storeVersion },
    ]) || patch[2]?.op !== "add" || patch[2]?.path !== "/data" || Object.keys(patch[2]).sort().join() !== "op,path,value") throw new Error();
    const data = patch[2].value;
    if (!data || typeof data !== "object" || Array.isArray(data) || !Object.keys(data).length
      || Object.entries(data).some(([key, value]) => typeof value !== "string"
        || key !== "COPILOT_GITHUB_TOKEN" && !/^KARS_PROVIDER_[A-Z0-9_]+_(ENDPOINT|API_KEY|TOKEN|MODELS)$/.test(key)
        || Buffer.from(value as string, "base64").toString("base64") !== value)) throw new Error();
  } catch { throw new Error("Kind provider installer permits only one exact provider Secret JSON compare-and-swap patch via stdin"); }
}

/** Run the single bootstrap patch as the enrolled SA, never as the operator. */
export async function withKindBetaInstallerWriter<T>(execute: Execute, writer: KindBetaInstallerWriter,
  directory: string, tools: BetaTools, local: BetaExecute, work: (write: Execute) => Promise<T>): Promise<T> {
  if (writer.name !== name || typeof writer.uid !== "string" || !writer.uid.trim()) throw new Error("Invalid Kind provider installer identity");
  writer = { ...writer };
  accountIdentity(await read(execute, "serviceaccount", name, namespace), writer);
  const initial = await read(execute, grantResource, "workspace", namespace) as ObjectMap;
  await waitForWorkspaceProviders(execute, namespace);
  const enrolled = await authority(execute, writer);
  if (enrolled.grantUid !== initial.metadata.uid || enrolled.generation !== initial.metadata.generation
    || enrolled.spec !== canonical(initial.spec)) throw new Error("Kind provider installer grant changed during readiness verification");
  const cluster = publicCluster(
    await execute(["config", "view", "--minify", "--raw", "-o", "jsonpath={.clusters[0].cluster.server}"]),
    await execute(["config", "view", "--minify", "--raw", "-o", "jsonpath={.clusters[0].cluster.certificate-authority-data}"]),
  );
  privateDirectory(directory);
  const temporary = mkdtempSync(path.join(path.resolve(directory), "provider-writer-"));
  let active = true, used = false;
  try {
    let raw: string;
    try {
      raw = await execute(["create", "--raw", `/api/v1/namespaces/${namespace}/serviceaccounts/${name}/token`, "-f", "-"], JSON.stringify({
        apiVersion: "authentication.k8s.io/v1", kind: "TokenRequest", spec: { audiences: [], expirationSeconds: 600,
          boundObjectRef: { apiVersion: "v1", kind: "Secret", name: storeName, uid: enrolled.storeUid } },
      }));
    } catch { throw new Error("Kind provider installer TokenRequest failed; private command output is not displayed"); }
    const { token, expires } = tokenResponse(raw, writer, enrolled.storeUid);
    const kubeconfig = path.join(temporary, "kubeconfig.json"), context = `provider-writer-${randomUUID()}`;
    writeFileSync(kubeconfig, JSON.stringify({ apiVersion: "v1", kind: "Config", clusters: [{ name: context, cluster }],
      users: [{ name: context, user: { token } }], contexts: [{ name: context, context: { cluster: context, user: context, namespace } }],
      "current-context": context }), { mode: 0o600, flag: "wx" });
    const run: Execute = async (args, input) => {
      try {
        return (await local(tools.kubectl, ["--kubeconfig", kubeconfig, "--context", context, ...args],
          { env: tools.env, stdio: "pipe", ...(input === undefined ? {} : { input }) })).stdout;
      } catch { throw new Error("Kind provider installer authenticated command failed; private command output is not displayed"); }
    };
    let identity: ObjectMap;
    try { identity = JSON.parse(await run(["auth", "whoami", "-o", "json"])); }
    catch { throw new Error("Kind provider installer identity verification failed; private command output is not displayed"); }
    if (identity?.status?.userInfo?.username !== subject || identity?.status?.userInfo?.uid !== writer.uid) {
      throw new Error("Kind provider installer authenticated subject or ServiceAccount UID mismatch");
    }
    const revalidate = async () => {
      if (Date.now() >= expires) throw new Error("Kind provider installer token expired; no write was attempted");
      if (canonical(await authority(execute, writer)) !== canonical(enrolled)) {
        throw new Error("Kind provider installer authority changed; existing credentials were preserved");
      }
      if (!active) throw new Error("Kind provider installer writer is closed");
      if (Date.now() >= expires) throw new Error("Kind provider installer token expired; no write was attempted");
    };
    await revalidate();
    const write: Execute = async (args, input) => {
      if (!active || used) throw new Error("Kind provider installer writer is closed or already used");
      validatePatch(args, input, enrolled);
      used = true;
      await revalidate();
      return run(patchArgs, input);
    };
    return await work(write);
  } finally {
    active = false;
    rmSync(temporary, { recursive: true, force: true });
  }
}
