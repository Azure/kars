import { describe, it, expect } from "vitest";
import { decodeToken, loadPairings, savePairing, getDefaultPairing } from "./pairing.js";

describe("pairing", () => {
  describe("decodeToken", () => {
    it("decodes a valid azcp_1_ token", () => {
      const payload = {
        controller_amid: "ctrl_abc123",
        relay_url: "wss://relay.agentmesh.online/v1/connect",
        registry_url: "https://agentmesh.online/v1",
        secret: "test-secret-value",
      };
      const b64 = Buffer.from(JSON.stringify(payload)).toString("base64url");
      const token = `azcp_1_${b64}`;

      const result = decodeToken(token);
      expect(result).not.toBeNull();
      expect(result!.controller_amid).toBe("ctrl_abc123");
      expect(result!.relay_url).toBe("wss://relay.agentmesh.online/v1/connect");
      expect(result!.secret).toBe("test-secret-value");
    });

    it("returns null for invalid prefix", () => {
      expect(decodeToken("bad_token")).toBeNull();
      expect(decodeToken("azcp_2_something")).toBeNull();
      expect(decodeToken("")).toBeNull();
    });

    it("returns null for missing required fields", () => {
      const b64 = Buffer.from(JSON.stringify({ foo: "bar" })).toString("base64url");
      expect(decodeToken(`azcp_1_${b64}`)).toBeNull();
    });

    it("returns null for garbage base64", () => {
      expect(decodeToken("azcp_1_!!!")).toBeNull();
    });
  });
});
