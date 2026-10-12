// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { RequiredMeshAuth as SharedMeshAuth, meshAuthRequest as request, type AuthHooks, type AuthTransport } from "@kars/mesh/dist/required-mesh-auth.js";
import { routerUrl } from "./router-client.js";
export { validateMeshVerification } from "@kars/mesh/dist/required-mesh-auth.js";

export function requiredMeshAuth(env: NodeJS.ProcessEnv = process.env): boolean {
  const required = env.MESH_AUTH_BACKEND === "EntraAgentIdentity";
  if (required && [env.AGT_SKIP_INIT, env.AGT_SKIP_ENTRA, env.AGT_SKIP_VERIFY].includes("1")) {
    throw new Error("Required Entra authentication conflicts with AGT skip flags");
  }
  return required;
}

export async function meshAuthRequest(method: string, path: string, body?: unknown, signal?: AbortSignal): Promise<any> {
  return request(method, routerUrl(path), body, signal);
}

export class RequiredMeshAuth<T extends AuthTransport> extends SharedMeshAuth<T> {
  constructor(raw: T, hooks: Omit<AuthHooks, "request"> & { request?: typeof meshAuthRequest }) {
    super(raw, { ...hooks, request: hooks.request ?? meshAuthRequest });
  }
}
