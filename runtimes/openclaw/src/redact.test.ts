import { describe, it, expect } from "vitest";
import { redactSecrets } from "./index.js";

describe("redactSecrets", () => {
  it("redacts azcp_1_ pairing tokens (bare, no preceding keyword)", () => {
    const t = "azcp_1_eyJjb250cm9sbGVyX2FtaWQiOiJjdHJsX2FiYzEyMyJ9";
    const out = redactSecrets(`Paired with ${t} successfully`);
    expect(out).not.toContain(t);
    expect(out).toContain("azcp_***");
  });

  it("redacts azcp_2_ (future versions)", () => {
    const out = redactSecrets("token=azcp_2_abc123def456");
    expect(out).toContain("azcp_***");
    expect(out).not.toContain("azcp_2_abc123def456");
  });

  it("redacts Bearer tokens in headers", () => {
    const out = redactSecrets("Authorization: Bearer eyJabc.def.ghi-superSecret");
    expect(out).not.toContain("superSecret");
    expect(out).toMatch(/Bearer \*\*\*/);
  });

  it("redacts Basic auth headers", () => {
    const out = redactSecrets("Basic dXNlcjpwYXNzd29yZA==");
    expect(out).toMatch(/Basic \*\*\*/);
  });

  it("redacts JWTs", () => {
    const jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NSJ9.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
    const out = redactSecrets(`got token ${jwt} from idp`);
    expect(out).toContain("***JWT***");
    expect(out).not.toContain(jwt);
  });

  it("redacts keyword=value style secrets", () => {
    const cases = [
      "api_key=abcdef1234567890",
      "apiKey: 'secret1234567890'",
      "password=\"hunter2hunter2\"",
      "handoff_token: abc12345def67890",
      "admin_token=sk_live_abcdef123456",
      "refresh_token=v1.MzK..longrefresh",
      "access_token: \"xoxp-12345-secret\"",
      "authorization=Basic dXNlcjpwYXNz",
      "invite_code: INV-ABCD1234-EFGH5678",
    ];
    for (const c of cases) {
      const out = redactSecrets(c);
      expect(out, `case: ${c}`).toContain("***");
    }
  });

  it("redacts PEM key blocks", () => {
    const pem = `-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7+secret+material
-----END PRIVATE KEY-----`;
    const out = redactSecrets(`key:\n${pem}\ntrailing`);
    expect(out).not.toContain("MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7");
    expect(out).toContain("***REDACTED***");
  });

  it("passes through non-sensitive text unchanged", () => {
    const txt = "Hello world — no secrets here";
    expect(redactSecrets(txt)).toBe(txt);
  });

  it("handles non-string input safely", () => {
    expect(redactSecrets(null as any)).toBe("null");
    expect(redactSecrets(undefined as any)).toBe("undefined");
    expect(redactSecrets(42 as any)).toBe("42");
  });
});
