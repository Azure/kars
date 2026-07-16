import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import {
  appendCollaborationEvent,
  beginEvidenceScope,
  endEvidenceScope,
  evidenceDigest,
} from "./evidence-log.js";

const originalRoot = process.env.KARS_WORKSPACE_ROOT;

afterEach(() => {
  if (originalRoot === undefined) delete process.env.KARS_WORKSPACE_ROOT;
  else process.env.KARS_WORKSPACE_ROOT = originalRoot;
});

describe("durable evidence logs", () => {
  it("writes collaboration and research JSONL with agent attribution", () => {
    const root = mkdtempSync(join(tmpdir(), "kars-evidence-"));
    process.env.KARS_WORKSPACE_ROOT = root;
    process.env.SANDBOX_NAME = "team-principal";
    beginEvidenceScope("run-1");
    try {
      appendCollaborationEvent({ event: "assignment_sent", member: "qa" });

      const collaboration = JSON.parse(
        readFileSync(join(root, "artifacts", ".run-run-1", "collaboration.jsonl"), "utf8").trim(),
      );
      expect(collaboration).toMatchObject({
        agent: "team-principal",
        event: "assignment_sent",
        member: "qa",
      });
      expect(collaboration.at).toMatch(/Z$/);
    } finally {
      endEvidenceScope();
      rmSync(root, { recursive: true, force: true });
      delete process.env.SANDBOX_NAME;
    }
  });

  it("produces stable content digests", () => {
    expect(evidenceDigest({ a: 1 })).toBe(evidenceDigest({ a: 1 }));
    expect(evidenceDigest({ a: 1 })).not.toBe(evidenceDigest({ a: 2 }));
  });

});
