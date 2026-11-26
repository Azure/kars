// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { randomBytes, randomUUID, X509Certificate } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { isIPv4 } from "node:net";
import path from "node:path";
import inquirer from "inquirer";
import { hash } from "bcryptjs";
import { parse, parseAllDocuments, stringify } from "yaml";
import { requirePackagedAsset } from "../../lib/repo-assets.js";
import { issueKindBudgetIdentity, issueKindTlsIdentity } from "./kind-beta-providers.js";
import { loadKindBetaPlan, verifyKindBetaIssuer, verifyKindBetaSchemaArchitecture, type BetaExecute, type BetaTools, type KindBetaPlan } from "./kind-beta.js";
import { pinnedBetaTools, writeBetaState, type KindBetaState } from "./kind-beta-state.js";
import { assertManagedHostSupport, changeManagedHostTrust } from "./kind-beta-host.js";

export const localDexNamespace = "kars-local-idp";
export const localKindNodeImage = "kindest/node:v1.35.0@sha256:452d707d4862f52530247495d180205e029056831160e22870e37e3f6c1ac31f";
export const localCiliumVersion = "1.19.10";
export interface LocalBetaAccount { email: string; password: string }
export interface LocalBetaPorts { https: number; bridge: number; headlamp: number }
export function localBetaPorts(value: unknown = undefined, includeBridge = true): LocalBetaPorts {
  if (value === undefined) return { https: 8443, bridge: 3000, headlamp: 4466 };
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Local ports must be an object");
  const ports = value as Record<string, unknown>;
  const keys = ["https", "bridge", "headlamp"] as const;
  if (Object.keys(ports).some(key => !keys.includes(key as typeof keys[number]))
    || keys.some(key => typeof ports[key] !== "number" || !Number.isInteger(ports[key]) || ports[key] < 1024 || ports[key] > 65535)) {
    throw new Error("Choose whole-number local ports from 1024 to 65535; privileged ports are not supported");
  }
  const result = { https: Number(ports.https), bridge: Number(ports.bridge), headlamp: Number(ports.headlamp) };
  const active = includeBridge ? Object.values(result) : [result.https, result.headlamp];
  if (new Set(active).size !== active.length) throw new Error("Identity, Bridge and Headlamp must use different local ports");
  return result;
}
export function validateLocalBetaAccount(account: LocalBetaAccount): void {
  if (!/^[a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$/.test(account.email) || account.email.length > 254
    || typeof account.password !== "string" || account.password.length < 12 || Buffer.byteLength(account.password) > 72) {
    throw new Error("Use an email and a password of at least 12 characters and at most 72 UTF-8 bytes (bcrypt's limit)");
  }
}
export async function promptLocalBetaAccount(hostname: string, httpsPort = 8443): Promise<LocalBetaAccount> {
  assertManagedHostSupport();
  if (!process.stdin.isTTY || !process.stdout.isTTY) throw new Error("Bare --beta-config requires interactive setup; use an explicit external-OIDC config for unattended installation");
  console.log(`A fresh packaged Dex will authenticate a local beta account. HTTPS uses ${hostname}:${httpsPort}, reachable only through this fresh Kind node's loopback mapping. Setup needs sudo to add one marked /etc/hosts entry, and explicit permission to trust a newly generated, hostname-constrained CA (one year). macOS uses your login keychain; Linux uses the system CA store. The CA signing key is destroyed after issuance. Browsers with independent trust stores may still require a manual trust decision. dev down removes only the recorded entry/certificate. No existing cluster or default context is changed.`);
  const consent = await inquirer.prompt<{ consent: boolean }>([{ type: "confirm", name: "consent", default: false, message: "Allow these owned hostname and certificate-trust changes for this local beta?" }]);
  if (!consent.consent) throw new Error("Local trust declined; no cluster or host settings changed. Explicit-file external OIDC remains available.");
  const account = await inquirer.prompt<LocalBetaAccount>([
    { type: "input", name: "email", message: "Local beta account email", validate: (email: string) => { try { validateLocalBetaAccount({ email, password: "validation-only" }); return true; } catch { return "Enter a valid email"; } } },
    { type: "password", name: "password", message: "New local beta password (12+ characters; not your provider key)", mask: "*",
      validate: (password: string) => password.length >= 12 && Buffer.byteLength(password) <= 72 || "Use 12+ characters and no more than 72 UTF-8 bytes" },
  ]);
  const confirmation = await inquirer.prompt<{ password: string }>([{ type: "password", name: "password", message: "Confirm local beta password", mask: "*" }]);
  if (confirmation.password !== account.password) throw new Error("Local beta passwords differ; no cluster or host settings changed");
  validateLocalBetaAccount(account); return account;
}

export function assertLocalBetaAssets(asset = requirePackagedAsset): void {
  const bridgeChart = asset("bridge/deploy/helm/kars-bridge"), coreChart = asset("deploy/helm/kars");
  const bridgeDefaults = parse(readFileSync(path.join(bridgeChart, "values.yaml"), "utf8"));
  const coreDefaults = parse(readFileSync(path.join(coreChart, "values.yaml"), "utf8"));
  if (!/^[^\s@]+@sha256:[a-f0-9]{64}$/.test(bridgeDefaults.idp?.dex?.image ?? "")
    || !/^latest@sha256:[a-f0-9]{64}$/.test(coreDefaults.inferenceRouter?.image?.tag ?? "")) {
    throw new Error("Managed beta requires the released package's digest-bound Dex and Core charts; source/default images are not usable release assets");
  }
}

export async function localBetaArchitecture(tools: BetaTools, execute: BetaExecute, asset = requirePackagedAsset): Promise<"amd64" | "arm64"> {
  const architecture = (await execute(tools.runtime, ["info", "--format", "{{.Host.Arch}}"], { env: tools.env, stdio: "pipe" })).stdout.trim();
  return verifyKindBetaSchemaArchitecture(architecture, asset("deploy/helm/kars"), {}, tools, execute);
}

export async function prepareLocalBeta(directory: string, state: KindBetaState, account: LocalBetaAccount, execute: BetaExecute,
  asset = requirePackagedAsset, includeBridge = true, selectedPorts?: LocalBetaPorts): Promise<KindBetaPlan> {
  const ports = localBetaPorts(selectedPorts, includeBridge);
  validateLocalBetaAccount(account);
  assertLocalBetaAssets(asset);
  const hostname = `${state.cluster}-${state.owner.slice(0, 8)}.kars.test`;
  const issuer = `https://${hostname}:${ports.https}`;
  const webOrigin = `http://localhost:${ports.bridge}`, headlampOrigin = `http://localhost:${ports.headlamp}`;
  const budget = await issueKindBudgetIdentity(path.join(directory, "budget-issuer"), execute);
  const tls = await issueKindTlsIdentity(path.join(directory, "oidc-issuer"), execute, hostname);
  const save = (name: string, content: string) => { const file = path.join(directory, name); writeFileSync(file, content, { mode: 0o600, flag: "wx" }); return file; };
  const caFile = save("oidc-ca.pem", tls.ca);
  state.trust = { hostname, fingerprint: new X509Certificate(tls.ca).fingerprint256, certificate: caFile, installed: false };
  writeBetaState(directory, state);
  const secret = (name: string, values: Record<string, string>, namespace = "kars-system") => ({ apiVersion: "v1", kind: "Secret", metadata: { name, namespace }, type: "Opaque", stringData: values });
  const bridgeSecret = includeBridge ? randomBytes(48).toString("base64url") : "", headlampSecret = randomBytes(48).toString("base64url");
  const core = {
    datapathWitness: { gadget: { hookMode: "fanotify+ebpf" } },
    inferenceBudget: { contracts: [], catalogVersion: `local-${state.owner}`, caBundle: budget.ca, tlsSecretName: "kars-kind-budget-tls" },
    headlamp: { oidc: { issuerURL: issuer, clientID: "kars-headlamp", callbackURL: `${headlampOrigin}/oidc-callback`,
      localDexNamespace, clientSecret: { name: "kars-headlamp-client", sourceName: "kars-headlamp-client", key: "client-secret" } } },
  };
  const bridge = { headlamp: { url: headlampOrigin }, auth: { principalSecretName: "kars-bridge-principal", principalSecretKey: "session-secret" },
    web: { oidc: { localDexNamespace }, extraEnv: [
      { name: "BRIDGE_OIDC_ISSUER", value: issuer }, { name: "BRIDGE_OIDC_CLIENT_ID", value: "kars-bridge" },
      { name: "BRIDGE_OIDC_REDIRECT_URI", value: `${webOrigin}/auth/callback` },
      { name: "BRIDGE_OIDC_SCOPES", value: "openid profile email offline_access" },
      { name: "BRIDGE_OIDC_ROLE_CLAIM", value: "email" }, { name: "BRIDGE_OIDC_ROLE_MAP", value: JSON.stringify({ [account.email]: "admin" }) },
      { name: "BRIDGE_OIDC_CLIENT_SECRET", valueFrom: { secretKeyRef: { name: "kars-bridge-client", key: "client-secret" } } },
    ] } };
  const budgetSecret = { ...secret("kars-kind-budget-tls", { "tls.crt": budget.certificate, "tls.key": budget.privateKey }), type: "kubernetes.io/tls",
    metadata: { name: "kars-kind-budget-tls", namespace: "kars-system", annotations: { "kars.azure.com/inference-budget-tls": "v1" } } };
  const resources = [budgetSecret, secret("kars-headlamp-client", { "client-secret": headlampSecret }),
    ...(includeBridge ? [secret("kars-bridge-client", { "client-secret": bridgeSecret })] : []),
    { apiVersion: "rbac.authorization.k8s.io/v1", kind: "ClusterRoleBinding", metadata: { name: "kars-kind-local-reader" },
      roleRef: { apiGroup: "rbac.authorization.k8s.io", kind: "ClusterRole", name: "view" },
      subjects: [{ apiGroup: "rbac.authorization.k8s.io", kind: "User", name: `oidc:${account.email}` }] },
    { apiVersion: "rbac.authorization.k8s.io/v1", kind: "ClusterRole", metadata: { name: "kars-kind-dashboard-reader" }, rules: [
      { apiGroups: ["kars.azure.com"], resources: ["karssandboxes", "inferencepolicies", "karsmemories", "mcpservers", "a2aagents", "toolpolicies", "trustgraphs", "karspairings", "karsevals", "egressapprovals", "karssreactions"], verbs: ["get", "list", "watch"] },
      { apiGroups: [""], resources: ["nodes", "namespaces"], verbs: ["get", "list", "watch"] },
      { apiGroups: ["metrics.k8s.io"], resources: ["pods", "nodes"], verbs: ["get", "list", "watch"] },
    ] },
    { apiVersion: "rbac.authorization.k8s.io/v1", kind: "ClusterRoleBinding", metadata: { name: "kars-kind-dashboard-reader" },
      roleRef: { apiGroup: "rbac.authorization.k8s.io", kind: "ClusterRole", name: "kars-kind-dashboard-reader" },
      subjects: [{ apiGroup: "rbac.authorization.k8s.io", kind: "User", name: `oidc:${account.email}` }] }];
  const dexValues = { namespace: localDexNamespace, idp: { enabled: true, issuer,
    clientId: includeBridge ? "kars-bridge" : "kars-headlamp", secretName: includeBridge ? "kars-bridge-client" : "kars-headlamp-client",
    redirectURIs: [includeBridge ? `${webOrigin}/auth/callback` : `${headlampOrigin}/oidc-callback`],
    headlampClient: { enabled: includeBridge, clientID: "kars-headlamp", callbackURL: `${headlampOrigin}/oidc-callback`, clientSecret: { name: "kars-headlamp-client", key: "client-secret" } },
    dex: { kindPrerequisite: true, enablePasswordDB: true, connectors: [], tls: { enabled: true, secretName: "kars-kind-idp-tls" },
      persistence: { enabled: true, existingClaim: "dex-data" },
      staticPasswords: [{ email: account.email, username: account.email, userID: randomUUID(), hash: await hash(account.password, 12) }] } },
    networkPolicy: { create: true, dexApiServerIngress: true } };
  save("dex-values.yaml", stringify(dexValues));
  save("dex-secrets.yaml", [...(includeBridge ? [secret("kars-bridge-client", { "client-secret": bridgeSecret }, localDexNamespace)] : []),
    secret("kars-headlamp-client", { "client-secret": headlampSecret }, localDexNamespace),
    { ...secret("kars-kind-idp-tls", { "tls.crt": tls.certificate, "tls.key": tls.privateKey }, localDexNamespace), type: "kubernetes.io/tls" }].map(value => stringify(value)).join("---\n"));
  const config = { version: 1, nodeImage: localKindNodeImage, ciliumVersion: localCiliumVersion,
    coreValues: save("core-input.yaml", stringify(core)),
    ...(includeBridge ? { bridgeValues: save("bridge-input.yaml", stringify(bridge)) } : { bridge: false }),
    resources: [save("resources.yaml", resources.map(value => stringify(value)).join("---\n"))],
    oidc: { usernameClaim: "email", usernamePrefix: "oidc:", groupsClaim: "groups", groupsPrefix: "oidc:", caFile } };
  const plan: KindBetaPlan = loadKindBetaPlan(save("beta.yaml", stringify(config)), state.cluster, asset, { hostname, port: ports.https });
  plan.localDex = true; return plan;
}

export async function discoverLocalDexGateways(state: KindBetaState, tools: BetaTools, execute: BetaExecute):
  Promise<{ kindGatewayCIDR: string; kindHostGatewayCIDR?: string }> {
  if (!state.nodeId) throw new Error("Local Dex gateway discovery requires the owned Kind node");
  tools = pinnedBetaTools(tools, state);
  const containers = JSON.parse((await execute(tools.runtime, ["inspect", state.nodeId], { env: tools.env })).stdout);
  const container = Array.isArray(containers) && containers.length === 1 ? containers[0] : undefined;
  const gateway = container?.NetworkSettings?.Networks?.kind?.Gateway;
  if (container?.Id !== state.nodeId || !isIPv4(gateway) || gateway === "0.0.0.0") throw new Error("Cannot discover the owned Kind gateway for local browser ingress");
  const kindGatewayCIDR = `${gateway}/32`;
  if (path.basename(state.runtime) !== "podman" || !state.endpoint.startsWith("ssh://")) return { kindGatewayCIDR };
  const endpoint = new URL(state.endpoint);
  if (!["127.0.0.1", "localhost", "[::1]"].includes(endpoint.hostname) || !state.sshKey) {
    throw new Error("Local Dex requires a local Podman machine with a pinned SSH identity");
  }
  // Host forwarding crosses the VM gateway before the container network gateway.
  // Bind by endpoint and key, never by the mutable default machine/connection.
  const machines: { Name: string; Port: number; IdentityPath: string; RemoteUsername: string; Running: boolean }[] =
    JSON.parse((await execute(tools.runtime, ["machine", "list", "--format", "json"], { env: tools.env })).stdout);
  const selected = Array.isArray(machines) ? machines.filter(machine => machine
    && machine.Port === Number(endpoint.port || "22") && machine.IdentityPath === state.sshKey
    && (machine.RemoteUsername === decodeURIComponent(endpoint.username) || endpoint.username === "root")) : [];
  if (selected.length !== 1 || selected[0].Running !== true || !/^[a-zA-Z0-9][a-zA-Z0-9_.-]*$/.test(selected[0].Name)) {
    throw new Error("Cannot bind the selected Podman endpoint to one running local machine; no browser ingress was widened");
  }
  const routes: { dst: string; gateway: string }[] = JSON.parse((await execute(tools.runtime,
    ["machine", "ssh", selected[0].Name, "ip", "-j", "-4", "route", "show", "default"], { env: tools.env })).stdout);
  const defaults = Array.isArray(routes) ? routes.filter(route => route?.dst === "default") : [];
  if (defaults.length !== 1 || !isIPv4(defaults[0].gateway) || defaults[0].gateway === "0.0.0.0") {
    throw new Error("Cannot discover one exact IPv4 forwarding gateway on the selected Podman machine; no browser ingress was widened");
  }
  return { kindGatewayCIDR, kindHostGatewayCIDR: `${defaults[0].gateway}/32` };
}

export async function installLocalDex(plan: KindBetaPlan, directory: string, state: KindBetaState, tools: BetaTools, execute: BetaExecute,
  operations = { trust: changeManagedHostTrust, verify: verifyKindBetaIssuer }): Promise<void> {
  if (!state.nodeId || !plan.localDex || !state.trust) throw new Error("Local Dex requires a verified owned Kind node");
  const nodeId = state.nodeId;
  const gateways = await discoverLocalDexGateways(state, tools, execute);
  const kube = async (args: string[], input?: unknown) => execute(tools.kubectl, ["--kubeconfig", path.join(directory, "kubeconfig"), "--context", `kind-${state.cluster}`, ...args],
    { env: tools.env, stdio: "pipe", ...(input !== undefined ? { input: typeof input === "string" ? input : JSON.stringify(input) } : {}) });
  const create = (value: unknown) => kube(["create", "--validate=strict", "-f", "-"], value);
  await create({ apiVersion: "v1", kind: "Namespace", metadata: { name: localDexNamespace, labels: { "pod-security.kubernetes.io/enforce": "restricted", "kars.azure.com/local-beta-owner": state.owner } } });
  const storage = `/var/local/kars-dex-${state.owner}`;
  await execute(tools.runtime, ["exec", nodeId, "install", "-d", "-m", "0700", "-o", "1001", "-g", "1001", storage], { env: tools.env });
  await create({ apiVersion: "v1", kind: "PersistentVolume", metadata: { name: "kars-local-dex" }, spec: { capacity: { storage: "1Gi" }, accessModes: ["ReadWriteOnce"],
    persistentVolumeReclaimPolicy: "Retain", storageClassName: "", local: { path: storage }, claimRef: { name: "dex-data", namespace: localDexNamespace },
    nodeAffinity: { required: { nodeSelectorTerms: [{ matchExpressions: [{ key: "kubernetes.io/hostname", operator: "In", values: [`${state.cluster}-control-plane`] }] }] } } } });
  await create({ apiVersion: "v1", kind: "PersistentVolumeClaim", metadata: { name: "dex-data", namespace: localDexNamespace }, spec: {
    accessModes: ["ReadWriteOnce"], storageClassName: "", volumeName: "kars-local-dex", resources: { requests: { storage: "1Gi" } } } });
  for (const document of parseAllDocuments(readFileSync(path.join(directory, "dex-secrets.yaml"), "utf8"))) await create(document.toJSON());
  const networkValues = path.join(directory, "dex-network.yaml");
  writeFileSync(networkValues, stringify({ idp: { dex: gateways } }), { mode: 0o600, flag: "wx" });
  // Render only the existing packaged Dex component. Full Bridge keeps its external-OIDC/bundled-Dex rejection.
  const rendered = await execute(tools.helm, ["template", "kars-local-idp", plan.bridgeChart, "--namespace", localDexNamespace,
    "--api-versions", "cilium.io/v2/CiliumNetworkPolicy", "--show-only", "templates/idp.yaml", "-f", path.join(directory, "dex-values.yaml"), "-f", networkValues], { env: tools.env });
  await create(rendered.stdout);
  await kube(["-n", localDexNamespace, "wait", "--for=condition=Valid=True", "ciliumnetworkpolicy/kars-local-idp-dex-apiserver", "--timeout=60s"]);
  await kube(["-n", localDexNamespace, "rollout", "status", "deployment/dex", "--timeout=300s"]);
  const service = JSON.parse((await kube(["-n", localDexNamespace, "get", "service", "dex", "-o", "json"])).stdout);
  const ip = service.spec?.clusterIP;
  if (!isIPv4(ip)) throw new Error("Dex Service has no allocated IPv4 address");
  const hostname = state.trust.hostname;
  const config = JSON.parse((await kube(["-n", "kube-system", "get", "configmap", "coredns", "-o", "json"])).stdout);
  if (!config.data?.Corefile || config.data.Corefile.includes(hostname)) throw new Error("Unexpected fresh CoreDNS configuration");
  config.data.Corefile += `\n${hostname}:53 {\n  hosts {\n    ${ip} ${hostname}\n  }\n}\n`;
  await kube(["replace", "-f", "-"], config);
  await kube(["-n", "kube-system", "rollout", "restart", "deployment/coredns"]);
  await kube(["-n", "kube-system", "rollout", "status", "deployment/coredns", "--timeout=180s"]);
  // Static host-network pods do not use cluster DNS. Give only the owned API server this Service mapping.
  const manifestPath = "/etc/kubernetes/manifests/kube-apiserver.yaml";
  const manifest = parse((await execute(tools.runtime, ["exec", nodeId, "cat", manifestPath], { env: tools.env })).stdout);
  if (manifest.kind !== "Pod" || manifest.metadata?.name !== "kube-apiserver" || !manifest.spec?.hostNetwork || manifest.spec.hostAliases) throw new Error("Unexpected owned API-server static pod manifest");
  manifest.spec.hostAliases = [{ ip, hostnames: [hostname] }];
  const stagedManifest = `/etc/kubernetes/kars-oidc-${state.owner}.yaml`;
  await execute(tools.runtime, ["exec", "-i", nodeId, "tee", stagedManifest], { env: tools.env, input: stringify(manifest), stdio: "pipe" });
  await execute(tools.runtime, ["exec", nodeId, "mv", stagedManifest, manifestPath], { env: tools.env });
  let ready = false;
  for (let attempt = 0; attempt < 60; attempt++) {
    try {
      const pods = JSON.parse((await kube(["-n", "kube-system", "get", "pods", "-l", "component=kube-apiserver", "-o", "json", "--request-timeout=3s"])).stdout);
      const pod = pods.items?.length === 1 ? pods.items[0] : undefined;
      if (!pod?.spec?.hostAliases?.some((alias: any) => alias.ip === ip && alias.hostnames?.includes(hostname))
        || !pod.status?.conditions?.some((condition: any) => condition.type === "Ready" && condition.status === "True")) throw new Error("Waiting for the updated API-server pod");
      await kube(["get", "--raw", "/readyz", "--request-timeout=3s"]);
      ready = true; break;
    }
    catch { await new Promise(resolve => setTimeout(resolve, 2000)); }
  }
  if (!ready) throw new Error("Owned API server did not recover after local issuer routing; retained cluster");
  state.trust.installed = true; writeBetaState(directory, state);
  await operations.trust(state, true, execute);
  await operations.verify(plan);
}
