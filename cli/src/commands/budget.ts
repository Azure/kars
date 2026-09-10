// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import { readFile } from "node:fs/promises";
import { parseAllDocuments } from "yaml";

type ObjectValue = Record<string, unknown>;
type Execute = (args: string[], input?: string) => Promise<string>;
const scope = "GovernedInference";

function object(value: unknown, message: string): ObjectValue {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(message);
  return value as ObjectValue;
}

function name(value: unknown): string {
  if (typeof value !== "string" || !/^[a-z0-9](?:[a-z0-9.-]{0,251}[a-z0-9])?$/.test(value)) {
    throw new Error("A valid Kubernetes resource name is required");
  }
  return value;
}

function amount(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new Error("Budget amounts must be exact nonnegative safe integers; no floating prices");
  }
  return value;
}

/** Explicit opt-in on CREATE only: never silently convert a running legacy UID. */
export function governedPlan(text: string, namespace: string): ObjectValue {
  const documents = parseAllDocuments(text);
  if (documents.length !== 1 || documents[0].errors.length) throw new Error("Expected one valid Task or Team manifest");
  const plan = object(documents[0].toJSON(), "Expected a Task or Team manifest");
  if (plan.apiVersion !== "kars.azure.com/v1alpha1" || !["KarsTask", "KarsTeam"].includes(String(plan.kind))) {
    throw new Error("Governed inference plans must be KarsTask or KarsTeam v1alpha1");
  }
  const metadata = object(plan.metadata, "Manifest metadata is required");
  name(metadata.name);
  if (metadata.uid || metadata.resourceVersion || plan.status) throw new Error("Create a new UID; existing state cannot be imported or reset");
  if (metadata.namespace !== undefined && metadata.namespace !== namespace) {
    throw new Error("Manifest workspace differs from --namespace");
  }
  metadata.namespace = name(namespace);
  const spec = object(plan.spec, "Manifest spec is required");
  const envelope = object(spec.envelope, "Manifest envelope is required");
  const budget = object(envelope.budget, "Set envelope.budget.tokens and/or usdMicros explicitly");
  if (budget.scope !== undefined && budget.scope !== scope) throw new Error("Unsupported budget scope");
  const tokens = amount(budget.tokens ?? 0);
  const usdMicros = amount(budget.usdMicros ?? 0);
  if (tokens === 0 && usdMicros === 0) throw new Error("At least one positive governed-inference cap is required; zero means unbounded");
  budget.scope = scope;
  return plan;
}

function resource(kind: string): string {
  if (kind === "task") return "karstask";
  if (kind === "team") return "karsteam";
  throw new Error("--kind must be task or team");
}

function readObject(text: string): ObjectValue {
  try { return object(JSON.parse(text), "Invalid API object"); }
  catch { throw new Error("Kubernetes returned an invalid budget object"); }
}

function metadata(value: ObjectValue): ObjectValue { return object(value.metadata, "Object identity is missing"); }

export async function budgetStatus(
  execute: Execute, kind: string, target: string, namespace: string,
): Promise<ObjectValue> {
  const owner = readObject(await execute(["get", resource(kind), name(target), "-n", name(namespace), "-o", "json"]));
  const ownerMeta = metadata(owner);
  const status = object(owner.status, "Task/Team has no controller status yet");
  const binding = kind === "task" ? object(status.inferenceBudget, "Task has no governed-inference account binding") : undefined;
  const reference = object(binding?.account ?? status.inferenceBudgetAccount, "No lifetime account has been bound");
  if (kind === "task" && binding?.taskUid !== ownerMeta.uid) throw new Error("Task UID binding is stale");
  const account = readObject(await execute(["get", "karsbudgetaccount", name(reference.name),
    "-n", name(reference.namespace), "-o", "json"]));
  if (metadata(account).uid !== reference.uid) throw new Error("Budget account was replaced; refusing to report a fresh zero balance");
  const spec = object(account.spec, "Account spec is missing");
  const root = object(spec.root, "Account root is missing");
  const identity = object(root.resource, "Account root identity is missing");
  if (identity.namespace !== namespace || spec.scope !== scope) throw new Error("Budget scope/workspace mismatch");
  if (kind === "team" && (root.kind !== "KarsTeam" || identity.uid !== ownerMeta.uid || identity.name !== ownerMeta.name)) {
    throw new Error("Team lifetime UID binding is stale");
  }
  if (binding && JSON.stringify(binding.root) !== JSON.stringify(root)) {
    // JSON object ordering is not authority; compare exact identity fields.
    const pinned = object(binding.root, "Task root binding is missing");
    const pinnedIdentity = object(pinned.resource, "Task root identity is missing");
    if (["kind", "workspaceUid", "clusterUid"].some((key) => pinned[key] !== root[key])
      || ["namespace", "name", "uid"].some((key) => pinnedIdentity[key] !== identity[key])) {
      throw new Error("Task/account root UID binding differs");
    }
  }
  const accountStatus = object(account.status, "Budget account is uninitialized");
  const ledger = object(accountStatus.ledger, "Budget ledger is missing; no zero fallback");
  if (ledger.accountUid !== reference.uid || ledger.scope !== scope || ledger.version !== "governed-inference/v1") {
    throw new Error("Ledger version or identity differs");
  }
  const totals = object(ledger.meters, "Budget meters are missing");
  const meters: ObjectValue = {};
  for (const group of ["reserved", "settled", "uncertain"]) {
    const values = object(totals[group], "Budget meters are incomplete");
    meters[group] = { tokens: String(amount(values.tokens)), usdMicros: String(amount(values.usdMicros)) };
  }
  const unpriced = amount(totals.unpricedAttempts);
  const pendingUnpriced = Object.values(object(ledger.attempts, "Budget attempt ledger is missing"))
    .some((entry) => {
      const attempt = object(entry, "Malformed budget attempt");
      return ["Reserved", "InFlight"].includes(String(attempt.phase))
        && object(attempt.quote, "Budget quote is missing").priceCovered === false;
    });
  return {
    scope, root, account: reference, accountPhase: ledger.phase,
    taskOrTeamPhase: status.phase, limits: ledger.limits, meters,
    unpricedAttempts: String(unpriced),
    priceCoverage: unpriced > 0 || pendingUnpriced ? "incomplete — monetary cost is unknown" : "configured maxima only — not an invoice",
    meaning: "Governed inference tokens and configured maximum prices only; compute, tools, storage and invoice costs are excluded",
    dispatch: "Every provider send still requires live router identity, full task authorization, valid bounds/tariffs and an atomic broker grant",
  };
}

async function kubectl(args: string[], input?: string): Promise<string> {
  const { execa } = await import("execa");
  try {
    return (await execa("kubectl", [...args, "--request-timeout=20s"], {
      input, stdio: ["pipe", "pipe", "pipe"],
    })).stdout;
  } catch {
    throw new Error("Kubernetes budget operation failed; check context, API readiness and operator RBAC");
  }
}

export function budgetCommand(): Command {
  const command = new Command("budget")
    .description("Governed inference token/configured-maximum-price accounts (not total task spend)");
  command.command("create")
    .description("Opt in a NEW Task/Team manifest; controller configuration and launch gates still apply")
    .requiredOption("-f, --file <path>", "One KarsTask or KarsTeam YAML/JSON manifest")
    .option("-n, --namespace <workspace>", "Task/Team workspace", "kars-system")
    .action(async (options: { file: string; namespace: string }) => {
      const plan = governedPlan(await readFile(options.file, "utf8"), options.namespace);
      await kubectl(["create", "-f", "-", "-o", "name"], JSON.stringify(plan));
      console.log("Governed-inference plan created. This is not an enforcement/readiness assertion; inspect controller status before launch.");
    });
  command.command("status <name>")
    .description("Read the pinned durable account; never substitute absent accounting with zero")
    .option("--kind <task|team>", "Resource owning the binding", "task")
    .option("-n, --namespace <workspace>", "Task/Team workspace", "kars-system")
    .action(async (target: string, options: { kind: string; namespace: string }) => {
      console.log(JSON.stringify(await budgetStatus(kubectl, options.kind, target, options.namespace), null, 2));
    });
  return command;
}
