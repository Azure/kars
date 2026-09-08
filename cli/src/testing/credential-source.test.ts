// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { parseAllDocuments } from "yaml";

const root = new URL("../../../", import.meta.url);
const source = (path: string) => readFileSync(new URL(path, root), "utf8");

describe("credential source public integration", () => {
  it("renders an optional UID-pinned schema with same-target and managed-runtime admission guards", () => {
    const yaml = execFileSync("helm", [
      "template", "kars", fileURLToPath(new URL("deploy/helm/kars", root)),
      "--show-only", "templates/crd.yaml",
    ], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 30_000 });
    const resource = parseAllDocuments(yaml).map(doc => {
      if (doc.errors.length) throw doc.errors[0];
      return doc.toJSON();
    }).find(doc => doc?.metadata?.name === "karssandboxes.kars.azure.com");
    const schema = resource.spec.versions[0].schema.openAPIV3Schema;
    const spec = schema.properties.spec;
    expect(spec.required).not.toContain("credentialsRef");
    expect(spec.required).not.toContain("upstreamCompatibility");
    expect(spec.properties.credentialsRef.required).toEqual(["name", "uid"]);
    // CEL compiles against declared schema fields, including those inside has().
    expect(spec.properties.upstreamCompatibility).toMatchObject({
      type: "object",
      properties: {
        sigsAgentSandbox: { type: "string", enum: ["off", "observe", "translate", "overlay"] },
        upstreamSandboxRef: {
          type: "object", required: ["name"],
          properties: { name: { type: "string", minLength: 1, maxLength: 253 } },
        },
        aiConformanceReference: { type: "boolean" },
      },
    });
    expect(spec.properties.upstreamCompatibility["x-kubernetes-validations"])
      .toContainEqual(expect.objectContaining({
        rule: "!has(self.sigsAgentSandbox) || self.sigsAgentSandbox != 'overlay' || has(self.upstreamSandboxRef)",
      }));
    expect(schema["x-kubernetes-validations"].some((rule: { rule: string }) =>
      rule.rule.includes("self.metadata.name") && rule.rule.includes("kars-credential-source-"))).toBe(true);
    expect(spec["x-kubernetes-validations"].some((rule: { rule: string }) =>
      rule.rule.includes("credentialsRef") && rule.rule.includes("overlay"))).toBe(true);
  });

  it("mounts the projection in the common agent container, never the inference router", () => {
    const code = source("controller/src/reconciler/mod.rs");
    const agent = code.slice(code.indexOf("let mut agent_container ="), code.indexOf("let mut agent_container =") + 1200);
    expect(agent).toContain('"envFrom": credentials.env_from(&name)');
    expect(code.match(/credentials\.env_from/g)).toHaveLength(1);
    expect(code).toContain('"envFrom": inference::provider_env_from()');
    expect(code.indexOf("credential_sources::reconcile(")).toBeLessThan(code.indexOf("let runtime_spec ="));
    expect(code).toContain(".owns(");
    expect(code.split("\n").length).toBeLessThan(3530);
  });

  it("keeps source values off argv and binds through a real UID rather than a generated placeholder", () => {
    const helpers = source("cli/src/lib/credential-source.ts");
    expect(helpers).toContain("metadata: { uid, resourceVersion }");
    expect(helpers).not.toContain("--from-literal");
    expect(helpers).not.toContain("--force");
    const add = source("cli/src/commands/add.ts");
    expect(add.indexOf("await prepareCredentialSource")).toBeLessThan(add.indexOf("await applySourceSandbox"));
    expect(add).toContain("await waitForCredentialSource");
    const projection = source("controller/src/reconciler/credential_source_projection.rs");
    expect(projection).not.toContain(".force()");
    expect(projection).toContain("Preconditions");
  });

  it("keeps new production modules bounded and free of stubs, crypto primitives, or gate waivers", () => {
    for (const path of [
      "controller/src/credential_source.rs",
      "controller/src/reconciler/agent_env.rs",
      "controller/src/reconciler/credential_sources.rs",
      "controller/src/reconciler/credential_source_projection.rs",
      "controller/src/reconciler/credential_source_workloads.rs",
      "cli/src/lib/credential-source.ts", "cli/src/lib/credential-source-io.ts",
    ]) {
      const text = source(path);
      expect(text.split("\n").length - 1, path).toBeLessThanOrEqual(800);
      expect(text.slice(0, 150), path).toContain("Copyright (c) Microsoft Corporation");
      expect(text, path).not.toMatch(/TODO\b|FIXME\b|XXX\b|HACK\b|unimplemented!\(|\btodo!\(|\bplaceholder\b|ci:stub-ok|ci:loc-ok/);
      expect(text, path).not.toMatch(/^use (sha2|hmac|curve25519_dalek|ed25519_dalek|x25519_dalek|aes|chacha20poly1305)::/m);
      expect(text, path).not.toMatch(/crypto\.subtle\.sign|createHmac|createSign|@noble\/(curves|hashes)/);
    }
  });
});
