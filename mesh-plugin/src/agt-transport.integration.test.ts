// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { once } from "node:events";
import { createServer, type IncomingMessage } from "node:http";
import { ed25519 } from "@noble/curves/ed25519.js";
import { WebSocket, WebSocketServer } from "ws";
import { expect, it, vi } from "vitest";
import { AgtTransport } from "./agt-transport.js";
import { deriveCanonicalDid } from "./did.js";

function record(value: unknown): Record<string, unknown> {
  assert.ok(value && typeof value === "object" && !Array.isArray(value));
  return value as Record<string, unknown>;
}

function text(value: unknown): string {
  assert.ok(typeof value === "string");
  return value;
}

async function body(request: IncomingMessage): Promise<Record<string, unknown>> {
  const chunks: Buffer[] = [];
  for await (const chunk of request) chunks.push(Buffer.from(chunk));
  return record(JSON.parse(Buffer.concat(chunks).toString()));
}

// Only registry storage and relay routing are fixtures. Both Kars transports load
// the real installed SDK and perform the actual signing, X3DH and ratchet work.
async function network(corruptPrekey: boolean) {
  const failures: unknown[] = [];
  const keys = new Map<string, Record<string, unknown>>();
  const identities = new Map<string, Buffer>();
  const sockets = new Map<string, WebSocket>();
  const frames: Record<string, unknown>[] = [];
  let registrations = 0;
  let publications = 0;
  let fetches = 0;
  const http = createServer((request, response) => {
    void (async () => {
      response.setHeader("content-type", "application/json");
      if (request.method === "POST" && request.url === "/v1/agents") {
        const data = await body(request);
        const publicKey = Buffer.from(text(data.public_key), "base64url");
        assert.ok(ed25519.verify(
          Buffer.from(text(data.proof), "base64url"),
          Buffer.from(text(data.public_key) + text(data.proof_timestamp)),
          publicKey,
        ));
        const did = deriveCanonicalDid(publicKey);
        identities.set(did, publicKey);
        registrations++;
        response.writeHead(201).end(JSON.stringify({ did }));
        return;
      }
      const match = /^\/v1\/agents\/([^/]+)\/prekeys$/.exec(request.url ?? "");
      assert.ok(match, `Unexpected registry request: ${request.method} ${request.url}`);
      const did = decodeURIComponent(match[1]);
      const [scheme, signer, timestamp, signature] = (request.headers.authorization ?? "").split(" ");
      assert.equal(scheme, "Ed25519-Timestamp");
      const publicKey = identities.get(signer);
      assert.ok(publicKey);
      assert.ok(ed25519.verify(Buffer.from(signature, "base64url"), Buffer.from(timestamp), publicKey));
      if (request.method === "PUT") {
        assert.equal(signer, did);
        const data = await body(request);
        assert.equal(text(data.identity_key_ed), publicKey.toString("base64url"));
        assert.ok(Array.isArray(data.one_time_pre_keys));
        assert.equal(data.one_time_pre_keys.length, 4);
        keys.set(did, data);
        publications++;
        response.end("{}");
      } else {
        assert.equal(request.method, "GET");
        const data = keys.get(did);
        assert.ok(data);
        const { one_time_pre_keys: oneTimeKeys, ...bundle } = data;
        assert.ok(Array.isArray(oneTimeKeys));
        const signed = record(bundle.signed_pre_key);
        const signature = corruptPrekey
          ? Buffer.alloc(64).toString("base64url") : signed.signature;
        fetches++;
        response.end(JSON.stringify({
          ...bundle,
          signed_pre_key: { ...signed, signature },
          one_time_pre_key: oneTimeKeys.shift() ?? null,
        }));
      }
    })().catch(error => {
      failures.push(error);
      response.writeHead(500).end(JSON.stringify({ error: String(error) }));
    });
  });
  const relay = new WebSocketServer({ server: http, path: "/ws" });
  relay.on("connection", socket => {
    let authenticated: string | undefined;
    socket.on("message", bytes => {
      try {
        const frame = record(JSON.parse(bytes.toString()));
        frames.push(frame);
        if (frame.type === "connect") {
          const publicKey = Buffer.from(text(frame.public_key), "base64");
          assert.equal(frame.from, deriveCanonicalDid(publicKey));
          assert.ok(ed25519.verify(
            Buffer.from(text(frame.signature), "base64"),
            Buffer.from(text(frame.timestamp)), publicKey,
          ));
          authenticated = text(frame.from);
          sockets.set(authenticated, socket);
        } else {
          assert.ok(authenticated);
          if (frame.type === "fetch_pending") {
            socket.send(JSON.stringify({ v: 1, type: "pending_messages", messages: [] }));
          } else if (frame.type !== "ack" && frame.type !== "disconnect") {
            assert.equal(frame.from, authenticated);
            const peer = sockets.get(text(frame.to));
            assert.ok(peer, "Relay target must be connected");
            peer.send(bytes.toString());
          }
        }
      } catch (error) {
        failures.push(error);
        socket.close(1011, "Fixture rejected invalid SDK frame");
      }
    });
  });
  http.listen(0, "127.0.0.1");
  await once(http, "listening");
  const address = http.address();
  assert.ok(address && typeof address !== "string");
  const url = `http://127.0.0.1:${address.port}`;
  const clients = Array.from({ length: 2 }, () => {
    const signingPrivateKey = randomBytes(32);
    const signingPublicKey = ed25519.getPublicKey(signingPrivateKey);
    return new AgtTransport({
      relayUrl: url, registryUrl: url, oneTimePreKeyCount: 4,
      identity: {
        agentId: deriveCanonicalDid(Buffer.from(signingPublicKey)),
        signingPrivateKey, signingPublicKey,
      },
    });
  });
  return {
    clients, frames, failures,
    counts: () => ({ registrations, publications, fetches }),
    async close() {
      try {
        await Promise.all(clients.map(client => client.disconnect()));
      } finally {
        for (const socket of relay.clients) socket.terminate();
        await new Promise<void>((resolve, reject) => relay.close(error => error ? reject(error) : resolve()));
        http.closeAllConnections();
        await new Promise<void>((resolve, reject) => http.close(error => error ? reject(error) : resolve()));
      }
    },
  };
}

it.each([false, true])("uses the real pinned SDK with corruptPrekey=%s", async corruptPrekey => {
  const fixture = await network(corruptPrekey);
  const [alice, bob] = fixture.clients;
  try {
    await Promise.all(fixture.clients.map(client => client.connect()));
    expect(fixture.counts()).toEqual({ registrations: 2, publications: 2, fetches: 0 });
    if (corruptPrekey) {
      await expect(alice.send(bob.currentDid, { secret: "must-not-be-sent" })).rejects.toThrow(/signature|bundle|pre.?key/i);
      expect(fixture.frames.filter(frame => frame.type === "message")).toHaveLength(0);
      expect(bob.getInbox()).toEqual([]);
    } else {
      for (let turn = 0; turn < 6; turn++) {
        const [sender, receiver] = turn % 2 ? [bob, alice] : [alice, bob];
        const content = { secret: `private-integration-turn-${turn}` };
        await sender.send(receiver.currentDid, content);
        await vi.waitFor(() => {
          expect(receiver.getInbox()).toEqual([expect.objectContaining({
            from: sender.currentDid, content, security: "encrypted",
          })]);
        });
        expect(receiver.drainInbox()).toHaveLength(1);
      }
      const messages = fixture.frames.filter(frame => frame.type === "message");
      expect(messages).toHaveLength(6);
      expect(JSON.stringify(messages)).not.toContain("private-integration-turn-");
      for (const message of messages) expect(typeof message.ciphertext).toBe("string");
      expect(fixture.counts().fetches).toBe(1);
    }
    expect(fixture.failures).toEqual([]);
  } finally {
    await fixture.close();
  }
}, 15_000);
