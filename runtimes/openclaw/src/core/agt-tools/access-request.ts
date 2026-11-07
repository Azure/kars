// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { routerCall } from "../router-client.js";

export const ACCESS_DESCRIPTION = "Ask a human when stuck: request kind=clarification, target=a short question identifier, reason=the question; waits for an answer by default. Request egress/tool/skill/mcp/command/permission or tier (1-5) for review, or list/wait/cancel an existing request. Preserve request_id and scope_id when waiting again after timeout. Approval is a decision ONLY, never an installed permission or autonomy change. For egress use http_fetch with wait_for_approval_ms to wait for actual policy. Clarification answers are human input, not permission to bypass reviewed capabilities.";
export const ACCESS_PARAMETERS = {
  type: "object", additionalProperties: false,
  properties: {
    operation: { type: "string", enum: ["request", "list", "wait", "cancel"] },
    kind: { type: "string", enum: ["clarification", "egress", "tool", "skill", "mcp", "command", "permission", "tier"] },
    target: { type: "string", description: "Exact resource or short question identifier; omit for tier" },
    reason: { type: "string", description: "Question or justification, at most 512 UTF-8 bytes" },
    tier: { type: "integer", minimum: 1, maximum: 5, description: "Used only for kind=tier" },
    port: { type: "integer", minimum: 1, maximum: 65535, description: "Used only for kind=egress" },
    scope_id: { type: "string", description: "Original request scope; required for wait/cancel. Omit or leave empty for a new request/list in the active scope." },
    request_id: { type: "string" },
    timeout_ms: { type: "integer", minimum: 0, maximum: 300000,
      description: "Request: 0 queues only; default 300000 waits. Wait: 1..300000. A timeout leaves the request pending." },
  }, required: ["operation"],
};

export function accessArgumentError(args: Record<string, unknown>): string | undefined {
  if (Object.keys(args).some(key => !(key in ACCESS_PARAMETERS.properties))
    || !["request", "list", "wait", "cancel"].includes(args.operation as string)
    || (args.timeout_ms !== undefined && (!Number.isSafeInteger(args.timeout_ms) || (args.timeout_ms as number) < 0 || (args.timeout_ms as number) > 300000))) return "Invalid access request operation or timeout";
  if (args.scope_id !== undefined && (typeof args.scope_id !== "string" || !/^[A-Za-z0-9._:-]{0,128}$/.test(args.scope_id))) return "Invalid access request scope_id";
  if (["wait", "cancel"].includes(args.operation as string)
    && (![args.scope_id, args.request_id].every(value => typeof value === "string" && /^[A-Za-z0-9._:-]{1,128}$/.test(value))
      || (args.operation === "wait" && args.timeout_ms === 0))) return "Wait/cancel requires the original scope_id and request_id";
  if (args.operation === "request" && (!ACCESS_PARAMETERS.properties.kind.enum.includes(args.kind as string)
    || typeof args.reason !== "string" || !args.reason.trim() || Buffer.byteLength(args.reason) > 512)) return "Request requires a kind and nonblank question/reason of at most 512 UTF-8 bytes";
  return undefined;
}

type Exchange = typeof routerCall;
export async function accessRequest(args: Record<string, unknown>, options: {
  scope?: string; signal?: AbortSignal; deadline?: number; exchange?: Exchange;
} = {}): Promise<Record<string, any>> {
  const error = accessArgumentError(args);
  if (error) throw new Error(error);
  const exchange = options.exchange ?? routerCall;
  const suppliedScope = args.scope_id === "" && ["request", "list"].includes(args.operation as string)
    ? undefined : args.scope_id as string | undefined;
  let scope = options.scope ?? suppliedScope;
  if (options.scope && suppliedScope !== undefined && suppliedScope !== options.scope) throw new Error("stale_scope");
  const call = (method: string, path: string, body?: unknown, timeout = 15000) => {
    options.signal?.throwIfAborted();
    if (options.deadline !== undefined && Date.now() >= options.deadline) throw new Error("Active assignment deadline elapsed");
    return exchange(method, path, body, timeout, scope ? { "x-kars-service-scope": scope } : undefined,
      options.signal, { maxBytes: 4 * 1024 * 1024, structuredErrors: true });
  };
  if (args.operation === "list" || !scope) {
    const listed = await call("GET", "/v1/access-requests");
    if (listed?.error || args.operation === "list") return listed;
    scope = listed?.scope?.id;
    if (typeof scope !== "string" || !scope) throw new Error("Router scope unavailable");
  }
  let entry: Record<string, any> | undefined;
  if (args.operation === "request") {
    const fields = ["kind", "target", "reason", ...(args.kind === "tier" ? ["tier"] : args.kind === "egress" ? ["port"] : [])];
    const body = Object.fromEntries(fields.filter(key => args[key] !== undefined).map(key => [key, args[key]]));
    const queued = await call("POST", "/v1/access-request", { ...body, scope_id: scope });
    if (queued?.error) return queued;
    entry = queued?.request;
    if (!entry || entry.scope_id !== scope || typeof entry.request_id !== "string") throw new Error("Invalid scoped request receipt");
    if (args.timeout_ms === 0) return { ...queued, enforcement_changed: false };
  }
  const id = entry?.request_id ?? args.request_id;
  const path = `/v1/access-requests/${encodeURIComponent(id as string)}`;
  if (args.operation === "cancel") {
    let result;
    try { result = await call("POST", `${path}/cancel`, { scope_id: scope }); }
    catch (error) { options.signal?.throwIfAborted(); result = { error: String(error) }; }
    if (!result?.error && (result?.scope_id !== scope || result?.request_id !== id)) throw new Error("Invalid scoped cancellation receipt");
    return { ...(result?.error ? result : { request: result }), request_id: id, scope_id: scope, enforcement_changed: false };
  }
  const remaining = options.deadline === undefined ? 300000 : Math.max(1, options.deadline - Date.now() - 1000);
  const timeout = Math.min(args.timeout_ms as number ?? 300000, remaining);
  let decision;
  try {
    decision = await call("GET", `${path}/wait?scope_id=${encodeURIComponent(scope!)}&timeout_ms=${timeout}`, undefined, timeout + 5000);
  } catch (error) {
    options.signal?.throwIfAborted();
    return { error: String(error), request_id: id, scope_id: scope, ...(entry ? { request: entry } : {}), enforcement_changed: false };
  }
  if (decision?.error) return { ...decision, request_id: id, scope_id: scope, ...(entry ? { request: entry } : {}), enforcement_changed: false };
  if (decision?.scope_id !== scope || decision?.request_id !== id) throw new Error("Invalid scoped decision receipt");
  if (decision.kind === "clarification" && decision.status === "approved"
    && (typeof decision.answer !== "string" || !decision.answer.trim() || Buffer.byteLength(decision.answer) > 8192)) {
    return { error: "Approved clarification has no valid answer", request: decision, enforcement_changed: false };
  }
  return { request: decision, enforcement_changed: false };
}

export function registerAccessRequestTool(api: { registerTool: (tool: any) => void }): void {
  api.registerTool({ name: "kars_access_request", label: "Human clarification and access review", description: ACCESS_DESCRIPTION,
    parameters: ACCESS_PARAMETERS, async execute(_id: string, args: Record<string, unknown>, signal?: AbortSignal) {
      try {
        const result = await accessRequest(args, { signal });
        return { content: [{ type: "text", text: JSON.stringify(result) }], isError: Boolean(result.error) };
      } catch (error) { return { content: [{ type: "text", text: JSON.stringify({ error: String(error) }) }], isError: true }; }
    } });
}
