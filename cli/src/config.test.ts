import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { join } from "path";
import { homedir } from "os";

// Mock fs before importing module under test
vi.mock("fs", () => ({
  existsSync: vi.fn(),
  readFileSync: vi.fn(),
  writeFileSync: vi.fn(),
  mkdirSync: vi.fn(),
  chmodSync: vi.fn(),
}));

import {
  loadConfig, loadContext, saveContext,
  CONFIG_DIR, CONFIG_FILE, CREDENTIALS_FILE, SECRETS_FILE,
  loadSecrets, saveSecrets, getSecret, setSecret, deleteSecret, resolveSecret,
  KNOWN_SECRETS,
} from "./config.js";
import { existsSync, readFileSync, writeFileSync, mkdirSync, chmodSync } from "fs";

const mockExistsSync = vi.mocked(existsSync);
const mockReadFileSync = vi.mocked(readFileSync);
const mockWriteFileSync = vi.mocked(writeFileSync);
const mockMkdirSync = vi.mocked(mkdirSync);
const mockChmodSync = vi.mocked(chmodSync);

beforeEach(() => {
  vi.clearAllMocks();
});

describe("config path resolution", () => {
  it("CONFIG_DIR is under home directory", () => {
    expect(CONFIG_DIR).toBe(join(homedir(), ".azureclaw"));
  });

  it("CONFIG_FILE is config.json inside CONFIG_DIR", () => {
    expect(CONFIG_FILE).toBe(join(homedir(), ".azureclaw", "config.json"));
  });

  it("CREDENTIALS_FILE is credentials inside CONFIG_DIR", () => {
    expect(CREDENTIALS_FILE).toBe(join(homedir(), ".azureclaw", "credentials"));
  });
});

describe("loadConfig", () => {
  it("returns null when config file does not exist", () => {
    mockExistsSync.mockReturnValue(false);
    expect(loadConfig()).toBeNull();
  });

  it("returns null when credentials file does not exist", () => {
    mockExistsSync.mockImplementation((p) =>
      String(p).endsWith("config.json") ? true : false,
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

  it("sets restrictive file permissions (0o600)", () => {
    saveContext({ region: "westus" });
    expect(mockChmodSync).toHaveBeenCalledWith(
      expect.stringContaining("context.json"),
      0o600,
    );
  });
});

describe("secrets store", () => {
  it("SECRETS_FILE is under config directory", () => {
    expect(SECRETS_FILE).toBe(join(homedir(), ".azureclaw", "secrets.json"));
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

  it("setSecret adds a key to existing secrets", () => {
    mockExistsSync.mockImplementation((path) => {
      if (typeof path === "string" && path.endsWith("secrets.json")) return true;
      return false;
    });
    mockReadFileSync.mockReturnValue(JSON.stringify({ existing: "value" }));

    setSecret("telegram-token", "bot999:abc");

    const written = JSON.parse(mockWriteFileSync.mock.calls[0][1] as string);
    expect(written["existing"]).toBe("value");
    expect(written["telegram-token"]).toBe("bot999:abc");
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
});
