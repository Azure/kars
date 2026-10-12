// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { parseAllDocuments } from "yaml";
import type { Execute } from "../../lib/deployment-target.js";
import {
  assertMeshReleaseConsistency, helmMeshEnabled, inspectMeshInstallation,
  readReleaseValues, verifyMeshHealth, type MeshInstallation,
} from "../../lib/mesh-release.js";

export type MeshTrustMode = "anonymous" | "entra";
const activationHelp = "Enable Entra through the reviewed same-release Core Helm activation (entraSidecar.authConfig and agentMesh.enabled); see deploy/helm/kars/README.md. kars up does not provision or adopt an identity configuration.";

export function validateMeshTrustOptions(options: {
  meshTrust?: unknown; meshProvider?: unknown; globalRegistry?: unknown;
  exposeRegistry?: unknown; serviceTree?: unknown;
}): MeshTrustMode {
  const mode = options.meshTrust ?? "anonymous";
  if (mode !== "anonymous" && mode !== "entra") throw new Error("--mesh-trust must be 'anonymous' or 'entra'");
  if (options.meshProvider != null && options.meshProvider !== "agt") throw new Error("--mesh-provider must be 'agt'");
  if (options.globalRegistry != null) {
    if (typeof options.globalRegistry !== "string" || !options.globalRegistry.trim()) {
      throw new Error("--global-registry must be a nonempty URL");
    }
    const url = new URL(options.globalRegistry);
    if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.hash) {
      throw new Error("--global-registry must be an HTTP(S) URL without credentials or a fragment");
    }
  }
  if (options.exposeRegistry) {
    throw new Error("--expose-registry is temporarily unavailable: automatic publication cannot verify DNS/TLS/ingress ownership and must not overwrite credentials. Configure and verify ingress separately.");
  }
  if (options.serviceTree != null) throw new Error(`--service-tree is not supported by this activation path. ${activationHelp}`);
  if (mode === "entra" && options.globalRegistry) {
    throw new Error("--mesh-trust=entra cannot configure or attest an external registry; configure its trust explicitly.");
  }
  return mode;
}

function object(value: unknown, label: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be an object`);
  return value as Record<string, unknown>;
}

export function helmUsesEntra(values: Record<string, unknown>): boolean {
  if (values.entraSidecar == null) return false;
  const sidecar = object(values.entraSidecar, "entraSidecar");
  if (sidecar.enabled != null && typeof sidecar.enabled !== "boolean") throw new Error("entraSidecar.enabled must be boolean");
  if (sidecar.authConfig == null) return false;
  const config = object(sidecar.authConfig, "entraSidecar.authConfig");
  const backend = config.meshAuthBackend ?? "Anonymous";
  if (backend !== "Anonymous" && backend !== "EntraAgentIdentity") throw new Error("Unsupported meshAuthBackend");
  if (backend !== "EntraAgentIdentity") return false;
  if (sidecar.enabled !== true || !helmMeshEnabled(values)) throw new Error(activationHelp);
  return true;
}

export interface UpMeshTrust {
  releaseExists: boolean;
  mesh: MeshInstallation;
}

/** Read the selected cluster only; failed reads never become fresh-install permission. */
export async function inspectUpMeshTrust(execute: Execute, mode: MeshTrustMode, globalRegistry?: string): Promise<UpMeshTrust> {
  const { stdout } = await execute("helm", ["list", "--all", "--filter", "^kars$", "-n", "kars-system", "-o", "json"], { stdio: "pipe" });
  const releases: unknown = JSON.parse(String(stdout));
  if (!Array.isArray(releases)) throw new Error("Invalid Core Helm release inventory");
  if (releases.length > 1) throw new Error("Ambiguous Core Helm release inventory");
  if (releases.length) {
    const release = object(releases[0], "Core Helm release");
    if (release.name !== "kars" || typeof release.chart !== "string" || !release.chart.startsWith("kars-")
      || release.status !== "deployed") throw new Error("Core Helm release is not deployed; use explicit release recovery before kars up");
  }
  const values = releases.length ? await readReleaseValues(execute) : {};
  const mesh = await inspectMeshInstallation(execute);
  assertMeshReleaseConsistency(mesh, values);
  const entra = helmUsesEntra(values);
  if (mode === "entra" && (!entra || globalRegistry)) throw new Error(activationHelp);
  if (globalRegistry && mesh.kind === "helm") {
    throw new Error("A managed AgentMesh already belongs to this Core release; switching to an external registry requires an explicit reviewed migration.");
  }
  if (!globalRegistry && mesh.kind === "external") {
    throw new Error("AgentMesh has external ownership; configure --global-registry or explicitly migrate it. kars up will not adopt it.");
  }
  return { releaseExists: releases.length === 1, mesh };
}

/** Ordinary up creates a missing local mesh through Core Helm, never raw YAML. */
export function upMeshHelmArgs(trust: UpMeshTrust, globalRegistry?: string): string[] {
  return [
    ...(trust.releaseExists ? ["--reuse-values"] : []),
    "--set", `agentMesh.enabled=${!globalRegistry && (trust.mesh.kind === "helm" || trust.mesh.kind === "absent")}`,
  ];
}

const verifierNames = ["AGENTMESH_ENTRA_AUDIENCE", "AGENTMESH_ENTRA_TENANT_ID"] as const;
const guid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

function verifier(resource: Record<string, unknown>, containerName: string): string[] {
  const spec = object(resource.spec, "Deployment spec");
  const template = object(spec.template, "Deployment template");
  const pod = object(template.spec, "Pod spec");
  if (!Array.isArray(pod.containers)) throw new Error("Mesh containers are missing");
  const containers = pod.containers.map(value => object(value, "Mesh container"))
    .filter(container => container.name === containerName);
  if (containers.length !== 1) throw new Error("Mesh container is ambiguous");
  const container = containers[0]!;
  if (container.envFrom != null && (!Array.isArray(container.envFrom) || container.envFrom.length)) {
    throw new Error("Mesh verifier cannot be verified from indirect environment sources");
  }
  if (!Array.isArray(container.env)) throw new Error("Mesh verifier environment is missing");
  const env = container.env.map(value => object(value, "Environment entry"));
  if (env.some(entry => entry.name === "AGENTMESH_RELAY_ALLOW_UNAUTHED_DID")) {
    throw new Error("Mesh Entra verification cannot allow an unauthenticated DID override");
  }
  return verifierNames.map(name => {
    const entries = env.filter(entry => entry.name === name);
    if (entries.length !== 1 || entries[0]!.valueFrom != null
      || typeof entries[0]!.value !== "string" || !guid.test(entries[0]!.value)) {
      throw new Error("Mesh Entra verifier requires one literal audience/tenant GUID pair on each component");
    }
    return entries[0]!.value as string;
  });
}

/** Compare installed Helm declarations with both live templates; not token acceptance proof. */
export async function verifyHelmMeshTrust(execute: Execute, mesh: MeshInstallation, requireEntra: boolean): Promise<void> {
  if (mesh.kind !== "helm") {
    if (requireEntra) throw new Error(activationHelp);
    return;
  }
  const entra = helmUsesEntra(mesh.values);
  if (requireEntra && !entra) throw new Error(activationHelp);
  if (!entra) return;
  const { stdout } = await execute("helm", ["get", "manifest", mesh.release, "-n", mesh.releaseNamespace], { stdio: "pipe" });
  const documents = parseAllDocuments(String(stdout));
  if (documents.some(document => document.errors.length)) throw new Error("Invalid installed Helm manifest");
  const resources = documents.map(document => document.toJSON()).filter(value => value != null)
    .map(value => object(value, "Helm resource"));
  let commonPair: string | undefined;
  for (const deployment of mesh.deployments) {
    const matches = resources.filter(resource => resource.kind === "Deployment"
      && object(resource.metadata, "Deployment metadata").name === deployment.name
      && object(resource.metadata, "Deployment metadata").namespace === mesh.namespace);
    if (matches.length !== 1) throw new Error("Installed Helm mesh declaration is missing or ambiguous");
    const expected = JSON.stringify(verifier(matches[0]!, deployment.container));
    if (commonPair && commonPair !== expected) throw new Error("Mesh components disagree on Entra verifier configuration");
    commonPair = expected;
    const { stdout: live } = await execute("kubectl", ["get", "deployment", deployment.name, "-n", mesh.namespace, "-o", "json"], { stdio: "pipe" });
    if (JSON.stringify(verifier(object(JSON.parse(String(live)), "Live mesh Deployment"), deployment.container)) !== expected) {
      throw new Error("Live mesh verifier differs from the installed Helm declaration");
    }
  }
  if (mesh.deployments.length !== 2) throw new Error("Both mesh components are required for Entra verification");
  await verifyMeshHealth(execute, mesh);
}
