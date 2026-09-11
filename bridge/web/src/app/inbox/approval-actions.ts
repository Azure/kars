// kars Bridge — approval decision server action (the steering primitive).
"use server";

import { defaultNamespace } from "@/lib/config";
import { decideApproval } from "@/lib/bff";

export async function decide(
  name: string,
  verdict: "approve" | "deny",
  resourceVersion: string,
  boundEnvelopeDigest: string | null,
  reason?: string,
): Promise<{ error: string | null }> {
  const ns = defaultNamespace();
  try {
    await decideApproval(ns, name, {
      verdict,
      reason: reason || undefined,
      resource_version: resourceVersion,
      bound_envelope_digest: boundEnvelopeDigest,
    });
  } catch (err) {
    return { error: err instanceof Error ? err.message : "unknown error" };
  }
  return { error: null };
}
