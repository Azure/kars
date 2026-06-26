// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { isFoundryProjectHost } from "./upgrade.js";

describe("isFoundryProjectHost", () => {
  it("accepts a real Foundry project endpoint", () => {
    expect(isFoundryProjectHost("https://acct.services.ai.azure.com/api/projects/p")).toBe(true);
    expect(isFoundryProjectHost("https://services.ai.azure.com")).toBe(true);
  });

  it("rejects look-alike hosts that a substring match would accept", () => {
    // The classic incomplete-sanitization bypass.
    expect(isFoundryProjectHost("https://services.ai.azure.com.evil.com/api/projects/p")).toBe(false);
    expect(isFoundryProjectHost("https://evilservices.ai.azure.com/x")).toBe(false);
    expect(isFoundryProjectHost("https://attacker.com/?q=services.ai.azure.com")).toBe(false);
  });

  it("rejects plain Azure OpenAI and empty/garbage input", () => {
    expect(isFoundryProjectHost("https://my-aoai.openai.azure.com")).toBe(false);
    expect(isFoundryProjectHost("")).toBe(false);
    expect(isFoundryProjectHost("not a url")).toBe(false);
  });
});
