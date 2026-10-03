// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { once } from "node:events";
import WebSocket, { WebSocketServer } from "ws";
import { AgtSocketOwner } from "./agt-socket.js";

function socket() {
  return {
    readyState: 0,
    onopen: null as ((e: object) => void) | null,
    onerror: null as ((e: object) => void) | null,
    onmessage: null as ((e: object) => void) | null,
    onclose: null as ((e: object) => void) | null,
    send: vi.fn(), close: vi.fn(),
  };
}

afterEach(() => vi.useRealTimers());

describe("AGT socket custody", () => {
  it.each(["connecting", "open"])("retires a native ws socket while %s before replacement", async state => {
    const relay = new WebSocketServer({ host: "127.0.0.1", port: 0 });
    await once(relay, "listening");
    const address = relay.address();
    if (!address || typeof address === "string") throw new Error("Missing relay address");
    const raw: WebSocket[] = [];
    const owner = new AgtSocketOwner(url => {
      const socket = new WebSocket(url);
      raw.push(socket);
      return socket;
    });
    const url = `ws://127.0.0.1:${address.port}`;
    try {
      const facade = owner.create(url);
      if (state === "open") await once(raw[0], "open");
      expect(raw[0].readyState).toBe(state === "open" ? WebSocket.OPEN : WebSocket.CONNECTING);
      const callback = vi.fn();
      facade.onclose = facade.onerror = callback;
      await owner.retire();
      expect(raw[0].readyState).toBe(WebSocket.CLOSED);
      expect(callback).not.toHaveBeenCalled();
      expect(() => facade.send("stale")).toThrow("retired");
      owner.create(url);
      await once(raw[1], "open");
      expect(raw[1].readyState).toBe(WebSocket.OPEN);
      await owner.retire();
      expect(raw[1].readyState).toBe(WebSocket.CLOSED);
    } finally {
      await owner.retire();
      for (const socket of relay.clients) socket.terminate();
      await new Promise<void>((resolve, reject) => relay.close(error => error ? reject(error) : resolve()));
    }
  });

  it("forwards frames through the supplied factory and preserves its send wrapper", async () => {
    const raw = socket();
    const factory = vi.fn(() => raw);
    const owner = new AgtSocketOwner(factory);
    const facade = owner.create("ws://relay/ws");
    const message = vi.fn();
    facade.onmessage = message;
    raw.onmessage?.({ data: "ciphertext" });
    facade.send("oauth-connect-frame");
    expect(factory).toHaveBeenCalledWith("ws://relay/ws");
    expect(message).toHaveBeenCalledWith({ data: "ciphertext" });
    expect(raw.send).toHaveBeenCalledWith("oauth-connect-frame");
    raw.readyState = 3;
    await owner.retire();
  });

  it("cannot replace a connecting socket until actual close, and fences retired callbacks", async () => {
    const raw = socket();
    const owner = new AgtSocketOwner(() => raw);
    const facade = owner.create("ws://relay");
    const event = vi.fn();
    facade.onopen = facade.onerror = facade.onmessage = facade.onclose = event;
    raw.onopen?.({});
    expect(owner.openCount).toBe(1);
    event.mockClear();
    const retiring = owner.retire();
    expect(raw.close).toHaveBeenCalledOnce();
    expect(() => owner.create("ws://relay")).toThrow("not been retired");
    raw.onmessage?.({}); raw.onerror?.({}); raw.onopen?.({});
    expect(event).not.toHaveBeenCalled();
    expect(() => facade.send("late")).toThrow("retired");
    raw.readyState = 3;
    raw.onclose?.({ code: 1000 });
    await retiring;
    expect(event).not.toHaveBeenCalled();
    expect(owner.openCount).toBe(1);
    owner.create("ws://relay");
    await owner.retire();
  });

  it("retains ownership on timeout and permits retirement only after observed closure", async () => {
    vi.useFakeTimers();
    const raw = socket();
    const owner = new AgtSocketOwner(() => raw);
    owner.create("ws://relay");
    const failure = expect(owner.retire()).rejects.toThrow("closure was not confirmed");
    await vi.advanceTimersByTimeAsync(5_000);
    await failure;
    expect(() => owner.create("ws://relay")).toThrow("not been retired");
    raw.readyState = 3;
    raw.onclose?.({ code: 1000 });
    await owner.retire();
  });

  it("retains ownership when close throws", async () => {
    const raw = socket();
    raw.close.mockImplementation(() => { throw new Error("close failed"); });
    const owner = new AgtSocketOwner(() => raw);
    owner.create("ws://relay");
    await expect(owner.retire()).rejects.toThrow("close failed");
    expect(() => owner.create("ws://relay")).toThrow("not been retired");
    raw.readyState = 3;
    await owner.retire();
  });

  it("reports a remote close once and suppresses later events from that socket", async () => {
    const raw = socket();
    const owner = new AgtSocketOwner(() => raw);
    const facade = owner.create("ws://relay");
    facade.onclose = vi.fn();
    facade.onmessage = vi.fn();
    raw.readyState = 3;
    raw.onclose?.({ code: 1006 });
    raw.onclose?.({ code: 1006 });
    raw.onmessage?.({ data: "late" });
    expect(facade.onclose).toHaveBeenCalledOnce();
    expect(facade.onmessage).not.toHaveBeenCalled();
    await owner.retire();
  });
});
