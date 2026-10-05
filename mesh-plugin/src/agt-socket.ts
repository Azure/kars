// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import WebSocket from "ws";

type SocketEvent = { code?: number; data?: unknown };
type Handler = ((event: SocketEvent) => void) | null;
interface RelaySocket {
  readonly readyState: number;
  onopen: Handler;
  onerror: Handler;
  onmessage: Handler;
  onclose: Handler;
  send(data: unknown): void;
  close(code?: number, reason?: string): void;
}

/** Owns the actual socket, including sockets the SDK drops after failed connects. */
export class AgtSocketOwner {
  private active: { socket: RelaySocket; mute(): void; closed: Promise<void> } | null = null;
  private opened = 0;

  constructor(private readonly factory: (url: string) => unknown = url => new WebSocket(url)) {}

  get openCount(): number { return this.opened; }

  readonly create = (url: string): RelaySocket => {
    if (this.active) throw new Error("Previous AGT socket has not been retired");
    const socket = this.factory(url) as RelaySocket;
    let live = true;
    let resolveClosed!: () => void;
    const closed = new Promise<void>(resolve => { resolveClosed = resolve; });
    const facade: RelaySocket = {
      get readyState() { return socket.readyState; },
      onopen: null, onerror: null, onmessage: null, onclose: null,
      send: data => {
        if (!live) throw new Error("AGT socket has been retired");
        socket.send(data);
      },
      close: (code, reason) => socket.close(code, reason),
    };
    this.active = { socket, mute: () => { live = false; }, closed };
    socket.onopen = event => { if (live) { this.opened++; facade.onopen?.(event); } };
    socket.onerror = event => { if (live) facade.onerror?.(event); };
    socket.onmessage = event => { if (live) facade.onmessage?.(event); };
    socket.onclose = event => {
      resolveClosed();
      if (live) {
        live = false;
        facade.onclose?.(event);
      }
    };
    return facade;
  };

  async retire(): Promise<void> {
    const active = this.active;
    if (!active) return;
    active.mute();
    if (active.socket.readyState !== WebSocket.CLOSED) {
      // A failed/connecting SDK client may return from disconnect without closing anything.
      active.socket.close(1000, "Kars transport retiring");
      let timer: ReturnType<typeof setTimeout> | undefined;
      try {
        await Promise.race([
          active.closed,
          new Promise<never>((_, reject) => {
            timer = setTimeout(() => reject(new Error("AGT socket closure was not confirmed")), 5_000);
          }),
        ]);
      } finally {
        if (timer) clearTimeout(timer);
      }
    }
    // Keep ownership on failure. Never open another socket after an ambiguous close.
    this.active = null;
  }
}
