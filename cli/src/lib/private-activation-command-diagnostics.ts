// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export type PrivateCommandPhase = "Unscoped" | "Review" | "Pausing" | "Retired" | "Rotating" | "Restoring" | "Qualified";
const phases = new Set<PrivateCommandPhase>(["Unscoped", "Review", "Pausing", "Retired", "Rotating", "Restoring", "Qualified"]);
const reasons = new Set([
  "BadRequest", "Unauthorized", "Forbidden", "NotFound", "AlreadyExists", "Conflict", "Invalid",
  "Timeout", "ServerTimeout", "TooManyRequests", "ServiceUnavailable", "InternalError",
  "MethodNotAllowed", "Gone", "RequestEntityTooLarge", "UnsupportedMediaType",
]);
const kinds: Record<string, string> = {
  namespace: "Namespace", namespaces: "Namespace", deployment: "Deployment", "deployments.apps": "Deployment",
  karssandbox: "KarsSandbox", karstask: "KarsTask", secret: "Secret", serviceaccount: "ServiceAccount",
  pods: "Pod", "replicasets.apps": "ReplicaSet", "karscredentialgrants.kars.azure.com": "KarsCredentialGrant",
  validatingadmissionpolicy: "AdmissionPolicy", validatingadmissionpolicybinding: "AdmissionBinding",
  "roles,rolebindings,clusterroles,clusterrolebindings": "AuthorizationInventory",
};

interface Facts {
  version: 1;
  phase: PrivateCommandPhase;
  operation: "get" | "patch" | "create" | "auth" | "other";
  resourceKind: string;
  serverReason: string;
  exitCode: number | null;
}

export class PrivateCommandFailure extends Error {
  constructor(readonly facts: Readonly<Facts>) {
    super(`KARS_PRIVATE_COMMAND_FAILURE ${JSON.stringify(facts)}`);
    this.name = "PrivateCommandFailure";
  }
}

/** Process diagnostics contain only fixed enums and an integer exit status.
 * The original error/cause, argv, names, stdout and stderr are never retained. */
export function privateCommandFailure(
  error: unknown, args: readonly string[], phase: PrivateCommandPhase = "Unscoped",
): PrivateCommandFailure {
  phase = phases.has(phase) ? phase : "Unscoped";
  if (error instanceof PrivateCommandFailure) {
    return new PrivateCommandFailure({ ...error.facts, phase });
  }
  const value = error && typeof error === "object" ? error as { stderr?: unknown; exitCode?: unknown } : {};
  const stderr = typeof value.stderr === "string" && Buffer.byteLength(value.stderr) <= 65_536 ? value.stderr : "";
  const matches = [...stderr.matchAll(/^Error from server \(([A-Za-z]+)\):/gm)];
  const reason = matches.length === 1 && reasons.has(matches[0]![1]!) ? matches[0]![1]! : "Unknown";
  const operation = (["get", "patch", "create", "auth"].includes(args[0] ?? "") ? args[0] : "other") as Facts["operation"];
  return new PrivateCommandFailure({
    version: 1, phase, operation,
    resourceKind: operation === "auth" ? "AuthorizationCheck"
      : Object.hasOwn(kinds, args[1] ?? "") ? kinds[args[1]!]! : "Other",
    serverReason: reason,
    exitCode: typeof value.exitCode === "number" && Number.isInteger(value.exitCode)
      && value.exitCode >= 0 && value.exitCode <= 255 ? value.exitCode : null,
  });
}

export function scopedCommandFailure(error: unknown, args: readonly string[], phase: PrivateCommandPhase): unknown {
  if (error instanceof PrivateCommandFailure || (error && typeof error === "object"
    && ("exitCode" in error || ("failed" in error && error.failed === true)))) {
    return privateCommandFailure(error, args, phase);
  }
  return error;
}
