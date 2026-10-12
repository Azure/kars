// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { applyReviewedGrant } from "../commands/credential-grants.js";
import { canonical, digest, kinds, PRIVATE_PREFIX as P, type Execute } from "./private-activation.js";
import { continuityFixture, rootPod } from "./private-activation-fixtures.js";
import { previewPrivateRootMigration } from "./private-root-migration-review.js";

const GRANTS = "karscredentialgrants.kars.azure.com";

async function setup() {
  const f = continuityFixture();
  const root = f.objects.get(f.key("deployment", "kars-controller", "core"));
  root.apiVersion = "apps/v1";
  root.metadata.annotations = { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "core" };
  root.spec.selector = { matchLabels: { app: "controller" } };
  root.spec.template.spec.containers[0].image = `example.test/controller@sha256:${"1".repeat(64)}`;
  f.pods.set("core", [rootPod(f)]);
  await applyReviewedGrant(f.execute, await f.document());
  const rendered = structuredClone(root);
  rendered.spec.template.spec.containers[0].image = `example.test/controller@sha256:${"2".repeat(64)}`;
  const calls: string[][] = [];
  const execute: Execute = async (args, input) => {
    calls.push(args);
    if (args[0] === "apply") {
      expect(args).toEqual(["apply", "--server-side", "--dry-run=server", "--field-manager=helm", "-f", "-", "-o", "json"]);
      return input!;
    }
    if (args[0] === "get" && args[1] === "namespaces") {
      return JSON.stringify({ metadata: {}, items: [...f.objects.entries()]
        .filter(([key]) => key.startsWith("namespace/")).map(([, object]) => object) });
    }
    if (args[0] === "get" && args[1] === Object.values(kinds).join(",")) {
      const namespace = args[args.indexOf("-n") + 1];
      return JSON.stringify({ metadata: {}, items: [...new Set(f.objects.values())].filter(object =>
        Object.hasOwn(kinds, object.kind ?? "") && object.metadata.namespace === namespace) });
    }
    return f.execute(args, input);
  };
  calls.length = 0;
  return { ...f, root, rendered, execute, reviewCalls: calls };
}

describe("read-only private root migration review", () => {
  it("binds a sealed root, grant identities and the exact rendered documents without a write", async () => {
    const f = await setup();
    const before = canonical([...f.objects.entries()]);
    const documents = [f.rendered, { apiVersion: "v1", kind: "Secret",
      metadata: { name: "private-input", namespace: "core" }, stringData: { token: "PRIVATE_VALUE_SENTINEL" } }];
    const review = await previewPrivateRootMigration(f.execute, "core", "kars", documents);
    expect(review.phase).toBe("reviewed");
    expect(review.grants).toEqual([{ namespace: "work", identity: {
      name: "workspace", uid: f.grant().metadata.uid, resourceVersion: f.grant().metadata.resourceVersion,
    }, specDigest: digest(f.grant().spec) }]);
    expect(review.renderDigest).toBe(digest(documents));
    expect(review.target.templateDigest).not.toBe(review.activation.root.templateDigest);
    expect(canonical([...f.objects.entries()])).toBe(before);
    expect(f.reviewCalls.every(args => ["get", "auth"].includes(args[0]!)
      || args.includes("--dry-run=server"))).toBe(true);
    expect(JSON.stringify(review)).not.toContain("PRIVATE_VALUE_SENTINEL");
    expect(JSON.stringify(review)).not.toContain("example.test/controller");
  });

  it("keeps an affiliated late namespace after its last grant is removed", async () => {
    const f = await setup();
    await applyReviewedGrant(f.execute, await f.document("second"));
    const grant = structuredClone(f.grant("second"));
    await applyReviewedGrant(f.execute, { ...grant, spec: { ...grant.spec, writers: [] } });
    f.objects.delete(f.key(GRANTS, "workspace", "second"));
    const review = await previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered]);
    expect(review.activation.namespaces.map(scope => scope.namespace.name)).toEqual(["core", "reader", "second", "work"]);
    expect(review.grants.map(grant => grant.namespace)).toEqual(["work"]);
  });

  it.each(["namespace", "grant"])("rejects a truncated %s inventory", async kind => {
    const f = await setup();
    const execute: Execute = async (args, input) => {
      const response = await f.execute(args, input);
      if (args[0] === "get" && args[1] === (kind === "namespace" ? "namespaces" : GRANTS)
        && args.includes("--all-namespaces")) {
        return JSON.stringify({ ...JSON.parse(response), metadata: { continue: "remaining" } });
      }
      return response;
    };
    await expect(previewPrivateRootMigration(execute, "core", "kars", [f.rendered])).rejects.toThrow("inventory");
  });

  it("refuses a foreign grant with a writer in the migrating scope instead of silently omitting it", async () => {
    const f = await setup();
    f.objects.set(f.key(GRANTS, "workspace", "third"), {
      metadata: { name: "workspace", namespace: "third", uid: "foreign-grant", resourceVersion: "1" },
      spec: { workspaceUid: "third-uid", writers: [{ namespace: "reader", name: "bff", uid: "reader-sa" }] },
    });
    await expect(previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered])).rejects.toThrow("inventory");
  });

  it.each(["mutable", "init-image", "account", "uid", "replicas", "selector", "budget", "hook"])(
    "rejects an unreviewed target %s", async fault => {
      const f = await setup();
      if (fault === "mutable") f.rendered.spec.template.spec.containers[0].image = "controller:latest";
      if (fault === "init-image") f.rendered.spec.template.spec.initContainers = [{ name: "init", image: "tool:latest" }];
      if (fault === "account") f.rendered.spec.template.spec.serviceAccountName = "other";
      if (fault === "uid") f.rendered.metadata.uid = "other";
      if (fault === "replicas") f.rendered.spec.replicas = 3;
      if (fault === "selector") f.rendered.spec.selector.matchLabels.app = "other";
      if (fault === "budget") f.rendered.spec.template.spec.containers[0].env = [{ name: "KARS_INFERENCE_BUDGET_ENABLED", value: "true" }];
      if (fault === "hook") f.rendered.metadata.annotations["helm.sh/hook"] = "pre-upgrade";
      await expect(previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered])).rejects.toThrow();
      expect(f.reviewCalls.every(args => ["get", "auth"].includes(args[0]!) || args.includes("--dry-run=server"))).toBe(true);
    },
  );

  it.each(["namespace", "grant", "root"])("rejects a concurrent %s change during projection", async fault => {
    const f = await setup();
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[0] === "apply") {
        if (fault === "namespace") f.namespace("work").metadata.resourceVersion = "999";
        if (fault === "grant") f.grant().spec.agentKeys = ["NEW_TOKEN"];
        if (fault === "root") f.root.spec.replicas = 3;
      }
      return result;
    };
    await expect(previewPrivateRootMigration(execute, "core", "kars", [f.rendered])).rejects.toThrow("inventory");
  });

  it.each(["missing-affiliation", "wrong-root-uid", "missing-parent", "bad-seal", "wrong-release", "missing-root", "duplicate-root"])(
    "refuses %s instead of producing a partial review", async fault => {
      const f = await setup();
      let documents = [f.rendered];
      if (fault === "missing-affiliation") delete f.namespace("work").metadata.annotations[`${P}root-namespace-uid`];
      if (fault === "wrong-root-uid") f.namespace("work").metadata.annotations[`${P}root-namespace-uid`] = "other";
      if (fault === "missing-parent") f.namespace("core").metadata.annotations[`${P}parent-missing`] = f.namespace("core").metadata.annotations[`${P}epoch`];
      if (fault === "bad-seal") f.namespace("core").metadata.annotations[`${P}root-retirement`] = "{}";
      if (fault === "wrong-release") f.root.metadata.annotations["meta.helm.sh/release-name"] = "other";
      if (fault === "missing-root") documents = [];
      if (fault === "duplicate-root") documents.push(structuredClone(f.rendered));
      await expect(previewPrivateRootMigration(f.execute, "core", "kars", documents)).rejects.toThrow();
    },
  );
});
