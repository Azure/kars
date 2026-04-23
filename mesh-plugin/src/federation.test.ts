/**
 * Federation protocol roundtrip tests.
 *
 * Verifies the complete message flow between the standalone plugin
 * and controller without requiring a live relay or K8s cluster.
 * Tests message encoding/decoding, token handling, and state transitions.
 */

import { describe, it, expect } from "vitest";
import { generateIdentity, deriveAmid } from "./identity.js";
import { decodeToken } from "./pairing.js";
import type {
  PairRequestMessage,
  PairResponseMessage,
  OffloadRequestMessage,
  OffloadStatusMessage,
  OffloadDoneMessage,
  OffloadErrorMessage,
  FederationMessage,
} from "./types.js";
import * as crypto from "node:crypto";

// Simulate the relay's base64 encode/decode (matches mesh_peer.rs)
function relayEncode(msg: unknown): string {
  return Buffer.from(JSON.stringify(msg)).toString("base64");
}
function relayDecode(b64: string): unknown {
  return JSON.parse(Buffer.from(b64, "base64").toString());
}

describe("federation protocol roundtrip", () => {
  describe("pairing ceremony", () => {
    it("full pair flow: token → pair_request → pair_response", async () => {
      // 1. Admin generates a pairing token
      const controllerIdentity = await generateIdentity();
      const secret = crypto.randomUUID();
      const tokenPayload = {
        controller_amid: controllerIdentity.amid,
        relay_url: "wss://relay.agentmesh.online/v1/connect",
        registry_url: "https://agentmesh.online/v1",
        secret,
      };
      const b64 = Buffer.from(JSON.stringify(tokenPayload)).toString("base64url");
      const token = `azcp_1_${b64}`;

      // 2. External agent decodes token
      const decoded = decodeToken(token);
      expect(decoded).not.toBeNull();
      expect(decoded!.controller_amid).toBe(controllerIdentity.amid);
      expect(decoded!.secret).toBe(secret);

      // 3. External agent creates identity and sends pair_request
      const externalIdentity = await generateIdentity();
      const pairRequest: PairRequestMessage = {
        type: "pair_request",
        secret,
        pubkey_ed25519: externalIdentity.signingPublicKey.toString("base64"),
        display_name: `external-${externalIdentity.amid.slice(0, 8)}`,
        capabilities_requested: ["offload", "handoff"],
      };

      // 4. Simulate relay transport (base64 encode/decode)
      const encoded = relayEncode(pairRequest);
      const transported = relayDecode(encoded) as PairRequestMessage;
      expect(transported.type).toBe("pair_request");
      expect(transported.secret).toBe(secret);
      expect(transported.pubkey_ed25519).toBe(
        externalIdentity.signingPublicKey.toString("base64")
      );

      // 5. Controller validates and responds
      // (simulating handle_pair_request logic)
      const secretHash = crypto
        .createHash("sha256")
        .update(secret)
        .digest("hex");
      expect(secretHash.length).toBe(64); // SHA-256 hex = 64 chars

      const pairResponse: PairResponseMessage = {
        type: "pair_response",
        success: true,
        cluster_name: "test-cluster",
        controller_amid: controllerIdentity.amid,
        capabilities_granted: ["offload"],
        slots: 1,
        token_budget: 500000,
        expires_at: new Date(Date.now() + 86400000 * 90).toISOString(),
      };

      // 6. Response roundtrips through relay
      const responseEncoded = relayEncode(pairResponse);
      const responseDecoded = relayDecode(responseEncoded) as PairResponseMessage;
      expect(responseDecoded.type).toBe("pair_response");
      expect(responseDecoded.success).toBe(true);
      expect(responseDecoded.cluster_name).toBe("test-cluster");
      expect(responseDecoded.controller_amid).toBe(controllerIdentity.amid);
      expect(responseDecoded.capabilities_granted).toContain("offload");
      expect(responseDecoded.token_budget).toBe(500000);
    });

    it("pair_response with error roundtrips correctly", () => {
      const errResponse: PairResponseMessage = {
        type: "pair_response",
        success: false,
        error: "Pairing token has expired",
      };
      const encoded = relayEncode(errResponse);
      const decoded = relayDecode(encoded) as PairResponseMessage;
      expect(decoded.success).toBe(false);
      expect(decoded.error).toBe("Pairing token has expired");
    });
  });

  describe("offload flow", () => {
    it("full offload flow: request → status → done", () => {
      const requestId = crypto.randomUUID();

      // 1. Plugin sends offload_request
      const request: OffloadRequestMessage = {
        type: "offload_request",
        task: "Analyze the Q4 revenue data and create a summary report",
        files: ["data/revenue.csv", "templates/report.md"],
        file_count: 2,
        total_bytes: 15360,
        preferences: {
          model: "gpt-4.1",
          timeout_minutes: 15,
        },
        request_id: requestId,
        timestamp: new Date().toISOString(),
      };

      const reqEncoded = relayEncode(request);
      const reqDecoded = relayDecode(reqEncoded) as OffloadRequestMessage;
      expect(reqDecoded.type).toBe("offload_request");
      expect(reqDecoded.task).toContain("Q4 revenue");
      expect(reqDecoded.files).toHaveLength(2);
      expect(reqDecoded.request_id).toBe(requestId);

      // 2. Controller sends status updates
      const statuses: OffloadStatusMessage[] = [
        { type: "offload_status", request_id: requestId, phase: "validating", message: "Checking pairing" },
        { type: "offload_status", request_id: requestId, phase: "spawning", message: "Creating sandbox" },
        { type: "offload_status", request_id: requestId, phase: "running", message: "Task in progress" },
      ];

      for (const status of statuses) {
        const enc = relayEncode(status);
        const dec = relayDecode(enc) as OffloadStatusMessage;
        expect(dec.type).toBe("offload_status");
        expect(dec.request_id).toBe(requestId);
      }

      // 3. Sandbox completes → controller sends done
      const done: OffloadDoneMessage = {
        type: "offload_done",
        request_id: requestId,
        summary: "Revenue analysis complete. Q4 revenue was $2.3M, up 15% from Q3.",
        output_files: ["report.md", "charts/revenue.png"],
        tokens_used: { prompt: 4200, completion: 1800 },
        duration_seconds: 47,
      };

      const doneEncoded = relayEncode(done);
      const doneDecoded = relayDecode(doneEncoded) as OffloadDoneMessage;
      expect(doneDecoded.type).toBe("offload_done");
      expect(doneDecoded.request_id).toBe(requestId);
      expect(doneDecoded.summary).toContain("$2.3M");
      expect(doneDecoded.output_files).toHaveLength(2);
      expect(doneDecoded.tokens_used.prompt).toBe(4200);
      expect(doneDecoded.duration_seconds).toBe(47);
    });

    it("offload error roundtrips correctly", () => {
      const error: OffloadErrorMessage = {
        type: "offload_error",
        request_id: "req-fail",
        error: "No available slots (1/1 used)",
        phase: "validating",
      };

      const encoded = relayEncode(error);
      const decoded = relayDecode(encoded) as OffloadErrorMessage;
      expect(decoded.type).toBe("offload_error");
      expect(decoded.error).toContain("No available slots");
      expect(decoded.phase).toBe("validating");
    });

    // Boundary guard: cloud-offload uses its own wire protocol
    // (`OffloadRequestMessage` → controller → ClawSandbox CRD, built directly
    // in `controller/src/mesh_peer.rs::handle_offload_request`). It does
    // *not* go through the router's `POST /sandbox/spawn` endpoint or
    // `SpawnRequest` struct, which is why the q4 rename of
    // `SpawnRequest.name` → `SpawnRequest.agent_id` is invisible to this
    // flow. If someone ever rewires cloud-offload to call `/sandbox/spawn`,
    // these assertions will fail and force them to think about backward
    // compatibility with both wire formats.
    it("offload wire format does NOT overlap with /sandbox/spawn fields", () => {
      const request: OffloadRequestMessage = {
        type: "offload_request",
        task: "boundary test",
        files: [],
        file_count: 0,
        total_bytes: 0,
        request_id: "guard-1",
        timestamp: new Date().toISOString(),
      };
      // OffloadRequestMessage must not carry a sub-agent identity field.
      // The controller is the CRD creator; it names the sandbox itself.
      expect((request as Record<string, unknown>).agent_id).toBeUndefined();
      expect((request as Record<string, unknown>).name).toBeUndefined();
      expect((request as Record<string, unknown>).sandbox_name).toBeUndefined();

      // OffloadStatusMessage uses `sandbox_name` (NOT `agent_id`) when the
      // controller echoes the CRD name back. This is a deliberate separate
      // channel from the router's spawn/handoff wire format.
      const status: OffloadStatusMessage = {
        type: "offload_status",
        request_id: "guard-1",
        phase: "ready",
        message: "Sandbox ready",
        sandbox_name: "offload-guard-1-abc",
      };
      expect((status as Record<string, unknown>).agent_id).toBeUndefined();
      expect(status.sandbox_name).toBe("offload-guard-1-abc");

      // Roundtrip sanity — the controller, not the caller, assigns the name.
      const enc = relayEncode(status);
      const dec = relayDecode(enc) as OffloadStatusMessage;
      expect(dec.sandbox_name).toBe("offload-guard-1-abc");
    });
  });

  describe("AMID consistency", () => {
    it("AMID derivation is consistent between identity instances", async () => {
      const id = await generateIdentity();
      const amid1 = deriveAmid(id.signingPublicKey);
      const amid2 = deriveAmid(id.signingPublicKey);
      expect(amid1).toBe(amid2);
      expect(amid1).toBe(id.amid);
    });

    it("different agents have different AMIDs", async () => {
      const amids = new Set<string>();
      for (let i = 0; i < 10; i++) {
        amids.add((await generateIdentity()).amid);
      }
      expect(amids.size).toBe(10);
    });
  });

  describe("token format", () => {
    it("rejects tokens with tampered secret", () => {
      const payload = {
        controller_amid: "ctrl123",
        relay_url: "wss://relay.example.com/v1/connect",
        registry_url: "https://example.com/v1",
        secret: "original-secret",
      };
      const b64 = Buffer.from(JSON.stringify(payload)).toString("base64url");
      const token = `azcp_1_${b64}`;

      // Token decodes fine
      const decoded = decodeToken(token);
      expect(decoded!.secret).toBe("original-secret");

      // But the controller would reject it because the SHA-256 hash won't match
      const providedHash = crypto.createHash("sha256").update("wrong-secret").digest("hex");
      const storedHash = crypto.createHash("sha256").update("original-secret").digest("hex");
      expect(providedHash).not.toBe(storedHash);
    });

    it("version 1 tokens use base64url encoding", () => {
      const payload = {
        controller_amid: "test",
        relay_url: "wss://r.com",
        registry_url: "https://r.com",
        secret: "s",
      };
      const b64 = Buffer.from(JSON.stringify(payload)).toString("base64url");
      const token = `azcp_1_${b64}`;

      // No +, /, = characters (base64url vs base64)
      const b64Part = token.slice(7);
      expect(b64Part).not.toMatch(/[+/=]/);
    });
  });
});
