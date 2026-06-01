// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { startTaskProgressHeartbeat } from "./agt-heartbeat.js";

describe("startTaskProgressHeartbeat", () => {
  // Vitest 4's ReturnType<typeof vi.fn> is no longer assignable to the
  // narrow callback type (m: string) => void. Use the structural type
  // the production code expects so the mock satisfies MeshLogger.
  let log: { info: (m: string) => void; warn: (m: string) => void };

  beforeEach(() => {
    vi.useFakeTimers();
    log = { info: vi.fn(), warn: vi.fn() };
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("fires an initial 'started' ping synchronously", () => {
    const send = vi.fn().mockResolvedValue(undefined);
    const cancel = startTaskProgressHeartbeat(
      "did:mesh:parent",
      { send },
      "sub-agent-x",
      log,
    );

    expect(send).toHaveBeenCalledTimes(1);
    const [amid, msg] = send.mock.calls[0];
    expect(amid).toBe("did:mesh:parent");
    expect(msg.type).toBe("task_progress");
    expect(msg.stage).toBe("started");
    expect(msg.from_agent).toBe("sub-agent-x");
    expect(msg.tick).toBe(0);
    expect(typeof msg.elapsed_seconds).toBe("number");
    expect(typeof msg.timestamp).toBe("string");

    cancel();
  });

  it("fires periodic 'executing' pings on the configured interval", () => {
    const send = vi.fn().mockResolvedValue(undefined);
    const cancel = startTaskProgressHeartbeat(
      "did:mesh:parent",
      { send },
      "sub-agent-x",
      log,
      5_000, // 5s for the test
    );

    expect(send).toHaveBeenCalledTimes(1); // initial 'started'

    vi.advanceTimersByTime(5_000);
    expect(send).toHaveBeenCalledTimes(2);
    expect(send.mock.calls[1][1].stage).toBe("executing");
    expect(send.mock.calls[1][1].tick).toBe(1);

    vi.advanceTimersByTime(5_000);
    expect(send).toHaveBeenCalledTimes(3);
    expect(send.mock.calls[2][1].tick).toBe(2);

    cancel();
  });

  it("stops firing after cancel()", () => {
    const send = vi.fn().mockResolvedValue(undefined);
    const cancel = startTaskProgressHeartbeat(
      "did:mesh:parent",
      { send },
      "sub-agent-x",
      log,
      5_000,
    );

    expect(send).toHaveBeenCalledTimes(1);
    cancel();

    vi.advanceTimersByTime(60_000);
    expect(send).toHaveBeenCalledTimes(1); // never increased
  });

  it("is no-op when meshClient is null", () => {
    const cancel = startTaskProgressHeartbeat(
      "did:mesh:parent",
      null,
      "sub-agent-x",
      log,
      5_000,
    );

    vi.advanceTimersByTime(20_000);
    // Nothing thrown, no logs about send failures.
    cancel();
  });

  it("swallows send errors and continues firing on the next tick", async () => {
    const send = vi
      .fn()
      .mockRejectedValueOnce(new Error("ratchet broken"))
      .mockResolvedValue(undefined);

    const cancel = startTaskProgressHeartbeat(
      "did:mesh:parent",
      { send },
      "sub-agent-x",
      log,
      5_000,
    );

    // Initial fire produced a rejecting promise; flush microtasks.
    await vi.advanceTimersByTimeAsync(0);
    expect(log.warn).toHaveBeenCalled();

    // Still ticks afterwards.
    await vi.advanceTimersByTimeAsync(5_000);
    expect(send).toHaveBeenCalledTimes(2);

    cancel();
  });

  it("cancel() is idempotent", () => {
    const send = vi.fn().mockResolvedValue(undefined);
    const cancel = startTaskProgressHeartbeat(
      "did:mesh:parent",
      { send },
      "sub-agent-x",
      log,
      5_000,
    );
    cancel();
    cancel();
    cancel();
    vi.advanceTimersByTime(20_000);
    expect(send).toHaveBeenCalledTimes(1);
  });
});
