// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash, X509Certificate } from "node:crypto";
import { isIP } from "node:net";
import { at, canonical, digest, read, readSecretMetadata, record, reviewed, type Execute } from "./private-activation.js";

export interface ControlMemoryMaterial {
  configuration: string;
  key: string;
  anchor: string;
  epoch: string;
  binding: string;
  version: string;
}
const failure = "Late private memory configuration or bound-token anchor does not match its reviewed runtime";
export class ControlMemoryPending extends Error {
  constructor() { super("Controller has not reissued memory authority for the current control epoch"); }
}
const sha256 = (value: string | Buffer) => createHash("sha256").update(value).digest("hex");

/** The controller owns issuance; retirement must also prove the old bound anchor is gone. */
export async function controlMemoryMaterial(
  execute: Execute, encoded: unknown, control: Buffer, data: Record<string, unknown>, secretUid: string,
  runtime: { workspace: string; sandbox: { name: string; uid: string }; task?: { object: { name: string; uid: string } } },
): Promise<ControlMemoryMaterial> {
  try {
    if (typeof encoded !== "string" || encoded.length > 131_072) throw new Error(failure);
    const bytes = Buffer.from(encoded, "base64");
    if (bytes.toString("base64") !== encoded) throw new Error(failure);
    const config = record(JSON.parse(bytes.toString("utf8")));
    const keys = ["identity", "map_uid", "anchor_uid", "epoch", "api_url", "ca", "token", "expires_at", "sandbox_uid"];
    if (canonical(Object.keys(config).sort()) !== canonical(keys.sort())) throw new Error(failure);
    const workspace = reviewed(await read(execute, "namespace", runtime.workspace));
    const owner = runtime.task?.object ?? runtime.sandbox;
    const identity = { namespace: runtime.workspace, namespace_uid: workspace.uid,
      owner_kind: runtime.task ? "KarsTask" : "KarsSandbox", owner_name: owner.name, owner_uid: owner.uid, principal: "principal" };
    if (canonical(config.identity) !== canonical(identity) || config.sandbox_uid !== runtime.sandbox.uid
      || typeof config.map_uid !== "string" || !config.map_uid || typeof config.anchor_uid !== "string" || !config.anchor_uid
      || typeof config.epoch !== "string" || !/^[a-f0-9]{64}$/.test(config.epoch)
      || typeof config.token !== "string" || config.token.length > 65_536
      || typeof config.expires_at !== "number" || !Number.isSafeInteger(config.expires_at)
      || config.expires_at * 1000 <= Date.now() || typeof config.api_url !== "string" || typeof config.ca !== "string") throw new Error(failure);
    const url = new URL(config.api_url);
    if (url.protocol !== "https:" || url.username || url.password || url.search || url.hash || url.pathname !== "/"
      || !isIP(url.hostname.replace(/^\[|\]$/g, ""))) throw new Error(failure);
    if (!/^-----BEGIN CERTIFICATE-----\r?\n[A-Za-z0-9+/=\r\n]+\r?\n-----END CERTIFICATE-----\s*$/.test(config.ca)) throw new Error(failure);
    const ca = new X509Certificate(config.ca);
    if (!ca.ca || Date.parse(ca.validFrom) > Date.now() || Date.parse(ca.validTo) <= Date.now()) throw new Error(failure);
    if (config.epoch !== sha256(control)) throw new ControlMemoryPending();
    const account = `kars-memory-${sha256(runtime.sandbox.uid).slice(0, 40)}`;
    const anchor = await readSecretMetadata(execute, account, "kars-memory-system");
    const sa = await read(execute, "serviceaccount", account, "kars-memory-system");
    const saIdentity = reviewed(sa);
    const system = await read(execute, "namespace", "kars-memory-system");
    const systemIdentity = reviewed(system);
    const anchorIdentity = reviewed({ metadata: anchor });
    const jwt = config.token.split(".");
    if (jwt.length !== 3 || jwt.some(part => !/^[A-Za-z0-9_-]+$/.test(part))) throw new Error(failure);
    // Consistency only, not JWT authentication: Kubernetes issued the controller-owned
    // config. Revocation is proven by replacement of its live bound Secret UID.
    const claims = record(JSON.parse(Buffer.from(jwt[1]!, "base64url").toString("utf8")));
    if (anchorIdentity.name !== account || anchorIdentity.uid !== config.anchor_uid || at(anchor, "namespace") !== "kars-memory-system"
      || canonical(at(anchor, "ownerReferences")) !== canonical([{ apiVersion: "v1", kind: "ServiceAccount", name: account,
        uid: saIdentity.uid, controller: true, blockOwnerDeletion: false }])
      || saIdentity.name !== account || at(sa, "metadata", "namespace") !== "kars-memory-system"
      || at(sa, "metadata", "annotations", "kars.azure.com/memory-sandbox-uid") !== runtime.sandbox.uid
      || sa.automountServiceAccountToken !== false
      || at(system, "metadata", "labels", "kars.azure.com/component") !== "private-memory"
      || canonical(at(sa, "metadata", "ownerReferences")) !== canonical([{ apiVersion: "v1", kind: "Namespace",
        name: "kars-memory-system", uid: systemIdentity.uid, controller: true, blockOwnerDeletion: false }])
      || at(claims, "kubernetes.io", "namespace") !== "kars-memory-system"
      || at(claims, "kubernetes.io", "secret", "name") !== account || at(claims, "kubernetes.io", "secret", "uid") !== config.anchor_uid
      || at(claims, "kubernetes.io", "serviceaccount", "name") !== account || at(claims, "kubernetes.io", "serviceaccount", "uid") !== saIdentity.uid
      || claims.sub !== `system:serviceaccount:kars-memory-system:${account}`
      || typeof claims.exp !== "number" || claims.exp * 1000 <= Date.now() || claims.exp < config.expires_at) throw new Error(failure);
    const { "builtin-memory.json": _memory, ...controlData } = data;
    return { configuration: digest(encoded), key: digest(config.token), anchor: anchorIdentity.uid,
      epoch: config.epoch, binding: digest({ identity, map: config.map_uid, api: config.api_url, ca: config.ca, sandbox: config.sandbox_uid }),
      version: `${secretUid}:control-v1:${digest(controlData)}` };
  } catch (error) {
    if (error instanceof ControlMemoryPending) throw error;
    // Parser and Kubernetes errors must not copy credentials or arbitrary response bodies.
    throw new Error(failure);
  }
}
