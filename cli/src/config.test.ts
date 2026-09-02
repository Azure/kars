// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect, vi, beforeEach } from "vitest";
import { join } from "path";
import { homedir } from "os";

// Mock fs before importing module under test
vi.mock("fs", () => ({
  existsSync: vi.fn(),
  readFileSync: vi.fn(),
  writeFileSync: vi.fn(),
  mkdirSync: vi.fn(),
  chmodSync: vi.fn(),
  renameSync: vi.fn(),
  rmSync: vi.fn(),
}));

import {
  clearDeploymentContext, loadConfig, loadContext, saveContext,
  CONFIG_DIR, CONFIG_FILE, CREDENTIALS_FILE, SECRETS_FILE,
  loadSecrets, saveSecrets, setSecret, deleteSecret, resolveSecret,
  listSecretVariants, KNOWN_SECRETS,
} from "./config.js";
import {
  existsSync,
  readFileSync,
  writeFileSync,
  mkdirSync,
  chmodSync,
  renameSync,
  rmSync,
} from "fs";

const mockExistsSync = vi.mocked(existsSync);
const mockReadFileSync = vi.mocked(readFileSync);
const mockWriteFileSync = vi.mocked(writeFileSync);
const mockMkdirSync = vi.mocked(mkdirSync);
const mockChmodSync = vi.mocked(chmodSync);
const mockRenameSync = vi.mocked(renameSync);
const mockRmSync = vi.mocked(rmSync);

beforeEach(() => {
  vi.clearAllMocks();
});

describe("config path resolution", () => {
  it("CONFIG_DIR is under home directory", () => {
    expect(CONFIG_DIR).toBe(join(homedir(), ".kars"));
  });

  it("CONFIG_FILE is config.json inside CONFIG_DIR", () => {
    expect(CONFIG_FILE).toBe(join(homedir(), ".kars", "config.json"));
  });

  it("CREDENTIALS_FILE is credentials inside CONFIG_DIR", () => {
    expect(CREDENTIALS_FILE).toBe(join(homedir(), ".kars", "credentials"));
  });
});

describe("loadConfig", () => {
  it("returns null when config file does not exist", () => {
    mockExistsSync.mockReturnValue(false);
    expect(loadConfig()).toBeNull();
  });

  it("returns null when credentials file does not exist", () => {
    mockExistsSync.mockImplementation((p) =>
      String(p).endsWith("config.json"),
    );
    expect(loadConfig()).toBeNull();
  });

  it("loads config and credentials when both exist", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockImplementation((p) => {
      const path = String(p);
      if (path.endsWith("config.json"))
        return JSON.stringify({ endpoint: "https://test.openai.azure.com", model: "gpt-4.1" });
      if (path.endsWith("credentials")) return "sk-test-key-1234567890\n";
      return "";
    });

    const config = loadConfig();
    expect(config).toEqual({
      endpoint: "https://test.openai.azure.com",
      model: "gpt-4.1",
      apiKey: "sk-test-key-1234567890",
      foundryProjectEndpoint: undefined,
      provider: "foundry",
      firstRunCompleted: false,
    });
  });

  it("defaults model to gpt-4.1 when not specified", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockImplementation((p) => {
      const path = String(p);
      if (path.endsWith("config.json"))
        return JSON.stringify({ endpoint: "https://test.openai.azure.com" });
      if (path.endsWith("credentials")) return "sk-test-key-1234567890";
      return "";
    });

    const config = loadConfig();
    expect(config?.model).toBe("gpt-4.1");
  });

  it("loads GitHub Models provider with sane defaults", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockImplementation((p) => {
      const path = String(p);
      if (path.endsWith("config.json"))
        return JSON.stringify({
          endpoint: "https://models.github.ai/inference",
          provider: "github-models",
        });
      if (path.endsWith("credentials")) return "ghp_testpat_1234567890";
      return "";
    });

    const config = loadConfig();
    expect(config?.provider).toBe("github-models");
    expect(config?.endpoint).toBe("https://models.github.ai/inference");
    // Default model for GitHub Models is gpt-4o-mini, not gpt-4.1
    expect(config?.model).toBe("gpt-4o-mini");
    expect(config?.apiKey).toBe("ghp_testpat_1234567890");
  });

  it("treats unknown provider strings as foundry", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockImplementation((p) => {
      const path = String(p);
      if (path.endsWith("config.json"))
        return JSON.stringify({
          endpoint: "https://test.openai.azure.com",
          provider: "bogus-provider",
        });
      if (path.endsWith("credentials")) return "sk-test-key";
      return "";
    });

    expect(loadConfig()?.provider).toBe("foundry");
  });

  it("returns null when endpoint is missing", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockImplementation((p) => {
      const path = String(p);
      if (path.endsWith("config.json")) return JSON.stringify({ model: "gpt-4.1" });
      if (path.endsWith("credentials")) return "sk-test-key";
      return "";
    });

    expect(loadConfig()).toBeNull();
  });

  it("returns null when apiKey is empty", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockImplementation((p) => {
      const path = String(p);
      if (path.endsWith("config.json"))
        return JSON.stringify({ endpoint: "https://test.openai.azure.com" });
      if (path.endsWith("credentials")) return "   \n";
      return "";
    });

    expect(loadConfig()).toBeNull();
  });

  it("returns null on corrupted JSON", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockImplementation((p) => {
      const path = String(p);
      if (path.endsWith("config.json")) return "{invalid json";
      if (path.endsWith("credentials")) return "sk-key";
      return "";
    });

    expect(loadConfig()).toBeNull();
  });

  it("includes foundryProjectEndpoint when present", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockImplementation((p) => {
      const path = String(p);
      if (path.endsWith("config.json"))
        return JSON.stringify({
          endpoint: "https://test.openai.azure.com",
          foundryProjectEndpoint: "https://test.services.ai.azure.com/api/projects/myproj",
        });
      if (path.endsWith("credentials")) return "sk-test-key-1234567890";
      return "";
    });

    const config = loadConfig();
    expect(config?.foundryProjectEndpoint).toBe(
      "https://test.services.ai.azure.com/api/projects/myproj",
    );
  });
});

describe("loadContext", () => {
  it("returns null when context file does not exist", () => {
    mockExistsSync.mockReturnValue(false);
    expect(loadContext()).toBeNull();
  });

  it("loads deployment context from JSON", () => {
    mockExistsSync.mockReturnValue(true);
    const ctx = {
      subscription: "sub-123",
      region: "eastus2",
      resourceGroup: "rg-test",
      aksCluster: "aks-test",
      acrLoginServer: "myacr.azurecr.io",
      acrName: "myacr",
    };
    mockReadFileSync.mockReturnValue(JSON.stringify(ctx));

    const result = loadContext();
    expect(result).toEqual(ctx);
  });

  it("loads legacy deployment context without a subscription", () => {
    mockExistsSync.mockReturnValue(true);
    const legacyCtx = {
      region: "eastus2",
      resourceGroup: "rg-test",
      aksCluster: "aks-test",
      acrLoginServer: "myacr.azurecr.io",
    };
    mockReadFileSync.mockReturnValue(JSON.stringify(legacyCtx));

    expect(loadContext()).toEqual(legacyCtx);
  });

  it("returns null on corrupted context file", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockImplementation(() => {
      throw new Error("file corrupt");
    });

    expect(loadContext()).toBeNull();
  });
});

describe("saveContext", () => {
  it("creates config directory and writes context", () => {
    const ctx = { subscription: "sub-123", region: "eastus2" };
    saveContext(ctx);

    expect(mockMkdirSync).toHaveBeenCalledWith(CONFIG_DIR, { recursive: true });
    expect(mockWriteFileSync).toHaveBeenCalledTimes(1);
    const written = JSON.parse(mockWriteFileSync.mock.calls[0][1] as string);
    expect(written.subscription).toBe("sub-123");
    expect(written.region).toBe("eastus2");
    expect(written.savedAt).toBeDefined();
  });

  it("atomically replaces context with a restrictive 0o600 temporary file", () => {
    saveContext({ region: "westus" });
    const temporaryFile = mockWriteFileSync.mock.calls[0][0] as string;
    expect(temporaryFile).toMatch(/context\.json\.\d+\.\d+\.tmp$/);
    expect(mockWriteFileSync.mock.calls[0][2]).toEqual({
      encoding: "utf-8",
      mode: 0o600,
    });
    expect(mockRenameSync).toHaveBeenCalledWith(
      temporaryFile,
      join(CONFIG_DIR, "context.json"),
    );
  });

  it("removes an incomplete temporary file when atomic replacement fails", () => {
    mockRenameSync.mockImplementationOnce(() => {
      throw new Error("rename failed");
    });

    expect(() => saveContext({ region: "westus" })).toThrow("rename failed");
    const temporaryFile = mockWriteFileSync.mock.calls[0][0] as string;
    expect(mockRmSync).toHaveBeenCalledWith(temporaryFile, { force: true });
  });
});

describe("clearDeploymentContext", () => {
  it("idempotently removes only context.json", () => {
    clearDeploymentContext();
    clearDeploymentContext();

    expect(mockRmSync).toHaveBeenCalledTimes(2);
    expect(mockRmSync).toHaveBeenNthCalledWith(
      1,
      join(CONFIG_DIR, "context.json"),
      { force: true },
    );
    expect(mockRmSync).toHaveBeenNthCalledWith(
      2,
      join(CONFIG_DIR, "context.json"),
      { force: true },
    );
  });
});

describe("secrets store", () => {
  it("SECRETS_FILE is under config directory", () => {
    expect(SECRETS_FILE).toBe(join(homedir(), ".kars", "secrets.json"));
  });

  it("loadSecrets returns empty object when file does not exist", () => {
    mockExistsSync.mockReturnValue(false);
    expect(loadSecrets()).toEqual({});
  });

  it("loadSecrets parses secrets.json", () => {
    mockExistsSync.mockImplementation((path) => {
      if (typeof path === "string" && path.endsWith("secrets.json")) return true;
      return false;
    });
    mockReadFileSync.mockReturnValue(JSON.stringify({ "telegram-token": "bot123:xyz" }));

    const secrets = loadSecrets();
    expect(secrets["telegram-token"]).toBe("bot123:xyz");
  });

  it("loadSecrets migrates legacy credentials file", () => {
    mockExistsSync.mockImplementation((path) => {
      if (typeof path === "string" && path.endsWith("secrets.json")) return false;
      if (typeof path === "string" && path.endsWith("credentials")) return true;
      return false;
    });
    mockReadFileSync.mockReturnValue("sk-legacy-api-key-1234567890");

    const secrets = loadSecrets();
    expect(secrets["azure-openai-key"]).toBe("sk-legacy-api-key-1234567890");
    // Should have written the migrated secrets
    expect(mockWriteFileSync).toHaveBeenCalled();
  });

  it("saveSecrets writes with mode 600", () => {
    saveSecrets({ "brave-api-key": "bsk-test" });

    expect(mockMkdirSync).toHaveBeenCalledWith(CONFIG_DIR, { recursive: true });
    expect(mockWriteFileSync).toHaveBeenCalledWith(
      SECRETS_FILE,
      expect.stringContaining("brave-api-key"),
      "utf-8",
    );
    expect(mockChmodSync).toHaveBeenCalledWith(SECRETS_FILE, 0o600);
  });

  it("setSecret adds a key to existing secrets and normalizes telegram-token bot prefix", () => {
    mockExistsSync.mockImplementation((path) => {
      if (typeof path === "string" && path.endsWith("secrets.json")) return true;
      return false;
    });
    mockReadFileSync.mockReturnValue(JSON.stringify({ existing: "value" }));

    setSecret("telegram-token", "bot999:abc");

    const written = JSON.parse(mockWriteFileSync.mock.calls[0][1] as string);
    expect(written["existing"]).toBe("value");
    // setSecret strips the leading `bot` so grammY's prefix doesn't collide.
    expect(written["telegram-token"]).toBe("999:abc");
  });

  it("setSecret leaves non-telegram secrets untouched", () => {
    mockExistsSync.mockImplementation((path) => {
      if (typeof path === "string" && path.endsWith("secrets.json")) return true;
      return false;
    });
    mockReadFileSync.mockReturnValue(JSON.stringify({}));

    setSecret("brave-api-key", "bot-shouldnt-strip");

    const written = JSON.parse(mockWriteFileSync.mock.calls[0][1] as string);
    expect(written["brave-api-key"]).toBe("bot-shouldnt-strip");
  });

  it("deleteSecret removes a key", () => {
    mockExistsSync.mockImplementation((path) => {
      if (typeof path === "string" && path.endsWith("secrets.json")) return true;
      return false;
    });
    mockReadFileSync.mockReturnValue(JSON.stringify({ "telegram-token": "bot123", "brave-api-key": "bsk-1" }));

    const result = deleteSecret("telegram-token");
    expect(result).toBe(true);

    const written = JSON.parse(mockWriteFileSync.mock.calls[0][1] as string);
    expect(written["telegram-token"]).toBeUndefined();
    expect(written["brave-api-key"]).toBe("bsk-1");
  });

  it("deleteSecret returns false for missing key", () => {
    mockExistsSync.mockReturnValue(false);
    expect(deleteSecret("nonexistent")).toBe(false);
  });

  it("resolveSecret priority: flag > secrets > env", () => {
    // Flag takes priority
    expect(resolveSecret("flag-value", "telegram-token")).toBe("flag-value");

    // Falls back to secrets.json
    mockExistsSync.mockImplementation((path) => {
      if (typeof path === "string" && path.endsWith("secrets.json")) return true;
      return false;
    });
    mockReadFileSync.mockReturnValue(JSON.stringify({ "telegram-token": "stored-value" }));
    expect(resolveSecret(undefined, "telegram-token")).toBe("stored-value");

    // Falls back to env var
    mockExistsSync.mockReturnValue(false);
    process.env.TELEGRAM_BOT_TOKEN = "env-value";
    expect(resolveSecret(undefined, "telegram-token")).toBe("env-value");
    delete process.env.TELEGRAM_BOT_TOKEN;
  });

  it("KNOWN_SECRETS has correct env mappings", () => {
    expect(KNOWN_SECRETS["telegram-token"].env).toBe("TELEGRAM_BOT_TOKEN");
    expect(KNOWN_SECRETS["slack-token"].env).toBe("SLACK_BOT_TOKEN");
    expect(KNOWN_SECRETS["azure-openai-key"].env).toBe("AZURE_OPENAI_API_KEY");
  });

  it("listSecretVariants finds base key and dot-suffixed variants", () => {
    mockExistsSync.mockImplementation((path) => {
      if (typeof path === "string" && path.endsWith("secrets.json")) return true;
      return false;
    });
    mockReadFileSync.mockReturnValue(JSON.stringify({
      "telegram-token": "bot-default",
      "telegram-token.cloud": "bot-cloud",
      "telegram-token.dev": "bot-dev",
      "slack-token": "xoxb-slack",
    }));

    const variants = listSecretVariants("telegram-token");
    expect(variants).toHaveLength(3);
    expect(variants[0]).toEqual({ key: "telegram-token", label: "default", value: "bot-default" });
    expect(variants[1]).toEqual({ key: "telegram-token.cloud", label: "cloud", value: "bot-cloud" });
    expect(variants[2]).toEqual({ key: "telegram-token.dev", label: "dev", value: "bot-dev" });

    // Unrelated keys not included
    const slackVariants = listSecretVariants("slack-token");
    expect(slackVariants).toHaveLength(1);
    expect(slackVariants[0].label).toBe("default");
  });

  it("listSecretVariants returns empty for missing base key", () => {
    mockExistsSync.mockReturnValue(false);
    expect(listSecretVariants("nonexistent")).toEqual([]);
  });
});
