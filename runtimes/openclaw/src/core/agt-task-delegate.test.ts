import { describe, expect, it } from "vitest";
import { taskSessionId } from "./agt-task-delegate.js";

describe("taskSessionId", () => {
  it("sanitizes a raw mesh DID into an OpenClaw-safe session id", () => {
    const id = taskSessionId("did:mesh:253ac141613e8361389d7af3edb58513");
    expect(id).toBe("agt-task-did-mesh-253ac141613e8361389d7af3edb58513");
    expect(id).toMatch(/^[a-z0-9_-]+$/);
  });

  it("bounds long sender identities", () => {
    expect(taskSessionId(`peer:${"x".repeat(200)}`).length).toBeLessThanOrEqual(61);
  });
});
