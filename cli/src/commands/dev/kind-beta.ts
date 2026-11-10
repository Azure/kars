// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createPrivateKey, randomBytes, X509Certificate } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { get as httpsGet } from "node:https";
import { createConnection, createServer, isIPv4 } from "node:net";
import * as path from "node:path";
import { execa } from "execa";
import { parse, parseAllDocuments, stringify } from "yaml";
import { requireBundledAsset, requirePackagedAsset } from "../../lib/repo-assets.js";
import { beginBetaState, betaDirectory, captureBetaNode, pinnedBetaTools, writeBetaState } from "./kind-beta-state.js";
import { kindBetaKubeletTrust } from "../../lib/kind-kubelet-trust.js";
export { kindBetaKubeletTrust } from "../../lib/kind-kubelet-trust.js";
import type { ensureCluster, installLocalCoreChart } from "./local-k8s.js";
import { prepareKindBetaInference } from "./kind-beta-inference.js";

type ObjectMap = Record<string, any>;
export type BetaExecute = (file: string, args: string[], options?: { input?: string; stdio?: "pipe" | "inherit"; env?: NodeJS.ProcessEnv }) => Promise<{ stdout: string }>;
export interface BetaTools { kind: string; kubectl: string; helm: string; runtime: string; env: NodeJS.ProcessEnv }
interface Hooks { ensureCluster: typeof ensureCluster; installLocalCoreChart: typeof installLocalCoreChart; openBrowser?: (url: string) => Promise<void>;
  afterCluster?: () => Promise<void>; beforeCore?: () => Promise<void> }
const object = (value: any, label: string): ObjectMap => {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be a YAML object`);
  return value;
};
function text(value: any, label: string): string {
  if (typeof value !== "string" || !value.trim()) throw new Error(`${label} is required`);
  return value;
}
function verifyBudgetTls(secret: ObjectMap, caBundle: unknown): void {
  const failure = "Budget TLS requires a valid server certificate for kars-inference-budget.kars-system.svc, its matching private key and a chain to inferenceBudget.caBundle";
  try {
    const certificates = (pem: unknown): X509Certificate[] => {
      if (typeof pem !== "string" || pem.length > 65536) throw new Error(failure);
      const blocks = pem.match(/-----BEGIN CERTIFICATE-----[\s\S]*?-----END CERTIFICATE-----/g) ?? [];
      if (!blocks.length || blocks.reduce((rest, cert) => rest.replace(cert, ""), pem).trim()) throw new Error(failure);
      return blocks.map(cert => new X509Certificate(cert));
    };
    const value = (key: string): string => secret.stringData?.[key] ?? Buffer.from(secret.data?.[key] ?? "", "base64").toString("utf8");
    const [leaf, ...intermediates] = certificates(value("tls.crt"));
    const roots = certificates(caBundle);
    const now = Date.now();
    const valid = (cert: X509Certificate) => now >= Date.parse(cert.validFrom) && now < Date.parse(cert.validTo);
    const trusted = (cert: X509Certificate, seen: Set<string>): boolean => {
      if (!valid(cert) || seen.has(cert.fingerprint256)) return false;
      if (roots.some(root => root.ca && root.fingerprint256 === cert.fingerprint256)) return true;
      const next = new Set([...seen, cert.fingerprint256]);
      return [...intermediates, ...roots].some(issuer => issuer.ca && cert.checkIssued(issuer)
        && cert.verify(issuer.publicKey) && trusted(issuer, next));
    };
    if (leaf.ca || leaf.checkHost("kars-inference-budget.kars-system.svc", { subject: "never" }) === undefined
      || (leaf.keyUsage && !leaf.keyUsage.includes("1.3.6.1.5.5.7.3.1"))
      || !leaf.checkPrivateKey(createPrivateKey(value("tls.key"))) || !trusted(leaf, new Set())) throw new Error(failure);
  } catch {
    throw new Error(failure);
  }
}
function origin(value: any, suffix: string): URL {
  const url = new URL(text(value, `OIDC ${suffix} URL`));
  if (url.protocol !== "http:" || !["localhost", "127.0.0.1"].includes(url.hostname)
    || url.pathname !== suffix || url.search || url.hash || url.username || url.password
    || !url.port || Number(url.port) < 1024) throw new Error(`Kind beta access requires a loopback HTTP URL on an explicit unprivileged port ending in ${suffix}`);
  return url;
}
function documents(file: string): ObjectMap[] {
  return parseAllDocuments(readFileSync(file, "utf8")).map(document => {
    if (document.errors.length) throw document.errors[0];
    return object(document.toJSON(), file);
  });
}

export function loadKindBetaPlan(file: string, cluster: string, asset = requireBundledAsset) {
  if (!/^[a-z0-9][a-z0-9-]{0,39}$/.test(cluster)) throw new Error("Use an explicit lowercase Kind cluster name (1–40 characters)");
  const configPath = path.resolve(file);
  const config = object(parse(readFileSync(configPath, "utf8")), "Kind beta config");
  const allowed = ["version", "nodeImage", "ciliumVersion", "coreValues", "bridgeValues", "resources", "oidc"];
  if (config.version !== 1 || Object.keys(config).some(key => !allowed.includes(key))) throw new Error("Unknown Kind beta config version or field");
  if (!/^kindest\/node(?::[^@\s]+)?@sha256:[a-f0-9]{64}$/.test(config.nodeImage ?? "")) throw new Error("nodeImage must identify a qualified Kubernetes 1.35 Kind node by digest");
  if (!/^\d+\.\d+\.\d+$/.test(config.ciliumVersion ?? "")) throw new Error("Supply a reviewed exact ciliumVersion");
  const resolve = (value: any) => path.resolve(path.dirname(configPath), text(value, "Input file"));
  const coreValues = resolve(config.coreValues), bridgeValues = resolve(config.bridgeValues);
  const core = object(parse(readFileSync(coreValues, "utf8")), "Core values");
  const bridge = object(parse(readFileSync(bridgeValues, "utf8")), "Bridge values");
  if (core.schemaHook?.enabled !== undefined && core.schemaHook.enabled !== true) {
    throw new Error("A fresh Kind beta install requires schemaHook.enabled=true; do not reuse upgrade-only schema staging settings");
  }
  for (const [section, field] of [["sre", "routerImageBaseline"], ["sre", "runtimeImageBaseline"], ["missionDispatcher", "imageBaseline"], ["metricsServer", "hostAliasesBaseline"]]) {
    if (core[section] && Object.hasOwn(core[section], field)) {
      throw new Error(`A fresh Kind beta install cannot use upgrade-only ${section}.${field}; omit the previous workspace's baseline without changing the selected settings`);
    }
  }
  for (const field of ["builtInFunding", "tokenTopUps", "unboundedTokenAllowances"]) {
    const entries = core.inferenceBudget?.[field];
    if (entries !== undefined && (!Array.isArray(entries) || entries.length > 0)) {
      throw new Error(`A fresh Kind beta install requires inferenceBudget.${field} to be omitted or empty; existing funding records belong to the previous cluster's identities`);
    }
  }
  if (bridge.workspaceEnrollment?.enabled === false
    || (bridge.workspaceEnrollment?.namespace && bridge.workspaceEnrollment.namespace !== "kars-system")) {
    throw new Error("A fresh Kind beta workspace requires credential-free enrollment in kars-system; do not reuse disabled enrollment from an existing operator-managed workspace");
  }
  const oidc = object(config.oidc, "Kubernetes OIDC claims");
  if (Object.keys(oidc).some(key => !["usernameClaim", "usernamePrefix", "groupsClaim", "groupsPrefix", "caFile"].includes(key))) throw new Error("Unknown Kubernetes OIDC field");
  for (const key of ["usernameClaim", "usernamePrefix", "groupsClaim", "groupsPrefix"]) text(oidc[key], `oidc.${key}`);
  if ([oidc.usernamePrefix, oidc.groupsPrefix].some(prefix => prefix === "-" || prefix.startsWith("system:"))) throw new Error("OIDC principals require explicit non-system prefixes");
  const issuer = text(core.headlamp?.oidc?.issuerURL, "Core Headlamp issuerURL");
  if (!/^https:\/\/[A-Za-z0-9][A-Za-z0-9.-]*(?::443)?(?:\/[^\s?#]*)?$/.test(issuer)
    || ["localhost", "127.0.0.1"].includes(new URL(issuer).hostname)) throw new Error("OIDC must be externally reachable over verified HTTPS on port 443 before Kind creation");
  const clientID = text(core.headlamp?.oidc?.clientID, "Core Headlamp clientID");
  const headlamp = origin(core.headlamp?.oidc?.callbackURL, "/oidc-callback");
  const env: ObjectMap = {};
  for (const entry of bridge.web?.extraEnv ?? []) {
    if (env[entry.name]) throw new Error("Duplicate Bridge environment setting");
    env[entry.name] = entry;
  }
  if (env.BRIDGE_OIDC_ISSUER?.value !== issuer) throw new Error("Bridge and Headlamp must use the same external issuer");
  const web = origin(env.BRIDGE_OIDC_REDIRECT_URI?.value, "/auth/callback");
  if (web.port === headlamp.port || bridge.headlamp?.url !== headlamp.origin) throw new Error("Bridge/Headlamp access URLs must match their callbacks and use different ports");
  if (!Array.isArray(config.resources) || !config.resources.length) throw new Error("Supply credential/TLS and reviewed OIDC RBAC resource files");
  const resources = config.resources.flatMap((entry: any) => documents(resolve(entry)));
  const identities = new Set<string>();
  for (const resource of resources) {
    const metadata = object(resource.metadata, "Resource metadata");
    const binding = ["ClusterRoleBinding", "RoleBinding"].includes(resource.kind);
    const role = resource.kind === "ClusterRole";
    if ((!binding && !role && !["Secret", "ConfigMap"].includes(resource.kind))
      || resource.apiVersion !== (binding || role ? "rbac.authorization.k8s.io/v1" : "v1")
      || (resource.kind === "ClusterRoleBinding" || role ? metadata.namespace !== undefined : metadata.namespace !== "kars-system")
      || metadata.ownerReferences || metadata.finalizers || metadata.deletionTimestamp
      || !/^[a-z0-9][a-z0-9.-]*$/.test(metadata.name ?? "")) throw new Error("Bootstrap files may contain only new kars-system Secrets/ConfigMaps and explicit OIDC RBAC bindings");
    const identity = `${resource.kind}/${metadata.name}`;
    if (identities.has(identity)) throw new Error(`Duplicate bootstrap resource ${identity}`);
    identities.add(identity);
    if (role && (resource.aggregationRule || !Array.isArray(resource.rules) || !resource.rules.length
      || resource.rules.some((rule: any) => !Array.isArray(rule.verbs) || !rule.verbs.length
        || rule.verbs.some((verb: string) => !["get", "list", "watch"].includes(verb))
        || !Array.isArray(rule.apiGroups) || !rule.apiGroups.length || rule.apiGroups.includes("*")
        || rule.nonResourceURLs || !Array.isArray(rule.resources) || !rule.resources.length
        || rule.resources.some((name: string) => ["*", "secrets"].includes(name) || name.includes("/"))))) {
      throw new Error("OIDC reader ClusterRoles must explicitly list read-only resources without Secrets, wildcards or subresources");
    }
    if (binding && (resource.roleRef?.apiGroup !== "rbac.authorization.k8s.io" || resource.roleRef.kind !== "ClusterRole"
      || !resource.roleRef.name || resource.roleRef.name === "cluster-admin" || resource.roleRef.name.startsWith("system:")
      || !Array.isArray(resource.subjects) || !resource.subjects.length || resource.subjects.some((subject: any) =>
        !["User", "Group"].includes(subject.kind) || subject.apiGroup !== "rbac.authorization.k8s.io"
        || !subject.name?.startsWith(subject.kind === "User" ? oidc.usernamePrefix : oidc.groupsPrefix)))) {
      throw new Error("Use reviewed, prefixed OIDC User/Group bindings, not anonymous, service-account or cluster-admin access");
    }
  }
  for (const binding of resources.filter(resource => resource.kind.endsWith("RoleBinding"))) {
    if (binding.roleRef.name !== "view" && !resources.some(resource => resource.kind === "ClusterRole" && resource.metadata.name === binding.roleRef.name)) {
      throw new Error("OIDC bindings must target the built-in view role or a supplied read-only ClusterRole");
    }
  }
  if (!resources.some(resource => resource.kind.endsWith("RoleBinding"))) throw new Error("Supply reviewed OIDC user/group RBAC");
  const secret = (name: any, key: any) => {
    text(name, "Referenced Secret name"); text(key, "Referenced Secret key");
    const resource = resources.find(resource => resource.kind === "Secret" && resource.metadata.name === name);
    if (!resource || !(resource.data?.[key] || resource.stringData?.[key])) throw new Error(`Bootstrap Secret ${name} must contain ${key}`);
    return resource;
  };
  const provider = core.inferenceRouter?.azure?.openai?.credentials;
  if (provider?.apiKey) throw new Error("Kind beta requires an inference credential Secret reference, not an inline key");
  if (provider?.existingSecret) secret(provider.existingSecret, provider.key ?? "api-key");
  const budgetTls = secret(core.inferenceBudget?.tlsSecretName, "tls.crt");
  secret(core.inferenceBudget?.tlsSecretName, "tls.key");
  if (budgetTls.type !== "kubernetes.io/tls"
    || budgetTls.metadata.annotations?.["kars.azure.com/inference-budget-tls"] !== "v1") {
    throw new Error(`Bootstrap Secret ${budgetTls.metadata.name} requires type kubernetes.io/tls and annotation kars.azure.com/inference-budget-tls: v1`);
  }
  verifyBudgetTls(budgetTls, core.inferenceBudget?.caBundle);
  secret(core.headlamp?.oidc?.clientSecret?.sourceName, core.headlamp?.oidc?.clientSecret?.key ?? "client-secret");
  const client = env.BRIDGE_OIDC_CLIENT_SECRET?.valueFrom?.secretKeyRef;
  secret(client?.name, client?.key);
  const principalName = text(bridge.auth?.principalSecretName, "Bridge auth.principalSecretName");
  const principalKey = bridge.auth?.principalSecretKey ?? "session-secret";
  if (resources.some(resource => resource.kind === "Secret" && resource.metadata.name === principalName)) secret(principalName, principalKey);
  else resources.push({ apiVersion: "v1", kind: "Secret", metadata: { name: principalName, namespace: "kars-system" }, type: "Opaque", stringData: { [principalKey]: randomBytes(48).toString("base64url") } });
  const caRefs = [core.headlamp?.oidc?.caConfigMap, core.headlamp?.oidc?.caSourceConfigMap, bridge.web?.oidc?.caConfigMap];
  if (caRefs.some(ref => ref && (!oidc.caFile || ref !== "kars-kind-oidc-ca"))) {
    throw new Error("Use oidc.caFile for shared API-server, Headlamp and Bridge trust; omit custom CA references or use kars-kind-oidc-ca");
  }
  let ca: string | undefined;
  if (oidc.caFile) {
    ca = readFileSync(resolve(oidc.caFile), "utf8");
    const certs = ca.match(/-----BEGIN CERTIFICATE-----[\s\S]*?-----END CERTIFICATE-----/g) ?? [];
    if (!certs.length || certs.reduce((rest, cert) => rest.replace(cert, ""), ca).trim()) throw new Error("OIDC caFile must contain public PEM certificates only");
    certs.forEach(cert => new X509Certificate(cert));
    if (identities.has("ConfigMap/kars-kind-oidc-ca")) throw new Error("kars-kind-oidc-ca is managed from oidc.caFile");
    resources.push({ apiVersion: "v1", kind: "ConfigMap", metadata: { name: "kars-kind-oidc-ca", namespace: "kars-system" }, data: { "ca.crt": ca } });
  }
  return { cluster, nodeImage: config.nodeImage as string, ciliumVersion: config.ciliumVersion as string, coreValues, bridgeValues,
    coreChart: asset("deploy/helm/kars"), bridgeChart: asset("bridge/deploy/helm/kars-bridge"),
    issuer, clientID, oidc, ca, resources, web, headlamp, core, inference: {} as ObjectMap };
}
export type KindBetaPlan = ReturnType<typeof loadKindBetaPlan> & { localDex?: boolean };

export function kindBetaClusterConfig(plan: KindBetaPlan, caPath?: string) {
  const args: ObjectMap = { "oidc-issuer-url": plan.issuer, "oidc-client-id": plan.clientID,
    "oidc-username-claim": plan.oidc.usernameClaim, "oidc-username-prefix": plan.oidc.usernamePrefix,
    "oidc-groups-claim": plan.oidc.groupsClaim, "oidc-groups-prefix": plan.oidc.groupsPrefix };
  if (caPath) args["oidc-ca-file"] = "/etc/kars/oidc-ca.crt";
  return { kind: "Cluster", apiVersion: "kind.x-k8s.io/v1alpha4", networking: { disableDefaultCNI: true },
    nodes: [{ role: "control-plane", ...(plan.localDex ? { extraPortMappings: [{ containerPort: 30443, hostPort: 443, listenAddress: "127.0.0.1", protocol: "TCP" }] } : {}), ...(caPath ? { extraMounts: [{ hostPath: caPath, containerPath: "/etc/kars/oidc-ca.crt", readOnly: true }] } : {}),
      // Kind patches its v1beta3 template before kubeadm performs any API conversion.
      kubeadmConfigPatches: [stringify({ apiVersion: "kubeadm.k8s.io/v1beta3", kind: "ClusterConfiguration", apiServer: { extraArgs: args,
        ...(caPath ? { extraVolumes: [{ name: "kars-oidc-ca", hostPath: "/etc/kars/oidc-ca.crt", mountPath: "/etc/kars/oidc-ca.crt", readOnly: true, pathType: "File" }] } : {}) } })] }] };
}

export async function verifyKindBetaIssuer(plan: KindBetaPlan): Promise<void> {
  const discovery = new URL(`${plan.issuer.replace(/\/$/, "")}/.well-known/openid-configuration`);
  await new Promise<void>((resolve, reject) => {
    const request = httpsGet(discovery, { ...(plan.ca ? { ca: plan.ca } : {}), timeout: 15000 }, response => {
      if (response.statusCode !== 200) { response.resume(); reject(new Error("OIDC discovery must return HTTPS 200 without redirects")); return; }
      let body = "";
      response.on("data", chunk => { body += chunk; if (Buffer.byteLength(body) > 1024 * 1024) request.destroy(new Error("OIDC discovery is oversized")); });
      response.on("error", reject);
      response.on("end", () => { try {
        const doc = JSON.parse(body);
        if (doc.issuer !== plan.issuer || new URL(doc.jwks_uri).protocol !== "https:") throw new Error("OIDC discovery issuer/JWKS mismatch");
        resolve();
      } catch (error) { reject(error); } });
    });
    request.on("timeout", () => request.destroy(new Error("OIDC discovery timed out")));
    request.on("error", reject);
  });
}

export function kindBetaApiDestinations(service: ObjectMap, slices: ObjectMap) {
  const destinations = new Map<string, { cidr: string; port: number }>();
  const add = (ip: string, port: number, protocol: string) => {
    if (!isIPv4(ip) || ip === "0.0.0.0" || !Number.isInteger(port) || port < 1 || port > 65535 || protocol !== "TCP") throw new Error("Kind API discovery requires exact IPv4/TCP destinations");
    destinations.set(`${ip}:${port}`, { cidr: `${ip}/32`, port });
  };
  if (service.metadata?.name !== "kubernetes" || service.metadata?.namespace !== "default" || !service.spec?.ports?.length) throw new Error("Missing Kubernetes API Service");
  for (const port of service.spec.ports) add(service.spec.clusterIP, port.port, port.protocol);
  if (!Array.isArray(slices.items) || !slices.items.length) throw new Error("Missing Kubernetes API EndpointSlices");
  for (const slice of slices.items) {
    if (slice.addressType !== "IPv4" || slice.metadata?.namespace !== "default" || slice.metadata?.labels?.["kubernetes.io/service-name"] !== "kubernetes" || !slice.ports?.length || !slice.endpoints?.length) throw new Error("Invalid Kubernetes API EndpointSlice");
    for (const endpoint of slice.endpoints) {
      if (endpoint.conditions?.ready === false || !endpoint.addresses?.length) throw new Error("Kubernetes API endpoint is not ready");
      for (const ip of endpoint.addresses) for (const port of slice.ports) add(ip, port.port, port.protocol);
    }
  }
  if (destinations.size > 8) throw new Error("Too many Kubernetes API destinations for the maintained schema hook");
  return [...destinations.values()];
}

export async function installKindBeta(plan: KindBetaPlan, tools: BetaTools, hooks: Hooks, execute: BetaExecute, directory: string) {
  const ignore = path.join(directory, ".gitignore");
  if (!existsSync(ignore)) writeFileSync(ignore, "*\n", { mode: 0o600, flag: "wx" });
  else if (readFileSync(ignore, "utf8") !== "*\n") throw new Error("Private beta attempt has an unexpected ignore policy");
  const context = `kind-${plan.cluster}`, kubeconfig = path.join(directory, "kubeconfig");
  const kube = (args: string[], input?: string) => execute(tools.kubectl, ["--kubeconfig", kubeconfig, "--context", context, ...args], { env: tools.env, stdio: "pipe", ...(input ? { input } : {}) });
  const helm = (args: string[]) => execute(tools.helm, ["--kubeconfig", kubeconfig, "--kube-context", context, ...args], { env: tools.env, stdio: "pipe" });
  const version = await execute(tools.helm, ["version", "--short"], { env: tools.env, stdio: "pipe" });
  if (!/^v4\.1\.3(?:\+[^\s]+)?\s*$/.test(version.stdout)) throw new Error("Kind beta's maintained first-install hooks require Helm 4.1.3");
  // Render the maintained namespace instead of inventing another namespace policy.
  const namespace = await execute(tools.helm, ["template", "kars", plan.coreChart, "--namespace", "kars-system", "--kube-version", "1.35.0", "--show-only", "templates/namespace.yaml"], { env: tools.env, stdio: "pipe" });
  const namespaceObject = parse(namespace.stdout);
  if (namespaceObject?.kind !== "Namespace" || namespaceObject.metadata?.name !== "kars-system") throw new Error("Core namespace template is invalid");
  const caPath = plan.ca ? path.join(directory, "oidc-ca.crt") : undefined;
  if (caPath) writeFileSync(caPath, plan.ca!, { mode: 0o600, flag: "wx" });
  const config = path.join(directory, "kind.yaml");
  writeFileSync(config, stringify(kindBetaClusterConfig(plan, caPath)), { mode: 0o600, flag: "wx" });
  await hooks.ensureCluster(tools.kind, plan.cluster, tools.env, tools.runtime, { config, kubeconfig, image: plan.nodeImage,
    ...(hooks.afterCluster ? { onCreated: hooks.afterCluster } : {}) });
  const server = JSON.parse((await kube(["version", "-o", "json"])).stdout);
  if (!(server.serverVersion?.gitVersion ?? "").startsWith("v1.35.")) throw new Error("Kind beta requires Kubernetes 1.35; retained the mismatched cluster for inspection");
  const apiServers = JSON.parse((await kube(["-n", "kube-system", "get", "pods", "-l", "component=kube-apiserver", "-o", "json"])).stdout);
  const expectedApiServer = parse(kindBetaClusterConfig(plan, caPath).nodes[0].kubeadmConfigPatches[0]).apiServer;
  const apiServerPod = apiServers.items?.length === 1 ? apiServers.items[0] : undefined;
  const apiServer = apiServerPod?.spec?.containers?.find((container: ObjectMap) => container.name === "kube-apiserver");
  const argumentsList = [...(apiServer?.command ?? []), ...(apiServer?.args ?? [])];
  const argumentsMatch = Object.entries(expectedApiServer.extraArgs).every(([name, value]) =>
    argumentsList.filter((argument: string) => argument.startsWith(`--${name}=`)).join("") === `--${name}=${value}`);
  const caMatches = !caPath || (apiServer?.volumeMounts?.some((mount: ObjectMap) =>
    mount.name === "kars-oidc-ca" && mount.mountPath === "/etc/kars/oidc-ca.crt" && mount.readOnly === true)
    && apiServerPod?.spec?.volumes?.some((volume: ObjectMap) =>
      volume.name === "kars-oidc-ca" && volume.hostPath?.path === "/etc/kars/oidc-ca.crt" && volume.hostPath?.type === "File"));
  if (!apiServer || !argumentsMatch || !caMatches) throw new Error("Kind API server did not apply the reviewed OIDC arguments and CA mount; retained the cluster for inspection before installing workloads");
  await helm(["install", "cilium", "cilium", "--repo", "https://helm.cilium.io/", "--version", plan.ciliumVersion,
    "--namespace", "kube-system", "--set", "ipam.mode=kubernetes", "--set", "operator.replicas=1", "--set", "kubeProxyReplacement=false", "--wait", "--wait-for-jobs", "--timeout", "10m"]);
  await kube(["-n", "kube-system", "rollout", "status", "daemonset/cilium", "--timeout=300s"]);
  await kube(["wait", "--for=condition=Established", "crd/ciliumnetworkpolicies.cilium.io", "--timeout=300s"]);
  await kube(["wait", "--for=condition=Ready", "nodes", "--all", "--timeout=300s"]);
  await kube(["label", "node", `${plan.cluster}-control-plane`, "kars.azure.com/pool=sandbox"]);
  const apiService = JSON.parse((await kube(["-n", "default", "get", "service", "kubernetes", "-o", "json"])).stdout);
  const apiEndpoints = JSON.parse((await kube(["-n", "default", "get", "endpointslices", "-l", "kubernetes.io/service-name=kubernetes", "-o", "json"])).stdout);
  const apiDestinations = kindBetaApiDestinations(apiService, apiEndpoints);
  const nodeName = `${plan.cluster}-control-plane`;
  const kindNodes = (await execute(tools.kind, ["get", "nodes", "--name", plan.cluster], { env: tools.env, stdio: "pipe" })).stdout.trim().split(/\s+/);
  if (kindNodes.length !== 1 || kindNodes[0] !== nodeName) throw new Error("Kind beta requires its single owned control-plane node");
  const node = JSON.parse((await kube(["get", "node", nodeName, "-o", "json"])).stdout);
  const containers = JSON.parse((await execute(tools.runtime, ["inspect", nodeName], { env: tools.env, stdio: "pipe" })).stdout);
  if (!Array.isArray(containers) || containers.length !== 1) throw new Error("Missing Kind control-plane container");
  // Read only public certificates from the node we just created; never disable kubelet TLS verification.
  const chain = (await execute(tools.runtime, ["exec", nodeName, "cat", "/var/lib/kubelet/pki/kubelet.crt"], { env: tools.env, stdio: "pipe" })).stdout;
  const metricsServer = kindBetaKubeletTrust(plan.cluster, node, containers[0], chain);
  await hooks.beforeCore?.();
  await kube(["create", "--validate=strict", "-f", "-"], namespace.stdout);
  // CREATE only: no adoption, overwrite, rollback or credential logging.
  for (const resource of plan.resources) await kube(["create", "--validate=strict", "-f", "-"], JSON.stringify(resource));
  const coreOverlay = path.join(directory, "core-kind.yaml"), bridgeOverlay = path.join(directory, "bridge-kind.yaml");
  writeFileSync(coreOverlay, stringify({ ...plan.inference, global: { installationProfile: "kind-beta" }, kindBeta: { cni: "cilium" }, cilium: { enabled: true },
    schemaHook: { apiDestinations }, metricsServer,
    datapathWitness: { nodes: [`${plan.cluster}-control-plane`] }, sandbox: { nodeSelector: { "kubernetes.io/hostname": `${plan.cluster}-control-plane` } },
    ...(plan.ca ? { headlamp: { oidc: { caConfigMap: "kars-kind-oidc-ca", caSourceConfigMap: "kars-kind-oidc-ca" } } } : {}) }), { mode: 0o600, flag: "wx" });
  writeFileSync(bridgeOverlay, stringify({ global: { installationProfile: "kind-beta" }, networkPolicy: { routerApiServerIngress: true },
    ...(plan.ca ? { web: { oidc: { caConfigMap: "kars-kind-oidc-ca" } } } : {}) }), { mode: 0o600, flag: "wx" });
  await hooks.installLocalCoreChart(tools.helm, tools.kubectl, plan.cluster, "kars", plan.coreChart,
    [path.join(plan.coreChart, "values-generic.yaml"), path.join(plan.coreChart, "values-kind-beta.yaml"), plan.coreValues, coreOverlay], [], undefined, { kubeconfig });
  // The enrollment hook can finish before its controller-created SRE consumer converges.
  const registration = JSON.parse((await kube(["get", "karssreregistration", "canonical", "-o", "json"])).stdout);
  const sreName = text(registration.spec?.sandbox?.name, "Core SRE sandbox name");
  const sreNamespace = text(registration.spec?.runtimeNamespace?.name, "Core SRE runtime namespace");
  await kube(["-n", sreNamespace, "wait", "--for=create", `deployment/${sreName}`, "--timeout=15m"]);
  await kube(["-n", sreNamespace, "rollout", "status", `deployment/${sreName}`, "--timeout=15m"]);
  await kube(["wait", "--for=condition=Ready", "karssreregistration/canonical", "--timeout=15m"]);
  await helm(["install", "kars-bridge", plan.bridgeChart, "--namespace", "kars-system", "-f", path.join(plan.bridgeChart, "values-kind-beta.yaml"),
    "-f", plan.bridgeValues, "-f", bridgeOverlay, "--wait", "--wait-for-jobs", "--timeout", "15m"]);
  const { setupInstalledKindBetaProviders } = await import("./kind-beta-providers.js");
  await setupInstalledKindBetaProviders(plan.cluster, kubeconfig, path.join(directory, "provider-setup"),
    plan.coreChart, tools, execute, hooks.installLocalCoreChart);
  return { kubeconfig, context };
}

async function assertPortAvailable(port: number) {
  if (port === 443) {
    // The runtime publishes this privileged port, not the unprivileged CLI.
    await new Promise<void>((resolve, reject) => {
      const socket = createConnection({ host: "127.0.0.1", port });
      socket.once("connect", () => { socket.destroy(); reject(new Error("Local HTTPS port 443 is already in use")); });
      socket.once("error", (error: NodeJS.ErrnoException) => { socket.destroy(); error.code === "ECONNREFUSED" ? resolve() : reject(error); });
      socket.setTimeout(3000, () => { socket.destroy(); reject(new Error("Cannot verify local HTTPS port 443 availability")); });
    });
    return;
  }
  await new Promise<void>((resolve, reject) => {
    const server = createServer(); server.once("error", reject);
    server.listen(port, "127.0.0.1", () => server.close(error => error ? reject(error) : resolve()));
  });
}

async function probeKindBetaUI(originURL: URL) {
  // Probe the forwarded socket, not localhost's possibly IPv6 DNS resolution.
  const target = new URL(originURL.origin); target.hostname = "127.0.0.1";
  const response = await fetch(target, { redirect: "manual", signal: AbortSignal.timeout(15000) });
  await response.body?.cancel();
  if (response.status < 200 || response.status >= 400) throw new Error(`Kind beta UI is not responsive (HTTP ${response.status})`);
}

export async function kindBetaAccess(plan: KindBetaPlan, tools: BetaTools, installed: { kubeconfig: string; context: string },
  dependencies: { spawn: typeof execa; probe: (url: URL) => Promise<void>; open?: (url: string) => Promise<void> } = { spawn: execa, probe: probeKindBetaUI }) {
  const children = new Set<ReturnType<typeof execa>>();
  const targets = [
    { namespace: "kars-system", service: "kars-bridge-web", url: plan.web, remote: "3000" },
    { namespace: "headlamp", service: "headlamp", url: plan.headlamp, remote: "80" },
  ];
  let stopped = false;
  let stop!: () => void;
  const interrupt = new Promise<void>(resolve => { stop = () => { stopped = true; resolve(); }; });
  const transient = (detail: string, recovering = false) => !/forbidden|unauthorized|address already in use/i.test(detail)
    && (/connection reset by peer|lost connection to pod|unexpected EOF/i.test(detail)
      || (recovering && /connection refused|Current status=Pending|timed out opening|HTTP 50[234]|fetch failed/i.test(detail)));
  const close = async (child: ReturnType<typeof execa>) => {
    child.kill("SIGTERM");
    await Promise.allSettled([child]);
    children.delete(child);
  };
  const openTunnel = async (target: typeof targets[number], timeout = 30000) => {
    const child = dependencies.spawn(tools.kubectl, ["--kubeconfig", installed.kubeconfig, "--context", installed.context, "--request-timeout=0",
      "-n", target.namespace, "port-forward", "--address", "127.0.0.1", `service/${target.service}`, `${target.url.port}:${target.remote}`],
    { env: tools.env, stdout: "pipe", stderr: "pipe", reject: false });
    children.add(child);
    let stderr = "", stdout = "";
    const onStderr = (chunk: Buffer) => { stderr = (stderr + chunk).slice(-4096); };
    child.stderr?.on("data", onStderr);
    const ended = child.then(
      () => stderr || "Kind beta UI tunnel exited; cluster retained",
      error => stderr || String(error),
    ).finally(() => child.stderr?.removeListener("data", onStderr));
    let markReady!: () => void;
    const ready = new Promise<void>(resolve => { markReady = resolve; });
    const onStdout = (chunk: Buffer) => {
      stdout = (stdout + chunk).slice(-4096);
      if (stdout.includes(`Forwarding from 127.0.0.1:${target.url.port}`)) markReady();
    };
    child.stdout!.on("data", onStdout);
    let timer!: ReturnType<typeof setTimeout>;
    try {
      await Promise.race([ready, interrupt,
        ended.then(detail => { throw new Error(`${target.service} tunnel exited before readiness: ${detail}`); }),
        new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error(`Timed out opening ${target.service} tunnel`)), timeout); }),
      ]);
      return { child, ended };
    } catch (error) {
      await close(child);
      throw error;
    } finally {
      clearTimeout(timer);
      child.stdout!.removeListener("data", onStdout);
    }
  };
  // Keep the handler registered during signal dispatch so execa does not re-raise it.
  process.on("SIGINT", stop); process.on("SIGTERM", stop);
  try {
    const tunnels: Awaited<ReturnType<typeof openTunnel>>[] = [];
    for (const target of targets) {
      tunnels.push(await openTunnel(target));
      if (stopped) return;
    }
    await Promise.race([Promise.all(targets.map(target => dependencies.probe(target.url))), interrupt]);
    if (stopped) return;
    console.log(`Bridge: ${plan.web.origin}\nHeadlamp: ${plan.headlamp.origin}\nAuthenticate with your OIDC account. Ctrl+C closes these tunnels; the cluster and failure evidence are retained.`);
    await Promise.race([dependencies.open?.(plan.web.origin), interrupt]);
    while (!stopped) {
      const failure = await Promise.race([interrupt, ...tunnels.map((tunnel, index) => tunnel.ended.then(detail => ({ index, detail })))]);
      if (!failure || stopped) return;
      if (!transient(failure.detail)) throw new Error(failure.detail);
      await close(tunnels[failure.index]!.child);
      const target = targets[failure.index]!;
      const deadline = Date.now() + 180000;
      let detail = failure.detail;
      while (!stopped) {
        if (Date.now() >= deadline) throw new Error(`${target.service} tunnel recovery timed out; cluster retained: ${detail}`);
        console.log(`${target.service} tunnel disconnected; reconnecting on the same port in 1s: ${detail}`);
        let timer!: ReturnType<typeof setTimeout>;
        try { await Promise.race([interrupt, new Promise<void>(resolve => { timer = setTimeout(resolve, 1000); })]); }
        finally { clearTimeout(timer); }
        if (stopped) return;
        let replacement: Awaited<ReturnType<typeof openTunnel>> | undefined;
        try {
          replacement = await openTunnel(target, Math.max(1, Math.min(30000, deadline - Date.now())));
          if (stopped) return;
          await Promise.race([dependencies.probe(target.url), interrupt]);
          if (stopped) return;
          tunnels[failure.index] = replacement;
          console.log(`${target.service} tunnel reconnected: ${target.url.origin}`);
          break;
        } catch (error) {
          if (replacement) await close(replacement.child);
          detail = error instanceof Error ? error.message : String(error);
          if (!transient(detail, true)) throw error;
        }
      }
    }
  } finally {
    for (const child of children) child.kill("SIGTERM");
    await Promise.allSettled(children);
    process.removeListener("SIGINT", stop); process.removeListener("SIGTERM", stop);
  }
}

export async function assertFreshKindBetaCluster(cluster: string, tools: BetaTools, execute: BetaExecute): Promise<void> {
  let names: string[];
  try {
    const { stdout } = await execute(tools.kind, ["get", "clusters"], { env: tools.env, stdio: "pipe" });
    names = stdout.split(/\r?\n/).map(name => name.trim()).filter(Boolean);
    if (names.some(name => !/^[a-z0-9][a-z0-9-]*$/.test(name))) throw new Error("Invalid cluster inventory");
  } catch {
    throw new Error("Kind cluster discovery failed; absence was not established. No inference settings or cluster resources were changed");
  }
  if (names.includes(cluster)) {
    throw new Error(`Kind beta requires a fresh cluster; ${cluster} already exists. To add Bridge to existing Core, install only the separate Bridge Helm release; do not rerun Core installation`);
  }
}

export async function runKindBeta(file: string | boolean, cluster: string, tools: BetaTools, hooks: Hooks) {
  if (file === true) return runManagedKindBeta(cluster, tools, hooks);
  if (typeof file !== "string") throw new Error("Provide a beta config filename or use bare --beta-config interactively");
  const plan = loadKindBetaPlan(file, cluster, requirePackagedAsset);
  const directory = path.resolve(`.kars-kind-beta-${cluster}`);
  if (existsSync(directory)) throw new Error(`Retained attempt exists at ${directory}; inspect it rather than replaying its identities`);
  await assertFreshKindBetaCluster(cluster, tools, async (tool, args, options) => {
    const result = await execa(tool, args, options); return { stdout: result.stdout ?? "" };
  });
  await Promise.all([assertPortAvailable(Number(plan.web.port)), assertPortAvailable(Number(plan.headlamp.port)), verifyKindBetaIssuer(plan)]);
  plan.inference = await prepareKindBetaInference(plan.core, plan.resources);
  mkdirSync(directory, { mode: 0o700 });
  const installed = await installKindBeta(plan, tools, hooks, async (tool, args, options) => {
    try { const result = await execa(tool, args, options); return { stdout: result.stdout ?? "" }; }
    catch { throw new Error(`${path.basename(tool)} ${args.includes("create") ? "bootstrap CREATE" : "Kind beta operation"} failed; inspect the retained cluster and Helm state. Credentials are not echoed.`); }
  }, directory);
  await kindBetaAccess(plan, tools, installed, { spawn: execa, probe: probeKindBetaUI, open: hooks.openBrowser });
}

async function runManagedKindBeta(cluster: string, tools: BetaTools, hooks: Hooks) {
  const { prepareLocalBeta, promptLocalBetaAccount, installLocalDex } = await import("./kind-beta-local.js");
  // Resolve both packaged charts before asking for credentials or changing the host.
  requirePackagedAsset("deploy/helm/kars"); requirePackagedAsset("bridge/deploy/helm/kars-bridge");
  const directory = betaDirectory(cluster);
  if (existsSync(directory)) throw new Error(`Retained attempt exists at ${directory}; no identities will be replayed`);
  const execute: BetaExecute = async (tool, args, options) => {
    try { const result = await execa(tool, args, options); return { stdout: result.stdout ?? "" }; }
    catch { throw new Error(`${path.basename(tool)} local beta operation failed; inspect retained private state. Credentials are not echoed.`); }
  };
  const state = await beginBetaState(cluster, tools, execute);
  tools = pinnedBetaTools(tools, state);
  await assertFreshKindBetaCluster(cluster, tools, execute);
  await Promise.all([443, 3000, 4466].map(assertPortAvailable));
  const account = await promptLocalBetaAccount(`${cluster}-${state.owner.slice(0, 8)}.kars.test`);
  mkdirSync(directory, { mode: 0o700 });
  writeFileSync(path.join(directory, ".gitignore"), "*\n", { mode: 0o600, flag: "wx" });
  writeBetaState(directory, state);
  try {
    const plan = await prepareLocalBeta(directory, state, account, execute);
    account.password = "";
    plan.inference = await prepareKindBetaInference(plan.core, plan.resources, undefined, true);
    writeFileSync(path.join(directory, "installed-resources.yaml"), plan.resources.map(resource => stringify(resource)).join("---\n"), { mode: 0o600, flag: "wx" });
    state.phase = "installing"; writeBetaState(directory, state);
    const installed = await installKindBeta(plan, tools, { ...hooks,
      afterCluster: async () => { await captureBetaNode(state, tools, execute); writeBetaState(directory, state); },
      beforeCore: () => installLocalDex(plan, directory, state, tools, execute),
    }, execute, directory);
    state.phase = "ready"; writeBetaState(directory, state);
    console.log(`Owned local beta: ${cluster}. To delete this cluster and its local trust: kars dev down --cluster-name ${cluster}. Private receipts are retained at ${directory}.`);
    await kindBetaAccess(plan, tools, installed, { spawn: execa, probe: probeKindBetaUI, open: hooks.openBrowser });
  } catch (error) {
    const { readBetaState } = await import("./kind-beta-state.js");
    if (readBetaState(directory)?.phase !== "deleted") {
      state.phase = "failed";
      state.failure = { at: new Date().toISOString(), message: error instanceof Error ? error.message : "Setup failed" };
      writeBetaState(directory, state);
    }
    throw new Error(`Local beta setup failed; private failure and ownership receipts retained in ${directory}`);
  } finally { account.password = ""; }
}
