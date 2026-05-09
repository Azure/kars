// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { LocalInbox } from "./local-inbox.js";

describe("LocalInbox", () => {
  it("delivers messages into FIFO inbox when no waiter is registered", () => {
    const inbox = new LocalInbox({ buildHash: "test" });
    const claimed = inbox.deliver("amid:alice", { hello: "world" });
    expect(claimed).toBe(false);
    const items = inbox.getInbox();
    expect(items).toHaveLength(1);
    expect(items[0].from).toBe("amid:alice");
    expect(items[0].content).toEqual({ hello: "world" });
    expect(items[0].id).toMatch(/^mesh-/);
  });

  it("hands message to a matching waiter and skips inbox push", async () => {
    const inbox = new LocalInbox({ buildHash: "test" });
    const wait = inbox.waitForMessage<string>(
      (content) => (typeof content === "object" && content !== null && "tag" in content
        ? (content as { tag: string }).tag
        : null),
      1000,
    );
    inbox.deliver("amid:bob", { tag: "got-it" });
    await expect(wait).resolves.toBe("got-it");
    expect(inbox.getInbox()).toHaveLength(0);
  });

  it("evicts oldest messages when maxSize is exceeded", () => {
    const inbox = new LocalInbox({ buildHash: "test", maxSize: 2 });
    inbox.deliver("amid:a", { n: 1 });
    inbox.deliver("amid:a", { n: 2 });
    inbox.deliver("amid:a", { n: 3 });
    expect(inbox.getInbox()).toHaveLength(2);
    expect(inbox.getDiagnostics().fifo_dropped).toBe(1);
  });

  it("markRead is idempotent and tracks counters", () => {
    const inbox = new LocalInbox({ buildHash: "test" });
    inbox.deliver("amid:a", { n: 1 });
    const id = inbox.getInbox()[0].id;
    expect(inbox.markRead([id])).toBe(1);
    expect(inbox.markRead([id])).toBe(0);
    expect(inbox.getUnreadCount()).toBe(0);
    expect(inbox.getDiagnostics().read_total).toBe(1);
  });

  it("consumeInbox claims matching entries and leaves others", () => {
    const inbox = new LocalInbox({ buildHash: "test" });
    inbox.deliver("amid:a", { kind: "x" });
    inbox.deliver("amid:b", { kind: "y" });
    const claimed = inbox.consumeInbox(
      (m) => typeof m.content === "object" && m.content !== null
        && (m.content as { kind: string }).kind === "x",
    );
    expect(claimed).toHaveLength(1);
    expect(inbox.getInbox()).toHaveLength(1);
  });

  it("waitForInbox wakes on next deliver and times out otherwise", async () => {
    const inbox = new LocalInbox({ buildHash: "test" });
    const woken = inbox.waitForInbox(2000);
    setTimeout(() => inbox.deliver("amid:a", { n: 1 }), 10);
    await expect(woken).resolves.toBe(true);

    const timedOut = inbox.waitForInbox(20);
    await expect(timedOut).resolves.toBe(false);
  });

  it("waitForMessage with consume:false leaves the entry in the inbox", async () => {
    const inbox = new LocalInbox({ buildHash: "test" });
    inbox.deliver("amid:a", { n: 1 });
    const got = await inbox.waitForMessage<number>(
      (c) => (typeof c === "object" && c !== null && "n" in c ? (c as { n: number }).n : null),
      1000,
      { consume: false },
    );
    expect(got).toBe(1);
    expect(inbox.getInbox()).toHaveLength(1);
  });

  it("drainInbox returns and clears all entries", () => {
    const inbox = new LocalInbox({ buildHash: "test" });
    inbox.deliver("amid:a", { n: 1 });
    inbox.deliver("amid:a", { n: 2 });
    expect(inbox.drainInbox()).toHaveLength(2);
    expect(inbox.getInbox()).toHaveLength(0);
  });
});
