// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { meshAuthRequest, validateMeshVerification, type RequiredMeshAuth } from "./required-mesh-auth.js";

export const dispatcherEntraName = "kars-mission-dispatcher-entra";
export const dispatcherEntraPin = "kars.azure.com/mission-entra-uid";
export const guid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
export interface DispatcherEntraConfig { tenantId: string; blueprintId: string }
export interface DispatcherEntraBinding extends DispatcherEntraConfig {
  version: "v1";
  state: "ready";
  namespaceUid: string;
  deploymentUid: string;
  rootUid: string;
  journalUid: string;
  appId: string;
  objectId: string;
}

export function verifyDispatcherIdentity(result: any, did: string, expectedDid: string, binding: DispatcherEntraBinding): void {
  validateMeshVerification(result, did);
  if (did !== expectedDid || result.verified_app_id !== binding.appId || result.verified_tenant_id !== binding.tenantId) {
    throw new Error("Registry did not verify the pinned dispatcher identity");
  }
}

/** Only the official SDK mints tokens; the registry, not decoded JWT claims, authenticates them. */
export function dispatcherAuthRequest(namespace: string, registryUrl: string, binding: DispatcherEntraBinding,
  request: typeof meshAuthRequest = meshAuthRequest): typeof meshAuthRequest {
  const sdk = `http://entra-auth-sidecar.${namespace}.svc:5000/AuthorizationHeaderUnauthenticated/AgentMesh?AgentIdentity=${binding.appId}`;
  return async (method, path, body, signal) => {
    if (method === "GET" && path === "/v1/mesh-token") {
      const result = await request("GET", sdk, undefined, signal);
      if (typeof result?.authorizationHeader !== "string" || !/^Bearer \S+$/i.test(result.authorizationHeader)
        || result.error || result.success === false) throw new Error("Invalid dispatcher SDK token response");
      // Match the router's conservative SDK lease. JWT exp may only shorten it.
      return { access_token: result.authorizationHeader.slice(7), token_type: "Bearer", expires_in: 50 * 60 };
    }
    if (method !== "POST" || path !== "/agt/registry/v1/registry/verify") throw new Error("Invalid dispatcher auth operation");
    return request("POST", `${registryUrl}/v1/registry/verify`, body, signal);
  };
}

export async function dispatcherSocketFactory(auth: () => RequiredMeshAuth<any>): Promise<(url: string) => unknown> {
  const { default: WebSocket } = await import("ws");
  return url => {
    const socket = new WebSocket(url);
    const send = socket.send.bind(socket);
    socket.send = (data: any, ...rest: any[]) => {
      let frame: any;
      try { if (typeof data === "string") frame = JSON.parse(data); }
      catch { /* Binary and non-JSON traffic is not a relay connect frame. */ }
      if (frame?.type === "connect") {
        frame.token = auth().connectToken();
        return send(JSON.stringify(frame), ...rest);
      }
      return send(data, ...rest);
    };
    return auth().bindSocketMessages(socket);
  };
}
