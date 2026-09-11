"use server";

import { revalidatePath } from "next/cache";
import {
  BffError,
  createLocalModelDeployment,
  deleteLocalModelDeployment,
  getLocalDeploymentLiveStatus,
  type DeployActivity,
} from "@/lib/bff";

export interface LocalInferenceState { error: string | null; ok: string | null }

export async function deployLocalModelAction(_p: LocalInferenceState, form: FormData): Promise<LocalInferenceState> {
  const name = String(form.get("name") ?? "").trim();
  const modelId = String(form.get("model_id") ?? "").trim();
  const tier = String(form.get("tier") ?? "cpu").trim();
  const image = String(form.get("image") ?? "").trim();
  if (!name) return { error: "A deployment name is required (lowercase letters, digits, hyphens).", ok: null };
  if (!modelId) return { error: "Pick a model or enter a HuggingFace model id.", ok: null };
  if (tier !== "cpu" && tier !== "gpu") return { error: "Tier must be cpu or gpu.", ok: null };
  try {
    await createLocalModelDeployment({
      name,
      model_id: modelId,
      tier: tier as "cpu" | "gpu",
      image: image || undefined,
    });
    revalidatePath("/console/configuration");
    return { error: null, ok: `Deploying ${modelId} as "${name}" — this can take a minute on first pull.` };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "deploy failed", ok: null };
  }
}

/** Kick off a local model deployment and return its name so the client can
 *  then poll {@link pollLocalDeploymentAction} for live progress. Unlike the
 *  fire-and-forget form action above, this drives a tracked progress UI. */
export async function startLocalDeployAction(input: {
  name: string;
  modelId: string;
  tier: "cpu" | "gpu";
  image?: string;
}): Promise<{ error: string | null; name: string | null }> {
  const name = input.name.trim();
  const modelId = input.modelId.trim();
  if (!name) return { error: "A deployment name is required (lowercase letters, digits, hyphens).", name: null };
  if (!modelId) return { error: "Pick a model or enter a HuggingFace model id.", name: null };
  if (input.tier !== "cpu" && input.tier !== "gpu") return { error: "Tier must be cpu or gpu.", name: null };
  try {
    await createLocalModelDeployment({
      name,
      model_id: modelId,
      tier: input.tier,
      image: input.image?.trim() || undefined,
    });
    return { error: null, name };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "deploy failed", name: null };
  }
}

export interface LocalDeployProgress {
  found: boolean;
  /** Raw CR phase: Pending | Deploying | Running | Failed | … (null before it appears). */
  phase: string | null;
  message: string | null;
  /** Milestone-derived percentage (0-100) from real cluster signals. */
  percent: number;
  /** True once Running — the model is now registered as a provider + in the catalogue. */
  ready: boolean;
  /** True on a terminal failure (ImagePullBackOff, CrashLoopBackOff, …). */
  failed: boolean;
  failureReason: string | null;
  failureMessage: string | null;
  /** Real Kubernetes events for this deployment's pods — the live activity feed. */
  activities: DeployActivity[];
  error: string | null;
}

/** Poll one local deployment's rich LIVE status by name: percentage + real
 *  pod/container state + the actual Kubernetes event stream (image pull,
 *  scheduling, container start/fail). This is what drives the deploy
 *  tracker's progress bar and activity feed. Also refreshes the page once
 *  Running so the model lands in the catalogue. */
export async function pollLocalDeploymentAction(name: string): Promise<LocalDeployProgress> {
  try {
    const s = await getLocalDeploymentLiveStatus(name);
    if (s.ready) revalidatePath("/console/configuration");
    return {
      found: s.found,
      phase: s.phase,
      message: s.message,
      percent: s.percent,
      ready: s.ready,
      failed: s.failed,
      failureReason: s.failure_reason,
      failureMessage: s.failure_message,
      activities: s.activities,
      error: null,
    };
  } catch (e) {
    return {
      found: false, phase: null, message: null, percent: 0, ready: false, failed: false,
      failureReason: null, failureMessage: null, activities: [],
      error: e instanceof BffError ? e.message || e.code : "status check failed",
    };
  }
}

export async function undeployLocalModelAction(_p: LocalInferenceState, form: FormData): Promise<LocalInferenceState> {
  const name = String(form.get("name") ?? "").trim();
  if (!name) return { error: "Missing name.", ok: null };
  try {
    await deleteLocalModelDeployment(name);
    revalidatePath("/console/configuration");
    return { error: null, ok: `${name} removed.` };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "remove failed", ok: null };
  }
}
