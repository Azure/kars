// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { PrivateCommandFailure } from "./private-activation-command-diagnostics.js";

// Only these fixed messages cross the Job-log/browser boundary. Never return
// the original exception, subprocess output, arguments or API object content.
const messages = {
  Unexpected: "Unexpected Core hook failure; inspect the retained hook and controller state.",
  Credentials: "Schema hook requires its own projected service-account credentials",
  Endpoint: "Schema hook requires the reviewed in-cluster IPv4 API endpoint",
  Options: "Schema hook accepts each exact release option once and no additional flags",
  OriginalInstall: "SRE bootstrap requires the original kars/kars-system revision-one Helm 4.1.3 install",
  Deadline: "SRE bootstrap deadline expired",
  ResponseBound: "SRE bootstrap response/deadline bound exceeded",
  PendingInstall: "SRE bootstrap is not the sole original pending install",
  ImmutableIntent: "Missing exact immutable SRE intent in pending Core manifest",
  IntentBound: "Invalid SRE intent bound",
  IncompleteIntent: "Incomplete operator SRE intent",
  EmptyCatalog: "Empty SRE catalog requires Kind beta, record-only accounting and unbounded token-only funding",
  ExistingSource: "SRE bootstrap requires three exact, absent custom resources; no adoption",
  FundingProvenance: "SRE source omitted its immutable funding provenance",
  ExistingEnrollment: "SRE bootstrap cannot resume an existing occupant or enrollment",
  Prerequisites: "Core prerequisites are absent",
  ManifestChanged: "Pinned Core manifest changed",
  CatalogChanged: "Operator catalog changed during SRE enrollment",
  CoreNotReady: "Core identity or immutable intent is not ready",
  CatalogPolicy: "Empty SRE catalog lost its Kind beta record-only policy",
  ControllerChanged: "Core controller intent changed",
  WorkspaceChanged: "SRE workspace/cluster identity changed",
  SourceChanged: "Created SRE source/support identity or configuration changed",
  RuntimeChanged: "Created SRE runtime identity changed",
  OperationRefused: "SRE hook refused an unreviewed operation",
  CreateIntent: "SRE hook CREATE escaped its pinned intent",
  CreateIdentity: "SRE CREATE omitted its exact committed identity; no adoption",
  SourceRace: "SRE source/support occupant raced this attempt; no adoption",
  FreshProvenance: "Fresh SRE provenance changed before enrollment",
  EnrollmentChanged: "SRE bootstrap identity changed after atomic enrollment",
  BudgetRoot: "SRE budget root differs from enrollment",
  AllowanceChanged: "Consumed first-install allowance changed",
  FundingOperation: "Unreviewed first-install funding operation",
  FundingCatalog: "Funding escaped the Core catalog",
  Registrar: "Explicit cluster-level kars-sre-registrar permission is required",
  NamespaceClaim: "SRE source was staged but its exact namespace claim did not converge",
  Authority: "SRE authority did not converge; verify the prerequisite controller/router images and registration status",
  ApiIdentity: "SRE authority API response omitted its exact identity",
  ApiInventory: "SRE authority inventory is malformed",
  CommandForbidden: "The Kubernetes API denied a hook request. Inspect hook RBAC and admission policy.",
  CommandUnauthorized: "The Kubernetes API rejected the hook's authentication.",
  CommandConflict: "A hook request conflicted with existing state. The attempt was not retried.",
  CommandInvalid: "The Kubernetes API rejected a hook resource as invalid.",
  CommandUnavailable: "The Kubernetes API was unavailable or timed out during a hook request.",
  CommandFailed: "A hook Helm or kubectl command failed. Inspect the retained hook and controller state.",
} as const;

export type CoreHookFailureCode = keyof typeof messages;

export function isCoreHookFailureCode(value: unknown): value is CoreHookFailureCode {
  return typeof value === "string" && Object.hasOwn(messages, value);
}

export function coreHookFailureMessage(code: CoreHookFailureCode): string {
  return messages[code];
}

export function coreHookFailureCode(error: unknown): CoreHookFailureCode {
  if (error instanceof PrivateCommandFailure) {
    switch (error.facts.serverReason) {
      case "Forbidden": return "CommandForbidden";
      case "Unauthorized": return "CommandUnauthorized";
      case "Conflict": case "AlreadyExists": return "CommandConflict";
      case "Invalid": case "BadRequest": return "CommandInvalid";
      case "Timeout": case "ServerTimeout": case "ServiceUnavailable": return "CommandUnavailable";
      default: return "CommandFailed";
    }
  }
  if (!(error instanceof Error)) return "Unexpected";
  return (Object.keys(messages) as CoreHookFailureCode[]).find(code => messages[code] === error.message) ?? "Unexpected";
}

export function readCoreHookFailure(message: unknown): CoreHookFailureCode | undefined {
  if (typeof message !== "string" || message.length > 4096) return undefined;
  const codes = [...message.matchAll(/^KARS_CORE_HOOK_FAILURE ([A-Za-z]+)$/gm)];
  return codes.length === 1 && isCoreHookFailureCode(codes[0][1]) ? codes[0][1] : undefined;
}
