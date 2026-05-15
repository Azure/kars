// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  POLICY_KIND_IDS,
  POLICY_KIND_SPECS,
  lookupPolicyKindSpec,
  signPolicyArtifact,
  renderBundleRefSnippet,
} from "./sign.js";

describe("POLICY_KIND_SPECS", () => {
  it("covers exactly the six signed policy kinds", () => {
    expect(POLICY_KIND_IDS).toEqual([
      "egress-allowlist",
      "agt-profile",
      "inference-policy",
      "memory-binding",
      "mcp-server-bundle",
      "eval-corpus",
    ]);
  });

  it("declares the canonical media types matching the controller", () => {
    expect(POLICY_KIND_SPECS["egress-allowlist"].mediaType).toBe(
      "application/vnd.azureclaw.egress-allowlist.v1+yaml",
    );
    expect(POLICY_KIND_SPECS["agt-profile"].mediaType).toBe(
      "application/vnd.azureclaw.agt-profile.v1+yaml",
    );
    expect(POLICY_KIND_SPECS["inference-policy"].mediaType).toBe(
      "application/vnd.azureclaw.inference-policy.v1+json",
    );
    expect(POLICY_KIND_SPECS["memory-binding"].mediaType).toBe(
      "application/vnd.azureclaw.memory-binding.v1+json",
    );
    expect(POLICY_KIND_SPECS["mcp-server-bundle"].mediaType).toBe(
      "application/vnd.azureclaw.mcp-server-bundle.v1+json",
    );
    expect(POLICY_KIND_SPECS["eval-corpus"].mediaType).toBe(
      "application/vnd.azureclaw.eval-corpus.v1+json",
    );
  });

  it("each spec has a non-empty controllerKind + extension", () => {
    for (const id of POLICY_KIND_IDS) {
      const spec = POLICY_KIND_SPECS[id];
      expect(spec.controllerKind.length).toBeGreaterThan(0);
      expect(spec.expectedExt.startsWith(".")).toBe(true);
    }
  });
});

describe("lookupPolicyKindSpec", () => {
  it("resolves a known kind", () => {
    expect(lookupPolicyKindSpec("inference-policy").controllerKind).toBe(
      "InferencePolicy",
    );
  });

  it("rejects an unknown kind with a helpful message", () => {
    expect(() => lookupPolicyKindSpec("egress")).toThrow(
      /unknown --kind 'egress'/,
    );
    expect(() => lookupPolicyKindSpec("egress")).toThrow(
      /egress-allowlist/,
    );
  });
});

describe("renderBundleRefSnippet", () => {
  it("emits YAML with all four bundleRef coordinates", () => {
    const snippet = renderBundleRefSnippet({
      kind: "agt-profile",
      mediaType: POLICY_KIND_SPECS["agt-profile"].mediaType,
      registry: "myacr.azurecr.io",
      repository: "policies/agt-profile",
      tag: "v3",
      digest: "sha256:" + "a".repeat(64),
      signMode: "keyless",
    });
    expect(snippet).toBe(
      [
        "bundleRef:",
        "  registry: myacr.azurecr.io",
        "  repository: policies/agt-profile",
        "  tag: v3",
        `  digest: sha256:${"a".repeat(64)}`,
      ].join("\n"),
    );
  });
});

describe("signPolicyArtifact — pre-flight", () => {
  function makeTmpFile(contents: string): { path: string; cleanup: () => void } {
    const dir = mkdtempSync(join(tmpdir(), "policy-sign-test-"));
    const path = join(dir, "artifact.json");
    writeFileSync(path, contents, { encoding: "utf8" });
    return { path, cleanup: () => rmSync(dir, { recursive: true, force: true }) };
  }

  it("rejects a missing file before spawning anything", async () => {
    await expect(
      signPolicyArtifact({
        kind: "inference-policy",
        filePath: "/no/such/file/please.json",
        registry: "r",
        repository: "p",
        signMode: "keyed",
        signKey: "/dev/null",
        skipSign: true,
        digestOverride: `sha256:${"0".repeat(64)}`,
        env: {},
        isTTY: false,
      }),
    ).rejects.toThrow(/does not exist/);
  });

  it("rejects an empty file", async () => {
    const f = makeTmpFile("");
    try {
      await expect(
        signPolicyArtifact({
          kind: "inference-policy",
          filePath: f.path,
          registry: "r",
          repository: "p",
          signMode: "keyed",
          signKey: "/dev/null",
          skipSign: true,
          digestOverride: `sha256:${"0".repeat(64)}`,
          env: {},
          isTTY: false,
        }),
      ).rejects.toThrow(/is empty/);
    } finally {
      f.cleanup();
    }
  });

  it("rejects an unknown --kind", async () => {
    const f = makeTmpFile("{}");
    try {
      await expect(
        signPolicyArtifact({
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          kind: "bogus-kind" as any,
          filePath: f.path,
          registry: "r",
          repository: "p",
          signMode: "keyed",
          signKey: "/dev/null",
          skipSign: true,
          digestOverride: `sha256:${"0".repeat(64)}`,
          env: {},
          isTTY: false,
        }),
      ).rejects.toThrow(/unknown --kind/);
    } finally {
      f.cleanup();
    }
  });

  it("rejects non-UTF-8 bytes", async () => {
    const dir = mkdtempSync(join(tmpdir(), "policy-sign-test-"));
    const path = join(dir, "binary.json");
    writeFileSync(path, Buffer.from([0xff, 0xfe, 0xfd, 0x00]));
    try {
      await expect(
        signPolicyArtifact({
          kind: "inference-policy",
          filePath: path,
          registry: "r",
          repository: "p",
          signMode: "keyed",
          signKey: "/dev/null",
          skipSign: true,
          digestOverride: `sha256:${"0".repeat(64)}`,
          env: {},
          isTTY: false,
        }),
      ).rejects.toThrow(/not valid UTF-8/);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("accepts a valid file and returns a populated SignedPolicyArtifact (skipSign)", async () => {
    const f = makeTmpFile('{"version":1}');
    try {
      const out = await signPolicyArtifact({
        kind: "inference-policy",
        filePath: f.path,
        registry: "myacr.azurecr.io",
        repository: "policies/inf",
        tag: "v7",
        signMode: "keyed",
        signKey: "/dev/null",
        skipSign: true,
        digestOverride: `sha256:${"b".repeat(64)}`,
        env: {},
        isTTY: false,
      });
      expect(out.kind).toBe("inference-policy");
      expect(out.mediaType).toBe(
        "application/vnd.azureclaw.inference-policy.v1+json",
      );
      expect(out.registry).toBe("myacr.azurecr.io");
      expect(out.repository).toBe("policies/inf");
      expect(out.tag).toBe("v7");
      expect(out.digest).toBe(`sha256:${"b".repeat(64)}`);
      expect(out.signMode).toBe("keyed");
    } finally {
      f.cleanup();
    }
  });

  it("rejects a digestOverride that is not sha256:…", async () => {
    const f = makeTmpFile('{"version":1}');
    try {
      await expect(
        signPolicyArtifact({
          kind: "inference-policy",
          filePath: f.path,
          registry: "r",
          repository: "p",
          signMode: "keyed",
          signKey: "/dev/null",
          skipSign: true,
          digestOverride: "deadbeef",
          env: {},
          isTTY: false,
        }),
      ).rejects.toThrow(/not a sha256/);
    } finally {
      f.cleanup();
    }
  });

  it("defaults the tag to 'latest' when omitted", async () => {
    const f = makeTmpFile('{"version":1}');
    try {
      const out = await signPolicyArtifact({
        kind: "memory-binding",
        filePath: f.path,
        registry: "r",
        repository: "p",
        signMode: "keyed",
        signKey: "/dev/null",
        skipSign: true,
        digestOverride: `sha256:${"c".repeat(64)}`,
        env: {},
        isTTY: false,
      });
      expect(out.tag).toBe("latest");
    } finally {
      f.cleanup();
    }
  });
});
