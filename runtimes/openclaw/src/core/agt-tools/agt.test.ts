import { describe, expect, it } from "vitest";
import type { AgtInboxEntry } from "../agt-handoff.js";
import { isReplyForAssignment } from "./agt.js";

function message(content: unknown, messageType?: string): AgtInboxEntry {
  return {
    from_amid: "did:mesh:worker",
    from_agent: "worker",
    content,
    timestamp: new Date(0).toISOString(),
    id: "message-1",
    message_type: messageType,
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

  it("keeps backward compatibility with unstructured peer replies", () => {
    expect(
      isReplyForAssignment(
        message("plain-text reply"),
        "did:mesh:worker",
        "worker",
        "assignment-1",
      ),
    ).toBe(true);
  });
});
