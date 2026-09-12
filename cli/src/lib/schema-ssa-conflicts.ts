// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

const managerClasses = new Set(["helm", "python-httpx", "Python-urllib", "kubectl-patch", "kubectl",
  "kubectl-client-side-apply", "kars-schema-stage"]);
const conflictPaths: Readonly<Record<string, string>> = {
  ".spec.versions": "spec/versions",
  ".metadata.annotations.meta.helm.sh/release-name": "metadata/annotations/meta.helm.sh/release-name",
  ".metadata.annotations.meta.helm.sh/release-namespace": "metadata/annotations/meta.helm.sh/release-namespace",
  ".metadata.annotations.kars.azure.com/core-schema-owner": "metadata/annotations/kars.azure.com/core-schema-owner",
  ".metadata.annotations.kars.azure.com/core-schema-spec": "metadata/annotations/kars.azure.com/core-schema-spec",
  ".metadata.labels.app.kubernetes.io/managed-by": "metadata/labels/app.kubernetes.io/managed-by",
};

export interface SsaConflictFacts {
  conflictKind: "field-manager" | "resource-version";
  conflictCount?: number;
  conflictFields?: string[];
  conflictManagers?: string[];
}

/** kubectl's SSA wrapper loses StatusError's usual "Error from server" prefix.
 * Parse only the pinned upstream conflict grammar, never echo manager/field text. */
export function schemaSsaConflict(stderr: string): SsaConflictFacts | undefined {
  if (/^(?:error: )?Operation cannot be fulfilled on customresourcedefinitions(?:\.apiextensions\.k8s\.io)? "[^"\r\n]+": the object has been modified; please apply your changes to the latest version and try again\.?$/m.test(stderr.slice(0, 16384))) {
    return { conflictKind: "resource-version" };
  }
  const match = /^(?:error: )?Apply failed with ([1-9][0-9]?) conflicts?: /m.exec(stderr.slice(0, 16384));
  if (!match || Number(match[1]) > 32) return undefined;
  const text = stderr.slice(match.index + match[0].length, 16384).split("\nPlease review the fields above")[0].trim();
  const fields: string[] = [];
  const managers = new Set<string>();
  let managerSeen = false;
  for (const line of text.split("\n")) {
    const manager = /^conflicts? with ("(?:[^"\\]|\\.)*")/.exec(line);
    if (manager) {
      let name: unknown;
      try { name = JSON.parse(manager[1]); } catch { return undefined; }
      managers.add(typeof name === "string" && managerClasses.has(name) ? name : "other");
      managerSeen = true;
      const field = /: (\.[^\r\n]+)$/.exec(line)?.[1];
      if (field) fields.push(conflictPaths[field] ?? "other");
      else if (!line.endsWith(":")) return undefined;
    } else if (managerSeen && line.startsWith("- .")) {
      fields.push(conflictPaths[line.slice(2)] ?? "other");
    } else {
      return undefined;
    }
  }
  if (fields.length !== Number(match[1]) || !managers.size || managers.size > fields.length) return undefined;
  return { conflictKind: "field-manager", conflictCount: fields.length,
    conflictFields: [...new Set(fields)].sort(), conflictManagers: [...managers].sort() };
}
