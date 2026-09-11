"use server";

// kars Bridge Operator Console — live model discovery, server-side. Client
// components must never import lib/bff.ts directly (it resolves the BFF via
// its in-cluster Service DNS name, unreachable from the browser) — this is
// the server-action bridge, callable directly as an async function (not just
// via a <form action>, since the discover button needs dynamic args).

import { discoverModels, BffError } from "@/lib/bff";
import type { DiscoveredModel } from "@/lib/types";

export type DiscoverResult = { models: DiscoveredModel[]; error: null } | { models: null; error: string };

export async function discoverModelsAction(input: {
  kind: string;
  endpoint?: string;
  key?: string;
}): Promise<DiscoverResult> {
  try {
    const models = await discoverModels(input);
    return { models, error: null };
  } catch (e) {
    return { models: null, error: e instanceof BffError ? e.message || e.code : "Discovery failed." };
  }
}
