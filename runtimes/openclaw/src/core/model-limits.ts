// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { request } from "node:http";
import { routerUrl } from "./router-client.js";

const object = (value: unknown): Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value)
  ? value as Record<string, unknown> : {};

export async function modelLimits(model: string, signal: AbortSignal): Promise<{ contextWindow: number; contextTokens: number; maxTokens: number }> {
  return new Promise((resolve, reject) => {
    const req = request(routerUrl("/v1/models"), {
      method: "GET", signal: AbortSignal.any([signal, AbortSignal.timeout(15_000)]),
    }, res => {
      const chunks: Buffer[] = [];
      let size = 0;
      res.on("error", reject);
      res.on("data", (chunk: Buffer) => {
        size += chunk.length;
        if (size > 64 * 1024) { req.destroy(new Error("Native model metadata exceeds its response bound")); return; }
        chunks.push(chunk);
      });
      res.on("end", () => {
        try {
          if (res.statusCode !== 200) throw new Error("Native model metadata is unavailable");
          const data = object(JSON.parse(Buffer.concat(chunks).toString("utf8"))).data;
          if (!Array.isArray(data) || data.length !== 1 || object(data[0]).id !== model) {
            throw new Error("Native model metadata does not match the selected model");
          }
          const { context_window: contextWindow, max_output_tokens: maxTokens } = object(data[0]);
          if (!Number.isSafeInteger(contextWindow) || (contextWindow as number) <= 0
            || !Number.isSafeInteger(maxTokens) || (maxTokens as number) <= 0) {
            throw new Error("Native model metadata has no valid context/output limits");
          }
          resolve({ contextWindow: contextWindow as number, contextTokens: contextWindow as number, maxTokens: maxTokens as number });
        } catch (error) { reject(error instanceof SyntaxError ? new Error("Native model metadata is malformed") : error); }
      });
    });
    req.on("error", reject);
    req.end();
  });
}

/** The pinned gateway's prepared generation, not a per-run config clone, owns model capacity. */
export async function prepareNativeModelConfig(config: Record<string, unknown>, model: string, signal: AbortSignal): Promise<Record<string, unknown>> {
  const prepared = structuredClone(config);
  const provider = object(object(object(prepared.models).providers)["azure-openai"]);
  const models = provider.models;
  if (!Array.isArray(models) || models.filter(entry => object(entry).id === model).length !== 1) {
    throw new Error("Native model must have one configured azure-openai catalog entry");
  }
  const limits = await modelLimits(model, signal);
  provider.models = models.map(entry => object(entry).id === model ? { ...object(entry), ...limits } : entry);
  return prepared;
}
