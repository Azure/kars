// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect, vi, beforeEach } from "vitest";
import {
  buildCanonicalAllowlist,
  digestOfCanonical,
  autoDetectSignMode,
  buildOrasPushArgv,
  buildCosignSignArgv,
  buildPatchArgv,
  parseOrasDigest,
  pushArtifact,
  signArtifact,
  patchClawSandbox,
  ensureSigningTools,
  buildEmitManifestYaml,
  describeSignerIdentity,
  writeEmitManifest,
  EGRESS_ALLOWLIST_MEDIA_TYPE,
} from "./sign.js";

describe("buildCanonicalAllowlist", () => {
  it("produces sorted, deduped, port-explicit YAML", () => {
    const out = buildCanonicalAllowlist({
      generation: 1,
      endpoints: [
        { host: "dev.azure.com", port: 443 },
        { host: "api.github.com", port: 443 },
        { host: "api.github.com", port: 443 }, // dup
        { host: "api.github.com", port: 80 },
      ],
    });
    expect(out.yaml).toBe(
      [
        "apiVersion: azureclaw.dev/v1alpha1",
        "kind: EgressAllowlist",
        "metadata:",
        "  generation: 1",
        "spec:",
        "  endpoints:",
        "    - host: api.github.com",
        "      port: 80",
        "    - host: api.github.com",
        "      port: 443",
        "    - host: dev.azure.com",
        "      port: 443",
        "",
      ].join("\n"),
    );
    expect(out.endpoints.length).toBe(3);
  });

  it("is byte-stable across two calls with the same input (different order)", () => {
    const a = buildCanonicalAllowlist({
      generation: 7,
      endpoints: [
        { host: "b.example.com", port: 443 },
        { host: "a.example.com", port: 443 },
      ],
    });
    const b = buildCanonicalAllowlist({
      generation: 7,
      endpoints: [
        { host: "a.example.com", port: 443 },
        { host: "b.example.com", port: 443 },
      ],
    });
    expect(a.yaml).toBe(b.yaml);
    expect(digestOfCanonical(a.yaml)).toBe(digestOfCanonical(b.yaml));
  });

  it("rejects empty endpoints list", () => {
    expect(() =>
      buildCanonicalAllowlist({ generation: 1, endpoints: [] }),
    ).toThrow(/empty/i);
  });

  it("rejects host with uppercase ASCII (must be IDNA-normalized)", () => {
    expect(() =>
      buildCanonicalAllowlist({
        generation: 1,
        endpoints: [{ host: "API.github.com", port: 443 }],
      }),
    ).toThrow(/canonical regex/);
  });

  it("rejects host with leading/trailing whitespace", () => {
    expect(() =>
      buildCanonicalAllowlist({
        generation: 1,
        endpoints: [{ host: " api.github.com", port: 443 }],
      }),
    ).toThrow(/whitespace/);
  });

  it("rejects wildcard host", () => {
    expect(() =>
      buildCanonicalAllowlist({
        generation: 1,
        endpoints: [{ host: "*.github.com", port: 443 }],
      }),
    ).toThrow(/wildcard/);
  });

  it("rejects out-of-range port", () => {
    expect(() =>
      buildCanonicalAllowlist({
        generation: 1,
        endpoints: [{ host: "api.github.com", port: 0 }],
      }),
    ).toThrow(/port/);
    expect(() =>
      buildCanonicalAllowlist({
        generation: 1,
        endpoints: [{ host: "api.github.com", port: 70000 }],
      }),
    ).toThrow(/port/);
  });

  it("rejects non-positive generation", () => {
    expect(() =>
      buildCanonicalAllowlist({
        generation: 0,
        endpoints: [{ host: "api.github.com", port: 443 }],
      }),
    ).toThrow(/generation/);
  });

  it("IDNA-encodes non-ASCII hosts to xn-- punycode", () => {
    const out = buildCanonicalAllowlist({
      generation: 1,
      endpoints: [{ host: "münchen.de", port: 443 }],
    });
    expect(out.endpoints[0].host).toMatch(/^xn--/);
    expect(out.yaml).toContain("xn--");
  });

  it("ends with a single trailing LF", () => {
    const out = buildCanonicalAllowlist({
      generation: 1,
      endpoints: [{ host: "a.example.com", port: 443 }],
    });
    expect(out.yaml.endsWith("\n")).toBe(true);
    expect(out.yaml.endsWith("\n\n")).toBe(false);
    expect(out.yaml.includes("\r")).toBe(false);
  });
});

describe("autoDetectSignMode", () => {
  it("picks keyless when TTY + no token + no key", () => {
    expect(
      autoDetectSignMode({ isTTY: true, env: {} }),
    ).toBe("keyless");
  });

  it("picks identity-token when SIGSTORE_ID_TOKEN is set", () => {
    expect(
      autoDetectSignMode({ isTTY: false, env: { SIGSTORE_ID_TOKEN: "x" } }),
    ).toBe("identity-token");
  });

  it("picks identity-token when OIDC_TOKEN is set", () => {
    expect(
      autoDetectSignMode({ isTTY: false, env: { OIDC_TOKEN: "x" } }),
    ).toBe("identity-token");
  });

  it("picks keyed when --sign-mode keyed + --sign-key", () => {
    expect(
      autoDetectSignMode({
        signModeFlag: "keyed",
        signKey: "azurekms://kv/k",
        isTTY: true,
        env: {},
      }),
    ).toBe("keyed");
  });

  it("errors when --sign-mode keyed without --sign-key", () => {
    expect(() =>
      autoDetectSignMode({
        signModeFlag: "keyed",
        isTTY: true,
        env: {},
      }),
    ).toThrow(/--sign-key/);
  });

  it("errors when non-TTY without token and without key", () => {
    expect(() =>
      autoDetectSignMode({ isTTY: false, env: {} }),
    ).toThrow(/SIGSTORE_ID_TOKEN/);
  });

  it("rejects unknown --sign-mode value", () => {
    expect(() =>
      autoDetectSignMode({
        signModeFlag: "weird",
        isTTY: true,
        env: {},
      }),
    ).toThrow(/--sign-mode/);
  });
});

describe("ensureSigningTools", () => {
  beforeEach(() => {
    vi.resetModules();
  });

  it("returns paths when both tools are present", async () => {
    const fakeExeca = vi.fn(async (_file: string, args: readonly string[]) => {
      const bin = args[0];
      if (bin === "oras") return { stdout: "/usr/local/bin/oras" };
      if (bin === "cosign") return { stdout: "/usr/local/bin/cosign" };
      throw new Error("unexpected");
    });
    vi.doMock("execa", () => ({ execa: fakeExeca }));
    const { ensureSigningTools: fresh } = await import("./sign.js");
    const out = await fresh();
    expect(out.orasPath).toBe("/usr/local/bin/oras");
    expect(out.cosignPath).toBe("/usr/local/bin/cosign");
    vi.doUnmock("execa");
  });

  it("throws actionable error with install URL when oras missing", async () => {
    vi.resetModules();
    vi.doMock("execa", () => ({
      execa: vi.fn(async (_file: string, args: readonly string[]) => {
        if (args[0] === "oras") throw new Error("not found");
        return { stdout: "/usr/local/bin/cosign" };
      }),
    }));
    const { ensureSigningTools: fresh } = await import("./sign.js");
    await expect(fresh()).rejects.toThrow(/oras\.land\/docs\/installation/);
    vi.doUnmock("execa");
  });

  it("throws actionable error with install URL when cosign missing", async () => {
    vi.resetModules();
    vi.doMock("execa", () => ({
      execa: vi.fn(async (_file: string, args: readonly string[]) => {
        if (args[0] === "oras") return { stdout: "/usr/local/bin/oras" };
        throw new Error("not found");
      }),
    }));
    const { ensureSigningTools: fresh } = await import("./sign.js");
    await expect(fresh()).rejects.toThrow(/sigstore\.dev\/cosign\/installation/);
    vi.doUnmock("execa");
  });

  // Reference unused symbol so TS doesn't drop it in declaration-only contexts.
  void ensureSigningTools;
});

describe("buildOrasPushArgv", () => {
  it("constructs the correct oras push argv", () => {
    const argv = buildOrasPushArgv({
      registry: "myacr.azurecr.io",
      repository: "policy/egress-allowlist/demo-agent",
      artifactType: EGRESS_ALLOWLIST_MEDIA_TYPE,
      filename: "allowlist.yaml",
    });
    expect(argv).toEqual([
      "push",
      "myacr.azurecr.io/policy/egress-allowlist/demo-agent:latest",
      "--artifact-type",
      EGRESS_ALLOWLIST_MEDIA_TYPE,
      "--format",
      "json",
      `allowlist.yaml:${EGRESS_ALLOWLIST_MEDIA_TYPE}`,
    ]);
  });

  it("respects custom tag", () => {
    const argv = buildOrasPushArgv({
      registry: "r.example.com",
      repository: "x/y",
      artifactType: EGRESS_ALLOWLIST_MEDIA_TYPE,
      tag: "gen-7",
      filename: "allowlist.yaml",
    });
    expect(argv[1]).toBe("r.example.com/x/y:gen-7");
  });
});

describe("parseOrasDigest", () => {
  it("parses JSON output", () => {
    const stdout = JSON.stringify({ reference: { digest: "sha256:" + "a".repeat(64) } });
    expect(parseOrasDigest(stdout)).toBe("sha256:" + "a".repeat(64));
  });

  it("parses plain text 'Digest: …' output", () => {
    expect(parseOrasDigest(`Pushed.\nDigest: sha256:${"b".repeat(64)}\n`)).toBe(
      "sha256:" + "b".repeat(64),
    );
  });

  it("returns null on unrecognized output", () => {
    expect(parseOrasDigest("???")).toBeNull();
  });
});

describe("pushArtifact", () => {
  it("invokes oras with the right argv and verifies the blob digest", async () => {
    const yaml = "apiVersion: azureclaw.dev/v1alpha1\nkind: EgressAllowlist\n";
    const expectedBlob = digestOfCanonical(yaml);
    const manifestDigest = "sha256:" + "a".repeat(64);
    const fakeExeca = vi.fn(async () => ({
      stdout: JSON.stringify({
        reference: "r.example.com/policy/egress-allowlist/foo:latest",
        digest: manifestDigest,
        files: [{ path: "allowlist.yaml", digest: expectedBlob }],
      }),
    }));
    const out = await pushArtifact({
      orasPath: "/usr/bin/oras",
      registry: "r.example.com",
      repository: "policy/egress-allowlist/foo",
      yaml,
      artifactType: EGRESS_ALLOWLIST_MEDIA_TYPE,
      execaImpl: fakeExeca as any,
    });
    expect(out).toBe(manifestDigest);
    expect(fakeExeca).toHaveBeenCalledTimes(1);
    const [bin, argv] = fakeExeca.mock.calls[0] as unknown as [string, string[]];
    expect(bin).toBe("/usr/bin/oras");
    expect(argv[0]).toBe("push");
    expect(argv[1]).toBe("r.example.com/policy/egress-allowlist/foo:latest");
  });

  it("aborts when oras-reported blob digest disagrees with locally computed digest", async () => {
    const yaml = "apiVersion: azureclaw.dev/v1alpha1\n";
    const wrongBlob = "sha256:" + "0".repeat(64);
    const fakeExeca = vi.fn(async () => ({
      stdout: JSON.stringify({
        reference: "r/x/y:latest",
        digest: "sha256:" + "a".repeat(64),
        files: [{ path: "allowlist.yaml", digest: wrongBlob }],
      }),
    }));
    await expect(
      pushArtifact({
        orasPath: "/usr/bin/oras",
        registry: "r",
        repository: "x/y",
        yaml,
        artifactType: EGRESS_ALLOWLIST_MEDIA_TYPE,
        execaImpl: fakeExeca as any,
      }),
    ).rejects.toThrow(/diverged/);
  });

  it("returns manifest digest when oras output omits files[].digest (still acceptable)", async () => {
    const yaml = "apiVersion: azureclaw.dev/v1alpha1\n";
    const manifest = "sha256:" + "c".repeat(64);
    const fakeExeca = vi.fn(async () => ({
      stdout: JSON.stringify({ reference: "r/x/y:latest", digest: manifest }),
    }));
    const out = await pushArtifact({
      orasPath: "/usr/bin/oras",
      registry: "r",
      repository: "x/y",
      yaml,
      artifactType: EGRESS_ALLOWLIST_MEDIA_TYPE,
      execaImpl: fakeExeca as any,
    });
    expect(out).toBe(manifest);
  });
});

describe("buildCosignSignArgv", () => {
  const base = {
    registry: "r.example.com",
    repository: "p/e/foo",
    digest: "sha256:" + "a".repeat(64),
  };

  it("keyless mode has no auth flag", () => {
    expect(
      buildCosignSignArgv({ ...base, mode: "keyless" }),
    ).toEqual([
      "sign",
      "--yes",
      "--registry-referrers-mode",
      "legacy",
      `${base.registry}/${base.repository}@${base.digest}`,
    ]);
  });

  it("identity-token mode passes --identity-token", () => {
    expect(
      buildCosignSignArgv({
        ...base,
        mode: "identity-token",
        identityToken: "tok123",
      }),
    ).toEqual([
      "sign",
      "--yes",
      "--registry-referrers-mode",
      "legacy",
      "--identity-token",
      "tok123",
      `${base.registry}/${base.repository}@${base.digest}`,
    ]);
  });

  it("keyed mode passes --key", () => {
    expect(
      buildCosignSignArgv({
        ...base,
        mode: "keyed",
        keyRef: "azurekms://kv/k",
      }),
    ).toEqual([
      "sign",
      "--yes",
      "--registry-referrers-mode",
      "legacy",
      "--key",
      "azurekms://kv/k",
      `${base.registry}/${base.repository}@${base.digest}`,
    ]);
  });

  it("keyed mode without keyRef throws", () => {
    expect(() =>
      buildCosignSignArgv({ ...base, mode: "keyed" }),
    ).toThrow(/--sign-key/);
  });
});

describe("signArtifact", () => {
  it("invokes cosign with the constructed argv", async () => {
    const fakeExeca = vi.fn(async () => ({ stdout: "" }));
    await signArtifact({
      cosignPath: "/usr/bin/cosign",
      registry: "r.example.com",
      repository: "x/y",
      digest: "sha256:" + "a".repeat(64),
      mode: "keyed",
      keyRef: "k.pem",
      execaImpl: fakeExeca as any,
    });
    const [bin, argv] = fakeExeca.mock.calls[0] as unknown as [string, string[]];
    expect(bin).toBe("/usr/bin/cosign");
    expect(argv).toEqual([
      "sign",
      "--yes",
      "--registry-referrers-mode",
      "legacy",
      "--key",
      "k.pem",
      `r.example.com/x/y@sha256:${"a".repeat(64)}`,
    ]);
  });
});

describe("buildPatchArgv / patchClawSandbox", () => {
  it("constructs the correct kubectl JSON merge patch argv", () => {
    const argv = buildPatchArgv({
      namespace: "azureclaw-foo",
      name: "foo",
      registry: "r.example.com",
      repository: "policy/egress-allowlist/foo",
      digest: "sha256:" + "a".repeat(64),
      artifactType: EGRESS_ALLOWLIST_MEDIA_TYPE,
    });
    expect(argv.slice(0, 5)).toEqual([
      "patch",
      "clawsandbox/foo",
      "-n",
      "azureclaw-foo",
      "--type=merge",
    ]);
    expect(argv[5]).toBe("-p");
    const patch = JSON.parse(argv[6]);
    expect(patch).toEqual({
      spec: {
        networkPolicy: {
          allowlistRef: {
            registry: "r.example.com",
            repository: "policy/egress-allowlist/foo",
            digest: "sha256:" + "a".repeat(64),
            artifactType: EGRESS_ALLOWLIST_MEDIA_TYPE,
          },
        },
      },
    });
  });

  it("invokes kubectl with the constructed patch argv", async () => {
    const fakeExeca = vi.fn(async () => ({ stdout: "patched" }));
    await patchClawSandbox({
      kubectlPath: "kubectl",
      namespace: "azureclaw-foo",
      name: "foo",
      registry: "r",
      repository: "x/y",
      digest: "sha256:" + "a".repeat(64),
      artifactType: EGRESS_ALLOWLIST_MEDIA_TYPE,
      execaImpl: fakeExeca as any,
    });
    expect(fakeExeca).toHaveBeenCalledTimes(1);
    const [bin] = fakeExeca.mock.calls[0] as unknown as [string];
    expect(bin).toBe("kubectl");
  });
});

// ---- S12.g — emit-manifest GitOps mode ------------------------------------

describe("buildEmitManifestYaml", () => {
  const base = {
    namespace: "azureclaw-demo",
    name: "demo-agent",
    registry: "myacr.azurecr.io",
    repository: "policy/egress-allowlist/demo-agent",
    digest: "sha256:" + "a".repeat(64),
    artifactType: EGRESS_ALLOWLIST_MEDIA_TYPE,
    signerIdentity: "keyless:fulcio",
  };

  it("emits a complete, byte-stable ClawSandbox patch with header comment", () => {
    const yaml = buildEmitManifestYaml(base);
    expect(yaml).toBe(
      [
        `# azureclaw egress allowlist — digest=${base.digest} signer=keyless:fulcio`,
        `# Generated by 'azureclaw egress … --emit-manifest'.`,
        `# Commit this file unchanged; your GitOps controller applies it.`,
        `apiVersion: azureclaw.azure.com/v1alpha1`,
        `kind: ClawSandbox`,
        `metadata:`,
        `  name: demo-agent`,
        `  namespace: azureclaw-demo`,
        `  annotations:`,
        `    azureclaw.io/applied-via-gitops: "true"`,
        `spec:`,
        `  networkPolicy:`,
        `    allowlistRef:`,
        `      registry: myacr.azurecr.io`,
        `      repository: policy/egress-allowlist/demo-agent`,
        `      digest: ${base.digest}`,
        `      artifactType: ${EGRESS_ALLOWLIST_MEDIA_TYPE}`,
        ``,
      ].join("\n"),
    );
  });

  it("is byte-identical across two calls with the same input (deterministic for git diff)", () => {
    const a = buildEmitManifestYaml(base);
    const b = buildEmitManifestYaml({ ...base });
    expect(a).toBe(b);
    // No trailing whitespace on any line, exactly one trailing LF.
    for (const line of a.split("\n").slice(0, -1)) {
      expect(line).not.toMatch(/\s$/);
    }
    expect(a.endsWith("\n")).toBe(true);
    expect(a.endsWith("\n\n")).toBe(false);
  });

  it("rejects malformed digest", () => {
    expect(() =>
      buildEmitManifestYaml({ ...base, digest: "notadigest" }),
    ).toThrow(/malformed digest/);
  });

  it("rejects empty signer identity", () => {
    expect(() =>
      buildEmitManifestYaml({ ...base, signerIdentity: "" }),
    ).toThrow(/signerIdentity/);
  });

  it("requires namespace and name", () => {
    expect(() =>
      buildEmitManifestYaml({ ...base, namespace: "" }),
    ).toThrow(/namespace and name/);
    expect(() =>
      buildEmitManifestYaml({ ...base, name: "" }),
    ).toThrow(/namespace and name/);
  });

  it("parses cleanly as YAML and shape-matches the CRD (allowlistRef + GitOps marker)", async () => {
    // Pure-parsing assertion — no live cluster required. Confirms the
    // emitted bytes round-trip through a generic YAML parser into the
    // structure the controller's CRD schema expects.
    const yamlMod = await import("yaml");
    const yaml = buildEmitManifestYaml(base);
    const obj = yamlMod.parse(yaml) as any;
    expect(obj.apiVersion).toBe("azureclaw.azure.com/v1alpha1");
    expect(obj.kind).toBe("ClawSandbox");
    expect(obj.metadata.name).toBe("demo-agent");
    expect(obj.metadata.namespace).toBe("azureclaw-demo");
    expect(obj.metadata.annotations["azureclaw.io/applied-via-gitops"]).toBe("true");
    expect(obj.spec.networkPolicy.allowlistRef).toEqual({
      registry: base.registry,
      repository: base.repository,
      digest: base.digest,
      artifactType: base.artifactType,
    });
  });
});

describe("describeSignerIdentity", () => {
  it("returns keyless:fulcio for keyless mode", () => {
    expect(describeSignerIdentity({ mode: "keyless" })).toBe("keyless:fulcio");
  });

  it("notes SIGSTORE_ID_TOKEN env source for identity-token mode", () => {
    expect(
      describeSignerIdentity({
        mode: "identity-token",
        env: { SIGSTORE_ID_TOKEN: "tok" },
      }),
    ).toBe("identity-token:SIGSTORE_ID_TOKEN");
  });

  it("notes OIDC_TOKEN env source for identity-token mode", () => {
    expect(
      describeSignerIdentity({
        mode: "identity-token",
        env: { OIDC_TOKEN: "tok" },
      }),
    ).toBe("identity-token:OIDC_TOKEN");
  });

  it("includes keyRef for keyed mode", () => {
    expect(
      describeSignerIdentity({
        mode: "keyed",
        keyRef: "azurekms://kv/k",
      }),
    ).toBe("keyed:azurekms://kv/k");
  });
});

describe("writeEmitManifest", () => {
  it("writes when target file does not exist", () => {
    const writes: Array<[string, string]> = [];
    writeEmitManifest({
      path: "./out.yaml",
      yaml: "hello\n",
      force: false,
      fsImpl: {
        existsSync: () => false,
        writeFileSync: (p, data) => {
          writes.push([p, data]);
        },
      },
    });
    expect(writes).toEqual([["./out.yaml", "hello\n"]]);
  });

  it("refuses to overwrite without --force", () => {
    expect(() =>
      writeEmitManifest({
        path: "/exists.yaml",
        yaml: "x",
        force: false,
        fsImpl: {
          existsSync: () => true,
          writeFileSync: () => {
            throw new Error("should not be called");
          },
        },
      }),
    ).toThrow(/refusing to overwrite/);
  });

  it("overwrites when --force is set", () => {
    const writes: Array<[string, string]> = [];
    writeEmitManifest({
      path: "/exists.yaml",
      yaml: "y",
      force: true,
      fsImpl: {
        existsSync: () => true,
        writeFileSync: (p, d) => writes.push([p, d]),
      },
    });
    expect(writes.length).toBe(1);
  });
});
