// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { privateCommandFailure, scopedCommandFailure, PrivateCommandFailure } from "./private-activation-command-diagnostics.js";

describe("private command failure projection", () => {
  it.each(["Conflict", "Forbidden", "Invalid", "NotFound", "ServiceUnavailable"])(
    "retains only the explicit server %s reason and process exit code", reason => {
      const failure = privateCommandFailure({
        exitCode: 1, stderr: `Error from server (${reason}): private-stderr-canary\nsecret-value-canary`,
        stdout: "private-stdout-canary", command: "kubectl patch secret private-name-canary",
        message: "private-error-canary", cause: new Error("private-cause-canary"),
      }, ["patch", "karssandbox", "private-name-canary", "-p", "private-payload-canary"], "Pausing");
      expect(failure.facts).toEqual({
        version: 1, phase: "Pausing", operation: "patch", resourceKind: "KarsSandbox",
        serverReason: reason, exitCode: 1,
      });
      expect(failure.message).toBe(`KARS_PRIVATE_COMMAND_FAILURE ${JSON.stringify(failure.facts)}`);
      expect(JSON.stringify(failure) + failure.stack).not.toContain("canary");
      expect(failure).not.toHaveProperty("cause");
      expect(failure.facts).not.toHaveProperty("httpStatus");
    });

  it.each(["private-error-canary", "Error from server (PrivateCanary): secret", "Error from server (Conflict): x\nError from server (Forbidden): y",
    `Error from server (Conflict): ${"x".repeat(65_536)}`])(
    "does not infer an API result from ambiguous or unsupported stderr %#", stderr => {
      const failure = privateCommandFailure({ stderr, exitCode: 1 }, ["get", "secret", "private"]);
      expect(failure.facts.serverReason).toBe("Unknown");
    });

  it.each([undefined, null, "1", -1, 256, NaN])("rejects unsupported process exit status %s", exitCode => {
    expect(privateCommandFailure({ exitCode }, ["patch", "namespace"]).facts.exitCode).toBeNull();
  });

  it.each(["private-kind-canary", "__proto__", "constructor"])("does not echo unclassified resource %s", kind => {
    const value = privateCommandFailure({}, ["private-operation-canary", kind, "private-name-canary"]);
    expect(value.facts.operation).toBe("other");
    expect(value.facts.resourceKind).toBe("Other");
    expect(value.message).not.toContain("canary");
  });

  it("adds the lifecycle phase without losing the underlying safe command facts", () => {
    const initial = privateCommandFailure({ exitCode: 1, stderr: "Error from server (Forbidden): private" },
      ["get", "deployment", "private"]);
    const scoped = scopedCommandFailure(initial, [], "Restoring");
    expect(scoped).toBeInstanceOf(PrivateCommandFailure);
    expect((scoped as PrivateCommandFailure).facts).toEqual({ ...initial.facts, phase: "Restoring" });
    const semantic = new Error("fixed semantic failure");
    expect(scopedCommandFailure(semantic, [], "Pausing")).toBe(semantic);
  });
});
