// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { asRecord, type UnknownRecord } from "./core.js";

function supportPlanNames(value: unknown): string[] {
  if (typeof value === "string") return [value];
  if (Array.isArray(value)) return value.flatMap(supportPlanNames);
  const record = asRecord(value);
  if (!record) return [];
  return [record.name, record.value, record.displayName].flatMap(supportPlanNames);
}

function isKubernetesOfficial(record: UnknownRecord, inherited = false): boolean {
  const capabilities = asRecord(record.capabilities);
  const plans = supportPlanNames(
    record.supportPlan ?? record.supportPlans ?? capabilities?.supportPlan ?? capabilities?.supportPlans,
  );
  return plans.length === 0
    ? inherited
    : plans.some((plan) => plan.toLowerCase() === "kubernetesofficial");
}

function isStableVersion(record: UnknownRecord): boolean {
  const status = String(record.status ?? record.lifecycle ?? "").toLowerCase();
  return (
    record.isPreview !== true &&
    record.preview !== true &&
    !status.includes("preview") &&
    !status.includes("deprecated")
  );
}

interface AksVersionCandidate {
  version: string;
  official: boolean;
  stable: boolean;
}

function aksVersionCandidates(payload: unknown): AksVersionCandidate[] {
  const root = asRecord(payload);
  const rawValues = Array.isArray(payload)
    ? payload
    : Array.isArray(root?.values)
      ? root.values
      : Array.isArray(root?.valuesProperty)
        ? root.valuesProperty
      : Array.isArray(root?.orchestrators)
        ? root.orchestrators
        : [];
  const candidates: AksVersionCandidate[] = [];

  for (const value of rawValues) {
    const record = asRecord(value);
    if (!record) continue;
    const version = String(record.version ?? record.orchestratorVersion ?? "").trim();
    const official = isKubernetesOfficial(record);
    const stable = isStableVersion(record);
    if (version) candidates.push({ version, official, stable });

    const patchVersions = asRecord(record.patchVersions);
    if (patchVersions) {
      for (const [patchVersion, patchValue] of Object.entries(patchVersions)) {
        const patch = asRecord(patchValue) ?? {};
        candidates.push({
          version: patchVersion,
          official: isKubernetesOfficial(patch, official),
          stable: stable && isStableVersion(patch),
        });
      }
    }

    const patches = Array.isArray(record.patches) ? record.patches : [];
    for (const patchValue of patches) {
      const patch = asRecord(patchValue);
      if (!patch) continue;
      const patchVersion = String(
        patch.version ?? patch.orchestratorVersion ?? "",
      ).trim();
      if (!patchVersion) continue;
      candidates.push({
        version: patchVersion,
        official: isKubernetesOfficial(patch, official),
        stable: stable && isStableVersion(patch),
      });
    }
  }

  return candidates;
}

function versionParts(version: string): number[] | undefined {
  const match = version.replace(/^v/i, "").match(/^(\d+)\.(\d+)(?:\.(\d+))?$/);
  if (!match) return undefined;
  return [Number(match[1]), Number(match[2]), Number(match[3] ?? -1)];
}

function compareVersionsDescending(left: string, right: string): number {
  const a = versionParts(left) ?? [-1, -1, -1];
  const b = versionParts(right) ?? [-1, -1, -1];
  for (let i = 0; i < 3; i += 1) {
    if (a[i] !== b[i]) return b[i] - a[i];
  }
  return 0;
}

/**
 * Validate a requested version, or choose the newest stable patch offered under
 * AKS's KubernetesOfficial (standard) support plan.
 */
export function selectAksKubernetesVersion(
  payload: unknown,
  requestedVersion?: string,
): string {
  const candidates = aksVersionCandidates(payload);
  const supported = candidates.filter(
    (candidate) => candidate.official && candidate.stable && versionParts(candidate.version),
  );

  if (requestedVersion) {
    const normalized = requestedVersion.replace(/^v/i, "");
    const match = supported.find(
      (candidate) => candidate.version.replace(/^v/i, "") === normalized,
    );
    if (match) return normalized;

    const available = [...new Set(supported.map((candidate) => candidate.version))]
      .sort(compareVersionsDescending)
      .slice(0, 8);
    throw new Error(
      `Kubernetes version '${requestedVersion}' is not available in this region under the ` +
        "KubernetesOfficial (standard) support plan." +
        (available.length > 0
          ? ` Supported versions include: ${available.join(", ")}.`
          : " Azure returned no stable KubernetesOfficial versions.") +
        " Choose one shown by `az aks get-versions --location <region> -o table`.",
    );
  }

  const patchCandidates = supported.filter(
    (candidate) => (versionParts(candidate.version)?.[2] ?? -1) >= 0,
  );
  const selectable = patchCandidates.length > 0 ? patchCandidates : supported;
  const selected = selectable.sort((a, b) =>
    compareVersionsDescending(a.version, b.version),
  )[0];
  if (!selected) {
    throw new Error(
      "Azure returned no stable KubernetesOfficial AKS version for this region. " +
        "Try another region or inspect `az aks get-versions --location <region> -o table`.",
    );
  }
  return selected.version;
}
