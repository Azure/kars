// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Sandbox-list fetchers (Docker + AKS).
 *
 * Extracted from `cli/src/commands/operator.ts` (S15.e.2) so the
 * top-level dashboard module can stay under §4.2's 800-line cap.
 *
 * Each fetcher returns a tree-ordered `SandboxInfo[]`:
 * controllers sorted alphabetically; sub-agents grouped under
 * their parent; orphans appended last.
 *
 * No behavioral change vs. the originals — bodies are byte-identical
 * apart from one mechanical edit: the closure-captured outer-scope
 * `kubeContext` is now passed in as an explicit parameter to the AKS
 * fetcher.
 */

import { execa } from "execa";
import type { HealthState, SandboxInfo } from "../types.js";
import { kctl, timeSince } from "../helpers.js";

/**
 * Unified Docker + AKS sandbox view (concatenated; deduplication is
 * intentionally avoided — same-named sandboxes in both runtimes are a
 * legitimate handoff state, e.g. dormant local + active-successor cloud).
 */
export async function fetchSandboxes(kubeContext?: string): Promise<SandboxInfo[]> {
  const [dockerResult, aksResult] = await Promise.allSettled([
    fetchSandboxesDocker(),
    fetchSandboxesAKS(kubeContext),
  ]);
  const docker = dockerResult.status === "fulfilled" ? dockerResult.value : [];
  const aks = aksResult.status === "fulfilled" ? aksResult.value : [];
  return [...docker, ...aks];
}

export async function fetchSandboxesAKS(kubeContext?: string): Promise<SandboxInfo[]> {
  try {
    const [sandboxResult, ipResult] = await Promise.allSettled([
      execa("kubectl", kctl(["get", "clawsandbox", "-A", "-o", "json"], kubeContext), { stdio: "pipe" }),
      execa("kubectl", kctl(["get", "inferencepolicy", "-A", "-o", "json"], kubeContext), { stdio: "pipe" }),
    ]);
    if (sandboxResult.status !== "fulfilled") return [];
    const data = JSON.parse(sandboxResult.value.stdout);
    const items: any[] = data.items || [];

    // Build a `<namespace>/<name>` → primary deployment map so we can resolve
    // the model used by each ClawSandbox via spec.inferenceRef.name without
    // making N additional kubectl calls. Post-S10/S13 the model lives on the
    // referenced InferencePolicy, NOT on the ClawSandbox spec.
    const ipModel = new Map<string, string>();
    if (ipResult.status === "fulfilled") {
      try {
        const ipData = JSON.parse(ipResult.value.stdout);
        for (const it of (ipData.items || []) as any[]) {
          const ns = it?.metadata?.namespace;
          const nm = it?.metadata?.name;
          const dep = it?.spec?.modelPreference?.primary?.deployment;
          if (ns && nm && typeof dep === "string" && dep) {
            ipModel.set(`${ns}/${nm}`, dep);
          }
        }
      } catch { /* non-fatal */ }
    }

    // Fetch pods + secrets for ALL sandboxes in parallel (not sequentially)
    const enriched = await Promise.allSettled(items.map(async (item) => {
      const name: string = item.metadata?.name || "";
      if (!name) return null;
      const ns: string = item.metadata?.namespace || "azureclaw-system";
      const phase: string = item.status?.phase || "Unknown";
      // Resolution order, post-S10/S13:
      //   1. spec.inferenceRef.name → InferencePolicy.modelPreference.primary.deployment
      //   2. legacy spec.inference.model (pre-S10 sandboxes still in flight)
      //   3. "gpt-4.1" fallback (clearly visible default for un-typed sandboxes)
      const inferenceRefName: string | undefined = item.spec?.inferenceRef?.name;
      const ipKey = inferenceRefName ? `${ns}/${inferenceRefName}` : "";
      const model: string = (ipKey && ipModel.get(ipKey))
        || item.spec?.inference?.model
        || "gpt-4.1";
      const isolation: string = item.spec?.sandbox?.isolation || "enhanced";
      const created: string = item.metadata?.creationTimestamp || "";
      const labels: Record<string, string> = item.metadata?.labels || {};
      const parentLabel = labels["azureclaw.azure.com/parent"] || "";
      const role: "controller" | "sub-agent" = parentLabel ? "sub-agent" : "controller";

      // Detect handoff successor (CRD has spec.handoff.mode = "restore")
      let handoffState: SandboxInfo["handoffState"] = undefined;
      if (item.spec?.handoff?.mode === "restore") {
        handoffState = "active-successor";
      }

      const sandboxNs = `azureclaw-${name}`;
      let podStatus = phase;
      let podName = "";
      let podCreated = "";
      let channels = "";
      let health: HealthState = "pending";
      let restarts = 0;

      // Fetch pods and secret in parallel
      const [podResult, secretResult] = await Promise.allSettled([
        execa("kubectl", kctl([
          "get", "pods", "-n", sandboxNs, "-o", "json",
        ], kubeContext), { stdio: "pipe", timeout: 15000 }),
        execa("kubectl", kctl([
          "get", "secret", `${name}-credentials`, "-n", sandboxNs,
          "-o", "jsonpath={.data}",
        ], kubeContext), { stdio: "pipe", timeout: 10000 }),
      ]);

      if (podResult.status === "fulfilled") {
        try {
          const pods = JSON.parse(podResult.value.stdout);
          if (pods.items?.length > 0) {
            const sorted = [...pods.items].sort((a: any, b: any) => {
              const order = (p: any) => {
                if (p.metadata?.deletionTimestamp) return 3;
                const phase = p.status?.phase || "";
                if (phase === "Running") return 0;
                if (phase === "Pending") return 1;
                return 2;
              };
              return order(a) - order(b);
            });
            const pod = sorted[0];
            const isTerminating = !!pod.metadata?.deletionTimestamp;
            podName = pod.metadata?.name || "";
            podCreated = pod.metadata?.creationTimestamp || "";
            const pPhase = isTerminating ? "Terminating" : (pod.status?.phase || "Unknown");
            const statuses: any[] = pod.status?.containerStatuses || [];
            const readyCount = statuses.filter((c: any) => c.ready).length;
            const totalCount = statuses.length;
            restarts = statuses.reduce((sum: number, c: any) => sum + (c.restartCount || 0), 0);
            podStatus = totalCount > 0 ? `${pPhase} (${readyCount}/${totalCount})` : pPhase;

            const hasCrash = statuses.some((c: any) =>
              c.state?.waiting?.reason === "CrashLoopBackOff" ||
              c.state?.waiting?.reason === "Error",
            );
            if (isTerminating) health = "degraded";
            else if (hasCrash || pPhase === "Failed") health = "down";
            else if (readyCount === totalCount && totalCount > 0) health = "healthy";
            else if (readyCount > 0) health = "degraded";
            else if (pPhase === "Pending") health = "pending";
            else health = "unknown";

            if (isTerminating || pPhase === "Pending" || readyCount === 0) podName = "";
          }
        } catch { /* bad JSON */ }
      }

      if (secretResult.status === "fulfilled") {
        const secretOut = secretResult.value.stdout;
        const chs: string[] = [];
        if (secretOut.includes("TELEGRAM")) chs.push("TG");
        if (secretOut.includes("SLACK")) chs.push("SL");
        if (secretOut.includes("DISCORD")) chs.push("DC");
        channels = chs.join(",") || "-";
      } else {
        channels = "-";
      }

      let age = "-";
      const ageSource = podCreated || created;
      if (ageSource) {
        const d = new Date(ageSource);
        if (!isNaN(d.getTime())) age = timeSince(d);
      }

      return {
        name, namespace: sandboxNs, status: podStatus,
        health, model, isolation,
        channels, age, podName, restarts,
        role, parent: parentLabel, handoffState,
        runtime: "aks",
      } as SandboxInfo;
    }));

    const results: SandboxInfo[] = enriched
      .filter((r): r is PromiseFulfilledResult<SandboxInfo | null> => r.status === "fulfilled" && r.value !== null)
      .map((r) => r.value!);

    // Build tree: controllers sorted alphabetically, sub-agents right after their parent
    const controllers = results.filter((s) => s.role === "controller").sort((a, b) => a.name.localeCompare(b.name));
    const subAgents = results.filter((s) => s.role === "sub-agent");
    const tree: SandboxInfo[] = [];
    for (const ctrl of controllers) {
      tree.push(ctrl);
      // Attach sub-agents that belong to this controller
      const children = subAgents.filter((s) => s.parent === ctrl.name).sort((a, b) => a.name.localeCompare(b.name));
      tree.push(...children);
    }
    // Orphaned sub-agents (parent not in list) go at the end
    const placed = new Set(tree.map((s) => s.name));
    for (const s of subAgents) {
      if (!placed.has(s.name)) tree.push(s);
    }

    return tree;
  } catch {
    return [];
  }
}

export async function fetchSandboxesDocker(): Promise<SandboxInfo[]> {
  try {
    const { stdout } = await execa("docker", [
      "ps", "-a", "--format",
      "{{.Names}}|{{.Status}}|{{.Label \"azureclaw.parent\"}}|{{.Label \"azureclaw.spawned-by\"}}|{{.CreatedAt}}",
      "--filter", "name=azureclaw-",
    ], { stdio: "pipe" });

    const results: SandboxInfo[] = [];
    for (const line of stdout.split("\n").filter(Boolean)) {
      const [containerName, status, parent, , createdAt] = line.split("|");
      if (!containerName?.startsWith("azureclaw-")) continue;
      // Skip AGT infrastructure containers
      if (containerName.includes("agt-postgres") || containerName.includes("agt-relay") || containerName.includes("agt-registry")) continue;

      const name = containerName.replace(/^azureclaw-/, "");
      const isUp = status?.startsWith("Up") ?? false;
      const health: HealthState = isUp ? "healthy" : "down";
      const podStatus = isUp ? "Running" : "Exited";
      const role: "controller" | "sub-agent" = parent ? "sub-agent" : "controller";

      let age = "-";
      if (createdAt) {
        const d = new Date(createdAt);
        if (!isNaN(d.getTime())) age = timeSince(d);
      }

      // Probe model + handoff state + channels from container env vars
      let model = "gpt-4.1";
      let channels = "-";
      let handoffState: SandboxInfo["handoffState"] = undefined;
      if (isUp) {
        // Read model + channels from env vars (single exec call)
        try {
          const { stdout: envOut } = await execa("docker", [
            "exec", containerName, "printenv",
          ], { stdio: "pipe", timeout: 5000 });
          const chs: string[] = [];
          for (const line of envOut.split("\n")) {
            if (line.startsWith("TELEGRAM_BOT_TOKEN=")) chs.push("TG");
            else if (line.startsWith("SLACK_BOT_TOKEN=")) chs.push("SL");
            else if (line.startsWith("DISCORD_BOT_TOKEN=")) chs.push("DC");
            else if (line.startsWith("DEFAULT_MODEL=")) {
              const val = line.split("=")[1]?.trim();
              if (val) model = val;
            }
          }
          channels = chs.join(",") || "-";
        } catch { /* env probe fail */ }

        // Check if this agent has been handed off (dormant)
        try {
          const { stdout: hsOut } = await execa("docker", [
            "exec", containerName, "curl", "-s", "--max-time", "2", "http://localhost:8443/agt/handoff/status",
          ], { stdio: "pipe" });
          const hs = JSON.parse(hsOut);
          if (hs.phase === "complete" && hs.direction === "local_to_aks") {
            handoffState = "dormant";
          } else if (hs.phase === "running" && hs.direction === "aks_to_local") {
            handoffState = "returning";
          }
        } catch { /* no handoff state */ }
      }

      results.push({
        name,
        namespace: containerName,
        status: podStatus,
        health: handoffState === "dormant" ? "dormant" : health,
        model,
        isolation: "standard",
        channels,
        age,
        podName: containerName,
        restarts: 0,
        role,
        parent: parent || "",
        handoffState,
        runtime: "docker",
      });
    }

    // Tree ordering: controllers first, sub-agents after their parent
    const controllers = results.filter((s) => s.role === "controller").sort((a, b) => a.name.localeCompare(b.name));
    const subAgents = results.filter((s) => s.role === "sub-agent");
    const tree: SandboxInfo[] = [];
    for (const ctrl of controllers) {
      tree.push(ctrl);
      tree.push(...subAgents.filter((s) => s.parent === ctrl.name));
    }
    const placed = new Set(tree.map((s) => s.name));
    for (const s of subAgents) {
      if (!placed.has(s.name)) tree.push(s);
    }

    return tree;
  } catch {
    return [];
  }
}
