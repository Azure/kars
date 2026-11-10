// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { randomUUID } from "node:crypto";
import { existsSync, lstatSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import type { BetaExecute, BetaTools } from "./kind-beta.js";

export interface KindBetaState {
  version: 1; owner: string; cluster: string; phase: "preparing" | "installing" | "ready" | "failed" | "deleted";
  runtime: string; endpoint: string; sshKey?: string; nodeId?: string; setupPid?: number;
  failure?: { at: string; message: string };
  trust?: { hostname: string; fingerprint: string; certificate: string; installed: boolean };
}
export function betaDirectory(cluster: string, root = process.cwd()): string {
  if (!/^[a-z0-9][a-z0-9-]{0,39}$/.test(cluster)) throw new Error("Invalid managed Kind cluster name");
  return path.resolve(root, `.kars-kind-beta-${cluster}`);
}
export function writeBetaState(directory: string, state: KindBetaState): void {
  const info = lstatSync(directory);
  if (!info.isDirectory() || info.isSymbolicLink() || (info.mode & 0o077) || (process.getuid && info.uid !== process.getuid())) {
    throw new Error("Kind lifecycle state requires its owned private directory");
  }
  const next = path.join(directory, `state-${randomUUID()}.json`);
  writeFileSync(next, JSON.stringify(state, null, 2) + "\n", { mode: 0o600, flag: "wx" });
  renameSync(next, path.join(directory, "managed-local.json"));
}
export function readBetaState(directory: string): KindBetaState | undefined {
  const file = path.join(directory, "managed-local.json");
  if (!existsSync(file)) return undefined;
  for (const name of [directory, file]) {
    const info = lstatSync(name);
    if (info.isSymbolicLink() || (info.mode & 0o077) || (process.getuid && info.uid !== process.getuid())) throw new Error("Foreign Kind lifecycle state");
  }
  const state = JSON.parse(readFileSync(file, "utf8"));
  if (state.version !== 1 || !/^[0-9a-f-]{36}$/.test(state.owner ?? "") || betaDirectory(state.cluster, path.dirname(directory)) !== directory
    || !["preparing", "installing", "ready", "failed", "deleted"].includes(state.phase)
    || !["docker", "podman"].includes(path.basename(state.runtime ?? ""))
    || !/^(unix|ssh):\/\//.test(state.endpoint ?? "")
    || (state.setupPid !== undefined && (!Number.isSafeInteger(state.setupPid) || state.setupPid < 1))
    || (state.nodeId && !/^[a-f0-9]{64}$/.test(state.nodeId))) throw new Error("Invalid managed Kind ownership receipt");
  if (state.trust && (state.trust.hostname !== `${state.cluster}-${state.owner.slice(0, 8)}.kars.test`
    || state.trust.certificate !== path.join(directory, "oidc-ca.pem") || typeof state.trust.installed !== "boolean"
    || !/^([0-9A-F]{2}:){31}[0-9A-F]{2}$/.test(state.trust.fingerprint ?? ""))) throw new Error("Invalid owned local trust receipt");
  return state;
}
export function pinnedBetaTools(tools: BetaTools, state: KindBetaState): BetaTools {
  const env: NodeJS.ProcessEnv = { ...tools.env, DOCKER_CONTEXT: "", DOCKER_HOST: "", CONTAINER_CONNECTION: "", CONTAINER_HOST: "", CONTAINER_SSHKEY: "" };
  if (path.basename(state.runtime) === "docker") {
    env.DOCKER_HOST = state.endpoint;
    env.KIND_EXPERIMENTAL_PROVIDER = "docker";
  } else {
    env.CONTAINER_HOST = state.endpoint;
    env.CONTAINER_SSHKEY = state.sshKey ?? "";
    env.KIND_EXPERIMENTAL_PROVIDER = "podman";
  }
  return { ...tools, runtime: state.runtime, env };
}
export async function beginBetaState(cluster: string, tools: BetaTools, execute: BetaExecute): Promise<KindBetaState> {
  const runtime = path.basename(tools.runtime);
  let endpoint: string, sshKey: string | undefined;
  if (runtime === "docker") {
    const context = tools.env.DOCKER_CONTEXT || "";
    if (!context && tools.env.DOCKER_HOST) endpoint = tools.env.DOCKER_HOST;
    else {
      const contexts = JSON.parse((await execute(tools.runtime, ["context", "inspect", ...(context ? [context] : [])], { env: tools.env })).stdout);
      endpoint = contexts[0]?.Endpoints?.docker?.Host;
    }
    if (!endpoint?.startsWith("unix://")) throw new Error("Managed local beta requires a local Docker-compatible Unix socket");
  } else if (runtime === "podman") {
    if (tools.env.CONTAINER_HOST) { endpoint = tools.env.CONTAINER_HOST; sshKey = tools.env.CONTAINER_SSHKEY; }
    else {
      const connections = JSON.parse((await execute(tools.runtime, ["system", "connection", "list", "--format", "json"], { env: tools.env })).stdout);
      const selected = connections.filter((item: any) => tools.env.CONTAINER_CONNECTION ? item.Name === tools.env.CONTAINER_CONNECTION : item.Default);
      if (selected.length !== 1) throw new Error("Select an explicit Podman connection before managed local setup");
      endpoint = selected[0].URI; sshKey = selected[0].Identity;
    }
    if (!/^(unix|ssh):\/\//.test(endpoint ?? "")) throw new Error("Unsupported Podman local endpoint");
    if (endpoint.startsWith("ssh://") && !["127.0.0.1", "localhost", "[::1]"].includes(new URL(endpoint).hostname)) {
      throw new Error("Managed beta requires a local Podman endpoint; a remote host cannot serve the owned loopback issuer");
    }
  } else throw new Error("Managed local beta currently supports Docker-compatible or Podman runtimes");
  return { version: 1, owner: randomUUID(), cluster, phase: "preparing", setupPid: process.pid, runtime: tools.runtime, endpoint, ...(sshKey ? { sshKey } : {}) };
}
export async function captureBetaNode(state: KindBetaState, tools: BetaTools, execute: BetaExecute): Promise<void> {
  const nodes = (await execute(tools.kind, ["get", "nodes", "--name", state.cluster], { env: tools.env })).stdout.trim().split(/\s+/);
  if (nodes.length !== 1 || nodes[0] !== `${state.cluster}-control-plane`) throw new Error("Managed beta requires one newly created control-plane node");
  const [node] = JSON.parse((await execute(tools.runtime, ["inspect", nodes[0]], { env: tools.env })).stdout);
  if (!/^[a-f0-9]{64}$/.test(node?.Id ?? "") || node.Config?.Labels?.["io.x-k8s.kind.cluster"] !== state.cluster) throw new Error("Cannot bind Kind node ownership");
  if (state.nodeId && state.nodeId !== node.Id) throw new Error("Recorded Kind node identity changed");
  if (!state.trust || !node.Mounts?.some((mount: any) => mount.Source === path.join(path.dirname(state.trust!.certificate), "oidc-ca.crt")
    && mount.Destination === "/etc/kars/oidc-ca.crt" && mount.RW === false)) throw new Error("Kind node does not contain this attempt's unique CA mount");
  state.nodeId = node.Id;
}
export async function teardownManagedBeta(directory: string, tools: BetaTools, execute: BetaExecute,
  removeTrust: (state: KindBetaState) => Promise<void>): Promise<void> {
  const state = readBetaState(directory);
  if (!state) throw new Error("No managed-local ownership receipt; external OIDC resources are not teardown targets");
  if (["preparing", "installing"].includes(state.phase) && state.setupPid) {
    let active = true;
    try { process.kill(state.setupPid, 0); } catch (error) { if ((error as NodeJS.ErrnoException).code === "ESRCH") active = false; else throw error; }
    if (active) throw new Error("Local setup is still running; stop the owning setup process before teardown");
  }
  tools = pinnedBetaTools(tools, state);
  if (state.phase !== "deleted") {
    if (!state.nodeId) throw new Error("Failed bootstrap has no verified node identity; retained for explicit inspection, not name-based deletion");
    const present = (await execute(tools.runtime, ["ps", "--all", "--no-trunc", "--filter", `id=${state.nodeId}`, "--format", "{{.ID}}"], { env: tools.env })).stdout.trim();
    if (present) {
      if (present !== state.nodeId) throw new Error("Unexpected runtime inventory; nothing was deleted");
      const containers = JSON.parse((await execute(tools.runtime, ["inspect", state.nodeId], { env: tools.env })).stdout);
      const node = containers.length === 1 ? containers[0] : undefined;
      if (node?.Id !== state.nodeId || node.Config?.Labels?.["io.x-k8s.kind.cluster"] !== state.cluster) {
        throw new Error("Kind node ownership changed; nothing was deleted");
      }
      // Delete by immutable container ID, never by a reusable name, current context or port.
      await execute(tools.runtime, ["rm", "--force", "--volumes", state.nodeId], { env: tools.env });
    }
    state.phase = "deleted"; writeBetaState(directory, state);
  }
  if (state.trust?.installed) { await removeTrust(state); state.trust.installed = false; writeBetaState(directory, state); }
}
