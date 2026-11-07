// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { companionImageDelta } from "./private-root-migration-images.js";

const OLD = `example.test/dispatcher:latest@sha256:${"1".repeat(64)}`;
const NEW = `example.test/dispatcher:latest@sha256:${"2".repeat(64)}`;
const spec = () => ({ replicas: 1, selector: { matchLabels: { app: "dispatcher" } },
  template: { metadata: { labels: { app: "dispatcher" } }, spec: { serviceAccountName: "dispatcher",
    containers: [{ name: "dispatcher", image: OLD, command: ["dispatch"], env: [{ name: "MODE", value: "real" }] }],
    initContainers: [{ name: "init", image: OLD }], volumes: [{ name: "data", emptyDir: {} }] } } });

describe("reviewed companion image-only delta", () => {
  it.each(["containers", "initContainers"] as const)("binds only the existing %s image without modifying inputs", group => {
    const before = spec(), target = structuredClone(before);
    target.template.spec[group][0]!.image = NEW;
    expect(companionImageDelta(before, target)).toEqual([{ group, name: group === "containers" ? "dispatcher" : "init", before: OLD, after: NEW }]);
    expect(before).toEqual(spec());
    expect(target.template.spec[group][0]!.image).toBe(NEW);
  });

  it.each(["unchanged", "mutable-old", "mutable-new", "command", "env", "account", "volumes", "replicas", "selector", "name", "extra-container", "removed-init", "annotation"])(
    "rejects %s instead of broadening image authority", fault => {
      const before = spec(), target: any = structuredClone(before);
      target.template.spec.containers[0].image = NEW;
      if (fault === "unchanged") target.template.spec.containers[0].image = OLD;
      if (fault === "mutable-old") before.template.spec.containers[0]!.image = "dispatcher:latest";
      if (fault === "mutable-new") target.template.spec.containers[0].image = "dispatcher:latest";
      if (fault === "command") target.template.spec.containers[0].command = ["other"];
      if (fault === "env") target.template.spec.containers[0].env[0].value = "other";
      if (fault === "account") target.template.spec.serviceAccountName = "other";
      if (fault === "volumes") target.template.spec.volumes = [];
      if (fault === "replicas") target.replicas = 2;
      if (fault === "selector") target.selector.matchLabels.app = "other";
      if (fault === "name") target.template.spec.containers[0].name = "other";
      if (fault === "extra-container") target.template.spec.containers.push({ name: "extra", image: NEW });
      if (fault === "removed-init") delete target.template.spec.initContainers;
      if (fault === "annotation") target.template.metadata.annotations = { checksum: "changed" };
      expect(() => companionImageDelta(before, target)).toThrow("image-only");
    },
  );
});
