// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// up/foundry_setup.ts — make a BYO ("--foundry-endpoint") Foundry project
// actually usable by kars, instead of assuming it's pre-configured.
//
// What this does (all idempotent, all read-mostly except the two explicit
// provisioning steps which are gated + best-effort):
//   1. Resolve the AI Services account + project from the endpoint URL.
//   2. List the project's deployed models (ARM control-plane — works with the
//      caller's existing `az login`, no Microsoft Graph).
//   3. Pick the BEST deployed chat model for the agent (so we stop hardcoding a
//      stale gpt-4.1). The user's explicit `--model` always wins.
//   4. Ensure an embedding model is deployed (Foundry Memory Store needs one);
//      best-effort deploy `text-embedding-3-small` if none exists.
//   5. Enable the project's system-assigned managed identity if it's missing
//      (Memory Store authenticates internally as the PROJECT MI) and re-read its
//      principalId so the caller can grant it `Azure AI User` on the RG.
//
// Nothing here aborts the deploy: every failure degrades to a clear note so the
// sandbox still comes up and the operator gets actionable remediation.

import type { Stepper } from "../../stepper.js";

/** One deployed model on the Foundry/AI-Services account (ARM shape). */
export interface FoundryDeployment {
  /** Deployment name — what you put in the request `model` field. */
  name: string;
  /** Underlying model name (e.g. "gpt-5.4"). */
  modelName: string;
  /** Model version. */
  modelVersion: string;
}

export interface FoundrySetupResult {
  accountName: string;
  accountResourceId: string;
  resourceGroup: string;
  projectName: string;
  /** Best deployed chat model deployment name, or undefined if none found. */
  bestChatModel?: string;
  /** Embedding deployment name in use (existing or just-created), or undefined. */
  embeddingModel?: string;
  /** Project system-assigned MI principalId (after any enable), or "". */
  projectMiPrincipalId: string;
  /** True if this run enabled the MI (was previously off). */
  miJustEnabled: boolean;
  /** Human-readable status notes for the deployment report. */
  notes: string[];
}

/**
 * Score a deployed model for use as an interactive, tool-using agent's chat
 * model. Returns a number (higher = better) or `null` when the model is not a
 * chat model (embeddings, image, audio, …) and must be excluded.
 *
 * Ranking: family/version dominates; within a family the plain flagship beats
 * `-pro`/`-chat`/`-mini`/`-nano`, because for a tool-calling agent the flagship
 * general model is the most reliable default (reasoning-`pro` variants are
 * slower/pricier and `mini`/`nano` are weaker). `--model` overrides all of this.
 */
export function scoreChatModel(modelName: string): number | null {
  const n = modelName.toLowerCase();

  // Hard-exclude anything that isn't a text chat model.
  const NON_CHAT =
    /(embedding|image|dall-?e|flux|whisper|tts|audio|realtime|sora|moderation|rerank|transcrib|stable-?diffusion)/;
  if (NON_CHAT.test(n)) return null;

  // Family/version score.
  let family: number;
  const gpt = n.match(/^gpt-(\d+)(?:\.(\d+))?/);
  const oSeries = n.match(/^o(\d+)/);
  if (gpt) {
    const major = parseInt(gpt[1], 10);
    const minor = gpt[2] ? parseInt(gpt[2], 10) : 0;
    family = major * 100 + minor; // gpt-5.4 → 504, gpt-4.1 → 401, gpt-4o → 400
  } else if (oSeries) {
    family = 300 + parseInt(oSeries[1], 10) * 10; // o3 → 330, o4 → 340 (below gpt-5)
  } else {
    family = 50; // unknown family — keep, but rank low.
  }

  // Variant adjustment (plain flagship preferred for agent tool-use).
  let variant: number;
  if (/-pro\b/.test(n)) variant = 3;
  else if (/-chat\b/.test(n)) variant = 2;
  else if (/-mini\b/.test(n)) variant = 1;
  else if (/-nano\b/.test(n)) variant = 0;
  else variant = 4; // plain flagship

  return family * 10 + variant;
}

/** Pick the best chat-capable deployment, or undefined if none qualify. */
export function pickBestChatModel(
  deployments: FoundryDeployment[],
): FoundryDeployment | undefined {
  let best: { dep: FoundryDeployment; score: number } | undefined;
  for (const dep of deployments) {
    const score = scoreChatModel(dep.modelName) ?? scoreChatModel(dep.name);
    if (score === null || score === undefined) continue;
    if (!best || score > best.score) best = { dep, score };
  }
  return best?.dep;
}

/** Find an embedding deployment, preferring 3-large > 3-small > ada. */
export function findEmbeddingModel(
  deployments: FoundryDeployment[],
): FoundryDeployment | undefined {
  const embeds = deployments.filter((d) =>
    /embedding/i.test(d.modelName) || /embedding/i.test(d.name),
  );
  if (embeds.length === 0) return undefined;
  const rank = (d: FoundryDeployment): number => {
    const n = `${d.modelName} ${d.name}`.toLowerCase();
    if (n.includes("3-large")) return 3;
    if (n.includes("3-small")) return 2;
    if (n.includes("ada")) return 1;
    return 0;
  };
  return embeds.sort((a, b) => rank(b) - rank(a))[0];
}

/** Parse "https://<acct>.services.ai.azure.com/api/projects/<proj>" → parts. */
export function parseFoundryEndpoint(
  endpoint: string,
): { accountName: string; projectName: string } | null {
  try {
    const u = new URL(endpoint);
    const accountName = u.hostname.split(".")[0];
    const m = u.pathname.match(/\/api\/projects\/([^/]+)/);
    if (!accountName || !m) return null;
    return { accountName, projectName: m[1] };
  } catch {
    return null;
  }
}

type Execa = typeof import("execa").execa;

/**
 * Discover + (best-effort) provision the BYO Foundry project so kars Memory
 * Store and the agent model "just work". Returns null when the endpoint isn't a
 * Foundry project endpoint (e.g. plain Azure OpenAI) — the caller keeps its
 * existing behaviour in that case.
 */
export async function setupFoundryForKars(args: {
  execa: Execa;
  stepper: Stepper;
  foundryEndpoint: string;
}): Promise<FoundrySetupResult | null> {
  const { execa, stepper, foundryEndpoint } = args;
  const parsed = parseFoundryEndpoint(foundryEndpoint);
  if (!parsed) return null;
  const { accountName, projectName } = parsed;
  const notes: string[] = [];

  // 1. Resolve the account ARM id + resource group.
  stepper.update("Discovering Foundry project...");
  const { stdout: acctJson } = await execa("az", [
    "cognitiveservices", "account", "list",
    "--query", `[?name=='${accountName}'].{id:id, rg:resourceGroup} | [0]`,
    "--output", "json",
  ], { stdio: "pipe" }).catch(() => ({ stdout: "{}" }));
  const acct = JSON.parse((acctJson || "{}").trim() || "{}");
  const accountResourceId: string = acct.id || "";
  const resourceGroup: string = acct.rg || "";
  if (!accountResourceId || !resourceGroup) {
    notes.push(
      `Could not resolve the Foundry account '${accountName}' in this subscription — ` +
        "skipping Foundry auto-setup (the sandbox will still deploy).",
    );
    return {
      accountName, accountResourceId: "", resourceGroup: "", projectName,
      projectMiPrincipalId: "", miJustEnabled: false, notes,
    };
  }

  // 2. List deployed models (ARM control-plane).
  let deployments: FoundryDeployment[] = [];
  try {
    const { stdout: depJson } = await execa("az", [
      "rest", "--method", "get",
      "--url", `${accountResourceId}/deployments?api-version=2024-10-01`,
    ], { stdio: "pipe" });
    const raw = JSON.parse(depJson.trim());
    deployments = (raw.value ?? []).map((d: {
      name: string;
      properties?: { model?: { name?: string; version?: string } };
    }) => ({
      name: d.name,
      modelName: d.properties?.model?.name ?? d.name,
      modelVersion: d.properties?.model?.version ?? "",
    }));
  } catch {
    notes.push("Could not list Foundry model deployments (continuing with defaults).");
  }

  // 3. Best chat model.
  const best = pickBestChatModel(deployments);
  const bestChatModel = best?.name;
  if (bestChatModel) {
    stepper.detail("info", `Best deployed chat model: ${bestChatModel}`);
  }

  // 4. Ensure an embedding model (Memory Store needs one).
  let embeddingModel = findEmbeddingModel(deployments)?.name;
  if (!embeddingModel && accountResourceId) {
    stepper.update("No embedding model deployed — deploying text-embedding-3-small...");
    const ok = await execa("az", [
      "cognitiveservices", "account", "deployment", "create",
      "--name", accountName,
      "--resource-group", resourceGroup,
      "--deployment-name", "text-embedding-3-small",
      "--model-name", "text-embedding-3-small",
      "--model-version", "1",
      "--model-format", "OpenAI",
      "--sku-name", "Standard",
      "--sku-capacity", "50",
      "--output", "none",
    ], { stdio: "pipe" }).then(() => true).catch(() => false);
    if (ok) {
      embeddingModel = "text-embedding-3-small";
      notes.push("Deployed embedding model 'text-embedding-3-small' for Memory Store.");
    } else {
      notes.push(
        "No embedding model is deployed and auto-deploy failed (quota/permissions?). " +
          "Memory Store needs one — deploy 'text-embedding-3-small' in the Foundry portal.",
      );
    }
  }

  // 5. Ensure the project's system-assigned MI (Memory Store authenticates
  //    internally as the PROJECT MI).
  const projectUrl = `${accountResourceId}/projects/${projectName}?api-version=2025-06-01`;
  let projectMiPrincipalId = "";
  let miJustEnabled = false;
  try {
    const { stdout: projJson } = await execa("az", [
      "rest", "--method", "get", "--url", projectUrl,
    ], { stdio: "pipe" });
    projectMiPrincipalId = JSON.parse(projJson.trim())?.identity?.principalId || "";
  } catch {
    // Fall through to enable attempt.
  }

  if (!projectMiPrincipalId) {
    stepper.update("Enabling Foundry project managed identity (for Memory Store)...");
    const enabled = await execa("az", [
      "rest", "--method", "patch", "--url", projectUrl,
      "--body", JSON.stringify({ identity: { type: "SystemAssigned" } }),
    ], { stdio: "pipe" }).then(() => true).catch(() => false);

    if (enabled) {
      // The principalId may take a few seconds to populate after enabling.
      for (let i = 0; i < 6 && !projectMiPrincipalId; i++) {
        await new Promise((r) => setTimeout(r, 3000));
        const { stdout: pj } = await execa("az", [
          "rest", "--method", "get", "--url", projectUrl,
        ], { stdio: "pipe" }).catch(() => ({ stdout: "{}" }));
        projectMiPrincipalId = JSON.parse((pj || "{}").trim() || "{}")?.identity?.principalId || "";
      }
      if (projectMiPrincipalId) {
        miJustEnabled = true;
        notes.push("Enabled the Foundry project's system-assigned managed identity.");
      } else {
        notes.push(
          "Enabled the Foundry project MI but its principalId hasn't populated yet — " +
            "Memory Store RBAC will be granted on the next `kars up` run.",
        );
      }
    } else {
      notes.push(
        "Foundry project has no system-assigned MI and kars couldn't enable it " +
          "(needs Contributor on the project). Enable it: Portal → Project → " +
          "Resource Management → Identity → System assigned → On, then re-run `kars up`.",
      );
    }
  }

  return {
    accountName,
    accountResourceId,
    resourceGroup,
    projectName,
    bestChatModel,
    embeddingModel,
    projectMiPrincipalId,
    miJustEnabled,
    notes,
  };
}
