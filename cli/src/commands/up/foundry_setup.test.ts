// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import {
  scoreChatModel,
  pickBestChatModel,
  findEmbeddingModel,
  parseFoundryEndpoint,
  type FoundryDeployment,
} from "./foundry_setup.js";

const dep = (name: string, modelName = name): FoundryDeployment => ({
  name,
  modelName,
  modelVersion: "1",
});

describe("scoreChatModel", () => {
  it("excludes non-chat models", () => {
    expect(scoreChatModel("text-embedding-3-small")).toBeNull();
    expect(scoreChatModel("gpt-image-1")).toBeNull();
    expect(scoreChatModel("FLUX.2-pro")).toBeNull();
    expect(scoreChatModel("whisper")).toBeNull();
    expect(scoreChatModel("tts-1")).toBeNull();
  });

  it("ranks newer families above older", () => {
    expect(scoreChatModel("gpt-5.4")!).toBeGreaterThan(scoreChatModel("gpt-4.1")!);
    expect(scoreChatModel("gpt-5.4")!).toBeGreaterThan(scoreChatModel("gpt-5")!);
    expect(scoreChatModel("gpt-5")!).toBeGreaterThan(scoreChatModel("gpt-4o")!);
  });

  it("prefers the plain flagship over variants within a family", () => {
    const plain = scoreChatModel("gpt-5.4")!;
    expect(plain).toBeGreaterThan(scoreChatModel("gpt-5.4-pro")!);
    expect(scoreChatModel("gpt-5.4-pro")!).toBeGreaterThan(scoreChatModel("gpt-5.4-chat")!);
    expect(scoreChatModel("gpt-5.4-chat")!).toBeGreaterThan(scoreChatModel("gpt-5.4-mini")!);
    expect(scoreChatModel("gpt-5.4-mini")!).toBeGreaterThan(scoreChatModel("gpt-5.4-nano")!);
  });
});

describe("pickBestChatModel", () => {
  it("picks the flagship from a realistic deployment set", () => {
    const deployments = [
      "gpt-5-mini", "text-embedding-3-small", "gpt-4.1", "gpt-5.4-mini",
      "gpt-5.3-chat", "FLUX.2-pro", "gpt-image-1", "gpt-5.4-pro", "gpt-5.4",
    ].map((n) => dep(n));
    expect(pickBestChatModel(deployments)?.name).toBe("gpt-5.4");
  });

  it("returns undefined when no chat model is deployed", () => {
    expect(pickBestChatModel([dep("text-embedding-3-small"), dep("gpt-image-1")])).toBeUndefined();
  });

  it("uses the deployment name when modelName is itself non-chat-looking", () => {
    // deployment named "my-gpt5" wrapping model "gpt-5.4"
    const d: FoundryDeployment = { name: "primary", modelName: "gpt-5.4", modelVersion: "1" };
    expect(pickBestChatModel([d])?.name).toBe("primary");
  });
});

describe("findEmbeddingModel", () => {
  it("prefers 3-large over 3-small over ada", () => {
    const deployments = [dep("ada-002", "text-embedding-ada-002"), dep("small", "text-embedding-3-small"), dep("large", "text-embedding-3-large")];
    expect(findEmbeddingModel(deployments)?.name).toBe("large");
  });
  it("returns undefined when no embedding deployed", () => {
    expect(findEmbeddingModel([dep("gpt-5.4")])).toBeUndefined();
  });
});

describe("parseFoundryEndpoint", () => {
  it("parses account + project from a Foundry project endpoint", () => {
    expect(
      parseFoundryEndpoint("https://azureclaw-foundry-services.services.ai.azure.com/api/projects/azureclaw"),
    ).toEqual({ accountName: "azureclaw-foundry-services", projectName: "azureclaw" });
  });
  it("returns null for a non-project endpoint", () => {
    expect(parseFoundryEndpoint("https://foo.openai.azure.com")).toBeNull();
    expect(parseFoundryEndpoint("not a url")).toBeNull();
  });
});
