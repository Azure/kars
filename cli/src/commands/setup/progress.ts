// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { BetaExecute, BetaTools } from "../dev/kind-beta.js";
import type { SetupCheck, SetupObservation } from "./types.js";
import { coreHookFailureMessage, readCoreHookFailure } from "../../lib/core-hook-diagnostics.js";

type RecordValue = Record<string, unknown>;
const record = (value: unknown): RecordValue => value !== null && typeof value === "object" && !Array.isArray(value) ? value as RecordValue : {};
const list = (value: unknown): RecordValue[] => Array.isArray(value) ? value.map(record) : [];
const count = (value: unknown): number => Number.isSafeInteger(value) && Number(value) >= 0 ? Number(value) : 0;
const name = (value: unknown): value is string => typeof value === "string" && /^[a-z0-9][a-z0-9.-]{0,252}$/.test(value);
const architecture = (value: unknown) => value === "amd64" || value === "arm64" ? value : undefined;

/** Project known status fields and fixed hook codes, never raw logs, specs or errors. */
export function summarizeSetupResources(value: unknown): SetupCheck[] {
  const input = record(value);
  if (!Array.isArray(input.items)) throw new Error("Invalid Kubernetes observation");
  const items = list(input.items), nodes = items.filter(item => item.kind === "Node");
  const rows: SetupCheck[] = [];
  for (const item of items) {
    const metadata = record(item.metadata), status = record(item.status), spec = record(item.spec);
    if (!name(metadata.name) || (item.kind !== "Node" && !name(metadata.namespace))) continue;
    if (item.kind !== "Node" && !["kars-system", "agentmesh", "kars-sre", "kars-local-idp", "kube-system", "headlamp", "local-path-storage"].includes(String(metadata.namespace))) continue;
    if (!["Node", "Pod", "Job", "Deployment", "KarsSandbox"].includes(String(item.kind))) continue;
    if (item.kind === "KarsSandbox" && (metadata.name !== "sre" || metadata.namespace !== "kars-system")) continue;
    const conditions = list(status.conditions);
    const row: SetupCheck = { id: `${item.kind}/${metadata.namespace ?? ""}/${metadata.name}`,
      label: `${item.kind}: ${metadata.name}`, status: "running", detail: String(metadata.namespace ?? "Cluster node") };
    const startedAt = typeof metadata.creationTimestamp === "string" ? Date.parse(metadata.creationTimestamp) : NaN;
    if (Number.isFinite(startedAt)) row.startedAt = startedAt;
    if (item.kind === "KarsSandbox") {
      row.label = "SRE runtime";
      const current = conditions.filter(condition => count(metadata.generation) > 0 && condition.observedGeneration === metadata.generation);
      const degraded = current.find(condition => condition.type === "Degraded" && condition.status === "True");
      row.status = degraded ? "blocked" : current.some(condition => condition.type === "Ready" && condition.status === "True") ? "passed" : "running";
      row.detail = degraded
        ? degraded.message === "no inference endpoint configured (FOUNDRY_ENDPOINT or AZURE_OPENAI_ENDPOINT)"
          ? "Controller requires an Azure inference endpoint. For a named provider such as GitHub Copilot, update the controller; do not add a dummy Azure endpoint. SRE enrollment cannot progress."
          : "Controller reports an SRE prerequisite failure. Inspect the retained controller state; enrollment may not progress."
        : row.status === "passed" ? "Controller reports the SRE runtime ready. Enrollment and funding are verified separately."
          : current.length === 0 ? "Waiting for the controller to reconcile the current SRE configuration."
            : !spec.inferenceBudgetRef ? "SRE source created. Waiting for the controller to bind initial funding before the runtime can start."
              : "Initial funding reference attached. Waiting for SRE runtime readiness; enrollment still verifies the active funding ledger.";
    } else if (item.kind === "Node") {
      const ready = conditions.find(condition => condition.type === "Ready");
      row.status = ready?.status === "True" ? "passed" : "blocked";
      row.detail = `${architecture(record(metadata.labels)["kubernetes.io/arch"]) ?? "Unknown architecture"} · ${row.status === "passed" ? "Node ready" : "Node not ready"}`;
    } else if (item.kind === "Job") {
      const failed = conditions.find(condition => ["Failed", "FailureTarget"].includes(String(condition.type)) && condition.status === "True");
      row.status = failed ? "failed" : conditions.some(condition => condition.type === "Complete" && condition.status === "True") ? "passed" : "running";
      row.detail += ` · ${count(status.active)} active · ${count(status.succeeded)} succeeded · ${count(status.failed)} failed`;
      if (failed) row.detail += failed.reason === "DeadlineExceeded" ? " · Job deadline exceeded" : failed.reason === "BackoffLimitExceeded" ? " · Job retry limit reached" : " · Job failed";
      const hook = record(metadata.annotations)["helm.sh/hook"];
      if (hook === "pre-install" || hook === "post-install") row.detail += ` · Helm ${hook} hook`;
      if (count(spec.activeDeadlineSeconds)) row.detail += ` · Deadline ${count(spec.activeDeadlineSeconds)}s`;
      if (metadata.namespace === "kars-system" && metadata.name.startsWith("kars-sre-install-") && row.status === "running") {
        row.detail += " · Creates the SRE source, registers its authority, then waits for verified initial funding. See SRE runtime for controller progress or blockers.";
      }
    } else if (item.kind === "Deployment") {
      const desired = spec.replicas === undefined ? 1 : count(spec.replicas);
      const ready = count(status.readyReplicas), updated = count(status.updatedReplicas);
      const observed = count(status.observedGeneration) >= count(metadata.generation);
      row.status = observed && ready === desired && updated === desired ? "passed" : "running";
      if (conditions.some(condition => condition.type === "Progressing" && condition.status === "False")) row.status = "failed";
      row.detail += ` · ${ready}/${desired} ready · ${updated}/${desired} updated`;
    } else {
      const containers = list(status.containerStatuses), init = list(status.initContainerStatuses);
      const desired = list(spec.containers).length;
      row.status = status.phase === "Succeeded" ? "passed" : status.phase === "Failed" ? "failed"
        : desired > 0 && containers.length === desired && containers.every(container => container.ready === true) ? "passed" : "running";
      row.detail += ` · ${containers.filter(container => container.ready === true).length}/${desired} containers ready`;
      row.detail += ` · ${[...init, ...containers].reduce((sum, container) => sum + count(container.restartCount), 0)} restarts`;
      const waiting = [...init, ...containers].map(container => record(record(container.state).waiting).reason);
      const reason = waiting.find(reason => ["ImagePullBackOff", "ErrImagePull", "CrashLoopBackOff", "CreateContainerConfigError", "ContainerCreating", "PodInitializing"].includes(String(reason)));
      if (reason) { row.detail += ` · ${reason}`; if (reason !== "ContainerCreating" && reason !== "PodInitializing") row.status = "blocked"; }
      if (metadata.namespace === "kars-system" && /^kars-(?:schema|sre-install)-/.test(metadata.name)) {
        for (const container of containers) {
          if (!["schemas", "sre-enrollment"].includes(String(container.name))) continue;
          const failure = readCoreHookFailure(record(record(container.state).terminated).message);
          if (failure) {
            row.diagnosticCode = failure;
            row.detail += ` · ${coreHookFailureMessage(failure)} [${failure}]`;
          }
        }
      }
      const scheduling = conditions.find(condition => condition.type === "PodScheduled" && condition.status === "False");
      if (scheduling) {
        row.status = "blocked";
        const message = typeof scheduling.message === "string" ? scheduling.message : "";
        row.detail += message.includes("Insufficient memory") ? " · Insufficient node memory"
          : message.includes("Insufficient cpu") ? " · Insufficient node CPU"
            : message.includes("node affinity/selector") ? " · No node matches this workload's selector" : " · Waiting for a schedulable node";
      }
    }
    const workloadSpec = item.kind === "Job" || item.kind === "Deployment" ? record(record(spec.template).spec) : spec;
    const required = architecture(record(workloadSpec.nodeSelector)["kubernetes.io/arch"]);
    if (required && nodes.length && nodes.every(node => {
      const actual = architecture(record(record(node.metadata).labels)["kubernetes.io/arch"]);
      return actual && actual !== required;
    })) {
      if (row.status !== "failed") row.status = "blocked";
      row.detail += ` · Requires ${required}; available nodes have a different architecture. Use matching qualified images and nodes.`;
    }
    rows.push(row);
  }
  const priority = { failed: 0, blocked: 1, running: 2, pending: 3, passed: 4 };
  return rows.sort((left, right) => priority[left.status] - priority[right.status] || left.id.localeCompare(right.id));
}

export function observeSetupCluster(tools: BetaTools, cluster: string, kubeconfig: string, execute: BetaExecute,
  publish: (value: SetupObservation) => void) {
  const abort = new AbortController();
  let timer: ReturnType<typeof setTimeout> | undefined, pending: Promise<void> | undefined;
  let observation: SetupObservation = { status: "live", message: "Reading workload and Helm-hook status…", resources: [] };
  publish(observation);
  const sample = (): Promise<void> => {
    if (abort.signal.aborted) return Promise.resolve();
    if (pending) return pending;
    clearTimeout(timer);
    pending = (async () => {
      try {
        const result = await execute(tools.kubectl, ["--kubeconfig", kubeconfig, "--context", `kind-${cluster}`, "--request-timeout=5s",
          "get", "nodes,pods,jobs,deployments", "--all-namespaces", "-o", "json"], { env: tools.env, stdio: "pipe", cancelSignal: abort.signal });
        const objects = record(JSON.parse(result.stdout));
        if (!Array.isArray(objects.items)) throw new Error("Invalid Kubernetes observation");
        const items = [...objects.items];
        if (items.some(item => record(item).kind === "Deployment"
          && record(record(item).metadata).namespace === "kars-system" && record(record(item).metadata).name === "kars-controller")) {
          const sre = await execute(tools.kubectl, ["--kubeconfig", kubeconfig, "--context", `kind-${cluster}`, "--request-timeout=5s",
            "get", "karssandboxes.kars.azure.com", "sre", "-n", "kars-system", "--ignore-not-found", "-o", "json"],
          { env: tools.env, stdio: "pipe", cancelSignal: abort.signal });
          if (sre.stdout.trim()) {
            const source = record(JSON.parse(sre.stdout));
            if (source.kind !== "KarsSandbox" || record(source.metadata).name !== "sre" || record(source.metadata).namespace !== "kars-system") {
              throw new Error("Invalid SRE observation");
            }
            items.push(source);
          }
        }
        const resources = summarizeSetupResources({ items });
        observation = { status: "live", observedAt: Date.now(), resources: resources.slice(0, 80),
          message: resources.length ? `${resources.filter(row => row.status === "passed").length}/${resources.length} observed resources ready or completed. Not customer acceptance.${resources.length > 80 ? " Showing the first 80, with failures first." : ""}`
            : "No installation workloads observed yet. Waiting for Kubernetes objects." };
      } catch {
        observation = { ...observation, status: "unavailable", message: "Status refresh failed. Last observations are retained, not current. Installation status is not inferred from this failure." };
      }
      if (!abort.signal.aborted) publish(observation);
    })().finally(() => {
      pending = undefined;
      if (!abort.signal.aborted) { timer = setTimeout(() => { void sample(); }, 3000); timer.unref(); }
    });
    return pending;
  };
  void sample();
  return { sample: async () => {
    await pending;
    await sample();
  }, stop: () => {
    if (abort.signal.aborted) return;
    clearTimeout(timer); abort.abort();
    publish({ ...observation, status: "stopped", message: `Observation stopped. ${observation.message}` });
  } };
}
