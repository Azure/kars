// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createPrivateKey, X509Certificate } from "node:crypto";
import { at, canonical, digest, record, template } from "./private-activation.js";

export interface ControlTlsMaterial { configuration: string; key: string }

/** Validate the controller's existing Task control transport, never a caller-supplied endpoint. */
export function controlTlsMaterial(encoded: unknown, deployment: unknown, expectedIdentity: unknown): ControlTlsMaterial {
  const failure = "Late private control TLS configuration does not match its reviewed Task transport";
  try {
    if (typeof encoded !== "string" || encoded.length > 131_072) throw new Error(failure);
    const bytes = Buffer.from(encoded, "base64");
    if (bytes.toString("base64") !== encoded) throw new Error(failure);
    const configuration = record(JSON.parse(bytes.toString("utf8")));
    const keys = ["identity", "serverName", "caPem", "certificatePem", "privateKeyPem", "expiresAt"];
    if (canonical(Object.keys(configuration).sort()) !== canonical(keys.sort())) throw new Error(failure);
    const containers = at(template(deployment), "spec", "containers");
    const router = Array.isArray(containers) ? containers.filter(c => at(c, "name") === "inference-router") : [];
    const env = at(router[0], "env");
    const entries = Array.isArray(env) ? env.filter(e => at(e, "name") === "KARS_SERVICE_IDENTITY_JSON") : [];
    const identity = record(JSON.parse(String(at(entries[0], "value"))));
    const uid = at(identity, "sandbox", "uid");
    if (router.length !== 1 || entries.length !== 1 || identity.managed !== true || !identity.task
      || typeof uid !== "string" || !/^[a-zA-Z0-9-]{1,128}$/.test(uid)
      || canonical(configuration.identity) !== canonical(identity) || canonical(identity) !== canonical(expectedIdentity)
      || configuration.serverName !== `control-${uid}.kars.internal`) throw new Error(failure);
    for (const name of ["caPem", "certificatePem", "privateKeyPem"]) {
      if (typeof configuration[name] !== "string" || !configuration[name]) throw new Error(failure);
    }
    if (![configuration.caPem, configuration.certificatePem].every(pem =>
      /^-----BEGIN CERTIFICATE-----\s+[A-Za-z0-9+/=\s]+-----END CERTIFICATE-----\s*$/.test(pem as string))) throw new Error(failure);
    const ca = new X509Certificate(configuration.caPem as string);
    const leaf = new X509Certificate(configuration.certificatePem as string);
    const now = Date.now();
    if (!ca.ca || leaf.ca || !leaf.verify(ca.publicKey)
      || !leaf.checkPrivateKey(createPrivateKey(configuration.privateKeyPem as string))
      || leaf.checkHost(configuration.serverName as string, { subject: "never", wildcards: false }) !== configuration.serverName
      || ![ca, leaf].every(c => Date.parse(c.validFrom) <= now && now < Date.parse(c.validTo))
      || typeof configuration.expiresAt !== "number" || !Number.isSafeInteger(configuration.expiresAt)
      || configuration.expiresAt * 1000 <= now
      || configuration.expiresAt * 1000 > Math.min(Date.parse(ca.validTo), Date.parse(leaf.validTo))) throw new Error(failure);
    return { configuration: digest(bytes.toString("base64")),
      key: digest(leaf.publicKey.export({ type: "spki", format: "der" }).toString("base64")) };
  } catch {
    // Do not propagate parser errors containing Secret material into operator diagnostics.
    throw new Error(failure);
  }
}
