import { describe, expect, it } from "vitest";
import type { AgtInboxEntry } from "../agt-handoff.js";
import {
  MESH_SEND_WAIT_SLICE_MS,
  assignmentWaitWindowOpen,
  canonicalLogicalAgentName,
  isMeshAwaitContentMessage,
  isReplyForAssignment,
  isTaskProgressMessage,
} from "./agt.js";

function message(
  content: unknown,
  messageType?: string,
  metadata: Partial<AgtInboxEntry> = {},
): AgtInboxEntry {
  return {
    from_amid: "did:mesh:worker",
    from_agent: "worker",
    content,
    timestamp: new Date(0).toISOString(),
    id: "message-1",
    message_type: messageType,
    ...metadata,
  };
}

describe("assignment reply correlation", () => {
  it("ignores file transfers and progress frames", () => {
    expect(
      isReplyForAssignment(
        message(JSON.stringify({ type: "file_transfer", file_name: "artifact.json" })),
        "did:mesh:worker",
        "worker",
        "assignment-1",
      ),
    ).toBe(false);
    expect(
      isReplyForAssignment(
        message({ type: "task_progress", stage: "working" }),
        "did:mesh:worker",
        "worker",
        "assignment-1",
      ),
    ).toBe(false);
  });

  it("classifies progress before generic auxiliary cleanup", () => {
    expect(
      isTaskProgressMessage(
        message({
          type: "task_progress",
          in_reply_to_id: "assignment-1",
          stage: "executing",
        }),
      ),
    ).toBe(true);
    expect(
      isTaskProgressMessage(
        message("working", "task_progress"),
      ),
    ).toBe(true);
    expect(
      isTaskProgressMessage(
        message({ type: "file_transfer" }),
      ),
    ).toBe(false);
  });

  it("accepts only the matching correlated task response", () => {
    expect(
      isReplyForAssignment(
        message({ type: "task_response", in_reply_to_id: "assignment-2", content: "wrong" }),
        "did:mesh:worker",
        "worker",
        "assignment-1",
      ),
    ).toBe(false);
    expect(
      isReplyForAssignment(
        message({ type: "task_response", in_reply_to_id: "assignment-1", content: "done" }),
        "did:mesh:worker",
        "worker",
        "assignment-1",
      ),
    ).toBe(true);
  });

  it("uses the production inbox correlation fields for task responses", () => {
    expect(
      isReplyForAssignment(
        message("done", "task_response", {
          in_reply_to_id: "assignment-1",
          task_ok: true,
        }),
        "did:mesh:worker",
        "worker",
        "assignment-1",
      ),
    ).toBe(true);
    expect(
      isReplyForAssignment(
        message("failed", "task_response", {
          in_reply_to_id: "assignment-2",
          task_ok: false,
        }),
        "did:mesh:worker",
        "worker",
        "assignment-1",
      ),
    ).toBe(false);
    expect(
      isReplyForAssignment(
        message("uncorrelated", "task_response"),
        "did:mesh:worker",
        "worker",
        "assignment-1",
      ),
    ).toBe(false);
  });

  it("accepts Hermes in_reply_to correlation", () => {
    expect(
      isReplyForAssignment(
        message({
          type: "task_response",
          in_reply_to: "assignment-1",
          ok: true,
          content: "done",
        }),
        "did:mesh:worker",
        "worker",
        "assignment-1",
      ),
    ).toBe(true);
  });

  it("rejects unstructured peer messages as assignment handbacks", () => {
    expect(
      isReplyForAssignment(
        message("plain-text reply"),
        "did:mesh:worker",
        "worker",
        "assignment-1",
      ),
    ).toBe(false);
  });
});

describe("assignment wait window", () => {
  it("renews the idle lease without exceeding the host-safe total slice", () => {
    const overallStartedAt = 1_000;
    const renewedIdleLeaseAt = overallStartedAt + MESH_SEND_WAIT_SLICE_MS - 1_000;

    expect(
      assignmentWaitWindowOpen(
        renewedIdleLeaseAt + 500,
        renewedIdleLeaseAt,
        overallStartedAt,
        90_000,
      ),
    ).toBe(true);
    expect(
      assignmentWaitWindowOpen(
        overallStartedAt + MESH_SEND_WAIT_SLICE_MS,
        renewedIdleLeaseAt,
        overallStartedAt,
        90_000,
      ),
    ).toBe(false);
  });

  describe("mesh await content filtering", () => {
    it("does not treat artifact transfer frames as role handbacks", () => {
      expect(
        isMeshAwaitContentMessage(
          message({ type: "file_transfer", file_name: "result.json" }, "file_transfer"),
        ),
      ).toBe(false);
      expect(
        isMeshAwaitContentMessage(
          message("done", "task_response", { in_reply_to_id: "assignment-1" }),
        ),
      ).toBe(true);
    });

    describe("mesh agent alias normalization", () => {
      it("maps a parent-scoped registry name back to its logical role", () => {
        const aliases = new Map([
          ["regression-ci-reviewer", "team-run-regression-a1b2c3d4"],
        ]);
        expect(
          canonicalLogicalAgentName("team-run-regression-a1b2c3d4", aliases),
        ).toBe("regression-ci-reviewer");
        expect(canonicalLogicalAgentName("dependency-analyst", aliases)).toBe(
          "dependency-analyst",
        );
      });
    });
  });

  it("still expires when progress stops before the total slice", () => {
    expect(assignmentWaitWindowOpen(91_000, 1_000, 1_000, 90_000)).toBe(false);
  });
});
