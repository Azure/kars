// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { decodeToken } from "./pair.js";

describe("pair command", () => {
  describe("token encoding/decoding", () => {
    it("decodes a valid azcp_1_ token", () => {
      const payload = {
        controller_amid: "ctrl_abc123",
        relay_url: "wss://relay.agentmesh.online/v1/connect",
        registry_url: "https://agentmesh.online/v1",
        secret: "test-secret-value",
      };
      const json = JSON.stringify(payload);
      const b64 = Buffer.from(json).toString("base64url");
      const token = `azcp_1_${b64}`;

      const result = decodeToken(token);
      expect(result).not.toBeNull();
      expect(result!.controller_amid).toBe("ctrl_abc123");
      expect(result!.relay_url).toBe("wss://relay.agentmesh.online/v1/connect");
      expect(result!.registry_url).toBe("https://agentmesh.online/v1");
      expect(result!.secret).toBe("test-secret-value");
    });

    it("returns null for invalid prefix", () => {
      expect(decodeToken("invalid_token")).toBeNull();
      expect(decodeToken("azcp_2_something")).toBeNull();
      expect(decodeToken("")).toBeNull();
    });

    it("returns null for malformed base64", () => {
      expect(decodeToken("azcp_1_!!!not-valid-base64!!!")).toBeNull();
    });

    it("returns null for valid base64 but missing fields", () => {
      const incomplete = Buffer.from(JSON.stringify({ foo: "bar" })).toString("base64url");
      expect(decodeToken(`azcp_1_${incomplete}`)).toBeNull();
    });

    it("roundtrips with encode → decode", () => {
      // Simulate what the generate command produces
      const payload = {
        controller_amid: "ctr_8Kp2MnXq9rTvWz",
        relay_url: "wss://relay.example.com",
        registry_url: "https://registry.example.com",
        secret: "VGhpcyBpcyBhIHRlc3Qgc2VjcmV0",
      };
      const json = JSON.stringify(payload);
      const token = `azcp_1_${Buffer.from(json).toString("base64url")}`;

      const decoded = decodeToken(token);
      expect(decoded).toEqual(payload);
    });
  });
});
