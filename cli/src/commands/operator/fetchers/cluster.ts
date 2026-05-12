// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Cluster-wide health fetchers (mesh infrastructure + cluster nodes).
 *
 * Extracted from `cli/src/commands/operator.ts` (S15.e.3) so the
 * top-level dashboard module can stay under §4.2's 800-line cap.
 *
 * Both fetchers branch on `devMode` to choose between Docker-based
 * inspection (local dev clusters) and `kubectl` queries (AKS).
 *
 * No behavioral change vs. the originals — bodies are byte-identical
 * apart from mechanical edits: closure-captured `kubeContext` and
 * `devMode` are now explicit parameters.
 */

import { execa } from "execa";
import type { ClusterHealth, MeshHealth } from "../types.js";
import { kctl, timeSince } from "../helpers.js";

export async function fetchMeshHealth(devMode: boolean, kubeContext?: string): Promise<MeshHealth> {
  const result: MeshHealth = { relayReady: false, registryReady: false, registryPods: 0, registryReadyPods: 0 };
  if (devMode) {
    try {
      const { stdout } = await execa("docker", ["ps", "--filter", "name=relay", "--filter", "status=running", "--format", "{{.Names}}"], { stdio: "pipe", timeout: 5000 });
      result.relayReady = stdout.trim().length > 0;
    } catch {}
    try {
      const { stdout } = await execa("docker", ["ps", "--filter", "name=registry", "--filter", "status=running", "--format", "{{.Names}}"], { stdio: "pipe", timeout: 5000 });
      result.registryReady = stdout.trim().length > 0;
      result.registryPods = result.registryReady ? 1 : 0;
      result.registryReadyPods = result.registryPods;
    } catch {}
    return result;
  }
  try {
    const { stdout } = await execa("kubectl", kctl([
      "get", "pods", "-n", "agentmesh", "-o",
      `jsonpath={range .items[*]}{.metadata.labels.app}{"||"}{.status.conditions[?(@.type=="Ready")].status}{"\\n"}{end}`,
    ], kubeContext), { stdio: "pipe", timeout: 10000 });
    for (const line of stdout.trim().split("\n")) {
      if (!line) continue;
      const [app, ready] = line.split("||");
      if (app?.includes("relay") && ready === "True") result.relayReady = true;
      if (app?.includes("registry")) {
        result.registryPods++;
        if (ready === "True") { result.registryReady = true; result.registryReadyPods++; }
      }
    }
  } catch {}
  return result;
}

export async function fetchClusterHealth(devMode: boolean, kubeContext?: string): Promise<ClusterHealth> {
  const result: ClusterHealth = {
    apiLatencyMs: -1, apiReachable: false,
    nodes: [], quotas: [], pvcs: [], warnings: [],
  };

  if (devMode) {
    result.apiReachable = true;
    result.apiLatencyMs = 0;
    try {
      const { stdout } = await execa("docker", ["info", "--format", "{{.NCPU}}|{{.MemTotal}}|{{.ServerVersion}}"], { stdio: "pipe" });
      const [cpus, mem, version] = stdout.trim().split("|");
      const memGb = (parseInt(mem || "0", 10) / 1073741824).toFixed(1);
      result.nodes = [{
        name: "docker-host",
        pool: "local",
        status: "Ready",
        version: version || "",
        cpuCores: `${cpus}`,
        cpuPct: "-",
        memBytes: `${memGb}Gi`,
        memPct: "-",
        os: "Docker",
        runtime: `docker://${version || ""}`,
      }];
    } catch { /* docker info fail */ }
    return result;
  }

  // All queries in parallel
  const [apiRes, nodesRes, topRes, quotaRes, pvcRes, eventsRes] = await Promise.allSettled([
    // 0: API server latency
    (async () => {
      const start = Date.now();
      await execa("kubectl", kctl(["get", "ns", "--no-headers"], kubeContext),
        { stdio: "pipe", timeout: 10000 });
      return Date.now() - start;
    })(),
    // 1: Node info
    execa("kubectl", kctl([
      "get", "nodes", "-o",
      `jsonpath={range .items[*]}{.metadata.name}|{.metadata.labels.agentpool}|` +
      `{.status.conditions[?(@.type=="Ready")].status}|{.status.nodeInfo.kubeletVersion}|` +
      `{.status.nodeInfo.osImage}|{.status.nodeInfo.containerRuntimeVersion}{"\\n"}{end}`,
    ], kubeContext), { stdio: "pipe", timeout: 10000 }),
    // 2: kubectl top nodes
    execa("kubectl", kctl(["top", "nodes", "--no-headers"], kubeContext),
      { stdio: "pipe", timeout: 10000 }),
    // 3: Resource quotas
    execa("kubectl", kctl([
      "get", "resourcequotas", "-A", "-o",
      `jsonpath={range .items[*]}{.metadata.namespace}|{.status.used.cpu}|{.status.hard.cpu}|` +
      `{.status.used.memory}|{.status.hard.memory}{"\\n"}{end}`,
    ], kubeContext), { stdio: "pipe", timeout: 10000 }),
    // 4: PVCs
    execa("kubectl", kctl([
      "get", "pvc", "-A", "-o",
      `jsonpath={range .items[*]}{.metadata.namespace}|{.metadata.name}|{.status.phase}|` +
      `{.spec.resources.requests.storage}{"\\n"}{end}`,
    ], kubeContext), { stdio: "pipe", timeout: 10000 }),
    // 5: Warning events
    execa("kubectl", kctl([
      "get", "events", "-A", "--field-selector", "type=Warning",
      "--sort-by=.lastTimestamp", "-o",
      `jsonpath={range .items[-8:]}{.lastTimestamp}|{.reason}|` +
      `{.involvedObject.kind}/{.involvedObject.name}|{.message}{"\\n"}{end}`,
    ], kubeContext), { stdio: "pipe", timeout: 10000 }),
  ]);

  // Parse API latency
  if (apiRes.status === "fulfilled") {
    result.apiLatencyMs = apiRes.value as number;
    result.apiReachable = true;
  }

  // Parse nodes
  if (nodesRes.status === "fulfilled") {
    const topMap = new Map<string, { cpu: string; cpuPct: string; mem: string; memPct: string }>();
    if (topRes.status === "fulfilled") {
      for (const line of (topRes.value as any).stdout.trim().split("\n").filter(Boolean)) {
        const parts = line.trim().split(/\s+/);
        if (parts.length >= 5) {
          topMap.set(parts[0], { cpu: parts[1], cpuPct: parts[2], mem: parts[3], memPct: parts[4] });
        }
      }
    }

    for (const line of (nodesRes.value as any).stdout.trim().split("\n").filter(Boolean)) {
      const [name, pool, ready, version, os, runtime] = line.split("|");
      if (!name) continue;
      const top = topMap.get(name);
      result.nodes.push({
        name, pool: pool || "-",
        status: ready === "True" ? "Ready" : "NotReady",
        version: version || "-",
        cpuCores: top?.cpu || "-", cpuPct: top?.cpuPct || "-",
        memBytes: top?.mem || "-", memPct: top?.memPct || "-",
        os: os || "-", runtime: runtime || "-",
      });
    }
  }

  // Parse quotas
  if (quotaRes.status === "fulfilled") {
    for (const line of (quotaRes.value as any).stdout.trim().split("\n").filter(Boolean)) {
      const [ns, cpuUsed, cpuHard, memUsed, memHard] = line.split("|");
      if (ns) result.quotas.push({ namespace: ns, cpuUsed: cpuUsed || "0", cpuHard: cpuHard || "-", memUsed: memUsed || "0", memHard: memHard || "-" });
    }
  }

  // Parse PVCs
  if (pvcRes.status === "fulfilled") {
    for (const line of (pvcRes.value as any).stdout.trim().split("\n").filter(Boolean)) {
      const [ns, name, phase, size] = line.split("|");
      if (ns) result.pvcs.push({ namespace: ns, name: name || "-", phase: phase || "Unknown", size: size || "-" });
    }
  }

  // Parse warnings
  if (eventsRes.status === "fulfilled") {
    for (const line of (eventsRes.value as any).stdout.trim().split("\n").filter(Boolean)) {
      const [time, reason, object, ...rest] = line.split("|");
      if (reason) result.warnings.push({
        time: time ? timeSince(new Date(time)) : "-",
        reason,
        object: (object || "-").substring(0, 40),
        message: (rest.join("|") || "").substring(0, 60),
      });
    }
  }

  return result;
}
