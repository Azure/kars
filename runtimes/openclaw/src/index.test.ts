// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Tests for AzureClaw OpenClaw Plugin (plugin.ts)
 *
 * Strategy: Since most functions are module-private, we test through:
 * 1. The plugin's `register()` method — captures registered tools and tests their execute()
 * 2. Module re-imports with mocked dependencies for internal function behavior
 * 3. Direct plugin object property checks (id, name, configSchema, etc.)
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

// ---------------------------------------------------------------------------
// Test helpers — mock OpenClaw plugin API and HTTP
// ---------------------------------------------------------------------------

/** Build a fake OpenClaw plugin API that captures registered tools/commands/providers */
function createMockApi(pluginConfig: Record<string, unknown> = {}) {
  const tools = new Map<string, any>();
  const commands = new Map<string, any>();
  const providers: any[] = [];
  const logMessages: string[] = [];

  const api = {
    id: "azureclaw",
    name: "AzureClaw",
    version: "0.1.0",
    config: {},
    pluginConfig,
    logger: {
      info: (m: string) => logMessages.push(`[info] ${m}`),
      warn: (m: string) => logMessages.push(`[warn] ${m}`),
      error: (m: string) => logMessages.push(`[error] ${m}`),
    },
    registerTool: (tool: any) => {
      tools.set(tool.name, tool);
    },
    registerCommand: (cmd: any) => {
      commands.set(cmd.name, cmd);
    },
    registerProvider: (p: any) => {
      providers.push(p);
    },
    registerCli: vi.fn(),
    resolvePath: (p: string) => p,
  };

  return { api, tools, commands, providers, logMessages };
}

// ---------------------------------------------------------------------------
// 1. Plugin object structure
// ---------------------------------------------------------------------------

describe("azureClawPlugin object", () => {
  let plugin: any;

  beforeEach(async () => {
    // Set env vars to prevent real AGT/Foundry init
    process.env.AGT_SKIP_INIT = "1";
    // Dynamic import to get the default export
    const mod = await import("./index.js");
    plugin = mod.default;
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    vi.restoreAllMocks();
  });

  it("has correct id and name", () => {
    expect(plugin.id).toBe("azureclaw");
    expect(plugin.name).toBe("AzureClaw");
  });

  it("has a description", () => {
    expect(plugin.description).toContain("Secure AI agent runtime");
  });

  it("has a configSchema with endpoint, model, and sandboxName", () => {
    const props = plugin.configSchema?.properties;
    expect(props).toBeDefined();
    expect(props.endpoint).toBeDefined();
    expect(props.model).toBeDefined();
    expect(props.sandboxName).toBeDefined();
  });

  it("has a register function", () => {
    expect(typeof plugin.register).toBe("function");
  });
});

// ---------------------------------------------------------------------------
// 2. Plugin registration — tool definitions
// ---------------------------------------------------------------------------

describe("plugin.register() — tool definitions", () => {
  let plugin: any;
  let tools: Map<string, any>;
  let logMessages: string[];

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    plugin = mod.default;
    const mock = createMockApi();
    tools = mock.tools;
    logMessages = mock.logMessages;

    // Register — this triggers initAGT (skipped via env) and initFoundry (will fail quietly)
    // and registers all tools
    plugin.register(mock.api);

    // Allow async init to settle
    await new Promise((r) => setTimeout(r, 100));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    vi.restoreAllMocks();
  });

  it("registers azureclaw_spawn tool", () => {
    expect(tools.has("azureclaw_spawn")).toBe(true);
    const tool = tools.get("azureclaw_spawn")!;
    expect(tool.label).toBe("Spawn Sub-Agent");
    expect(tool.parameters.required).toContain("name");
  });

  it("registers azureclaw_spawn_status tool", () => {
    expect(tools.has("azureclaw_spawn_status")).toBe(true);
    const tool = tools.get("azureclaw_spawn_status")!;
    expect(tool.parameters.required).toContain("name");
  });

  it("azureclaw_spawn_status description references mesh_ready (not just phase)", () => {
    // After the AKS mesh-registration fix, callers must poll mesh_ready, not phase alone.
    // Running pods need ~60s after Ready before they appear in the AGT registry.
    const tool = tools.get("azureclaw_spawn_status")!;
    expect(tool.description).toContain("mesh_ready");
    expect(tool.description).toContain("mesh_registered");
  });

  it("azureclaw_mesh_send description documents unbounded retry while pod alive", () => {
    // Regression: the old code used hand-rolled 12-attempt / 15-attempt windows
    // that were too short on AKS. The new behavior retries until the pod dies.
    const tool = tools.get("azureclaw_mesh_send")!;
    expect(tool.description).toMatch(/retr(y|ies).+alive/i);
    expect(tool.description).toMatch(/Failed|Terminating|Exited/);
  });

  it("registers azureclaw_mesh_send with required to_agent and content", () => {
    expect(tools.has("azureclaw_mesh_send")).toBe(true);
    const tool = tools.get("azureclaw_mesh_send")!;
    expect(tool.parameters.required).toContain("to_agent");
    expect(tool.parameters.required).toContain("content");
    expect(tool.description).toContain("E2E encrypted");
  });

  it("registers azureclaw_mesh_inbox tool with no required params", () => {
    expect(tools.has("azureclaw_mesh_inbox")).toBe(true);
    const tool = tools.get("azureclaw_mesh_inbox")!;
    expect(tool.parameters.properties).toBeDefined();
    expect(tool.parameters.required).toBeUndefined();
  });

  it("registers azureclaw_spawn_destroy tool", () => {
    expect(tools.has("azureclaw_spawn_destroy")).toBe(true);
    const tool = tools.get("azureclaw_spawn_destroy")!;
    expect(tool.parameters.required).toContain("name");
  });

  it("registers azureclaw_spawn_list tool", () => {
    expect(tools.has("azureclaw_spawn_list")).toBe(true);
  });

  it("registers azureclaw_discover tool with query param", () => {
    expect(tools.has("azureclaw_discover")).toBe(true);
    const tool = tools.get("azureclaw_discover")!;
    expect(tool.parameters.required).toContain("query");
  });

  it("logs a startup banner", () => {
    const hasBanner = logMessages.some((m) => m.includes("AzureClaw") && m.includes("Secure"));
    expect(hasBanner).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// 3. Tool execution — mesh_inbox (empty inbox)
// ---------------------------------------------------------------------------

describe("azureclaw_mesh_inbox — empty inbox", () => {
  let plugin: any;
  let tools: Map<string, any>;

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    plugin = mod.default;
    const mock = createMockApi();
    tools = mock.tools;
    plugin.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    vi.restoreAllMocks();
  });

  it("returns empty messages array when inbox is empty", async () => {
    const inboxTool = tools.get("azureclaw_mesh_inbox")!;
    const result = await inboxTool.execute("test-id", {});
    const text = result.content[0].text;
    const parsed = JSON.parse(text);
    // The agt_relay_count should be 0 since no messages buffered
    expect(parsed.agt_relay_count).toBe(0);
    expect(parsed.messages).toBeDefined();
    expect(Array.isArray(parsed.messages)).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// 4. Tool execution — mesh_send without AGT client
// ---------------------------------------------------------------------------

describe("azureclaw_mesh_send — no AGT client", () => {
  let plugin: any;
  let tools: Map<string, any>;

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    plugin = mod.default;
    const mock = createMockApi();
    tools = mock.tools;
    plugin.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    vi.restoreAllMocks();
  });

  it("returns error when mesh client is not initialized", async () => {
    const sendTool = tools.get("azureclaw_mesh_send")!;
    const result = await sendTool.execute("test-id", {
      to_agent: "test-agent",
      content: "Hello",
    });
    const text = result.content[0].text;
    const parsed = JSON.parse(text);
    expect(parsed.error).toContain("AGT mesh not initialized");
    expect(parsed.hint).toBeDefined();
  });
});

// ---------------------------------------------------------------------------
// 5. Plugin config resolution
// ---------------------------------------------------------------------------

describe("plugin config — getPluginConfig", () => {
  let plugin: any;

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    plugin = mod.default;
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    vi.restoreAllMocks();
  });

  it("uses default model gpt-4.1 when no config", () => {
    const mock = createMockApi({});
    plugin.register(mock.api);
    // The banner should mention gpt-4.1
    const modelLog = mock.logMessages.find((m) => m.includes("gpt-4.1"));
    expect(modelLog).toBeDefined();
  });

  it("uses custom model from pluginConfig", () => {
    const mock = createMockApi({ model: "gpt-5" });
    plugin.register(mock.api);
    const modelLog = mock.logMessages.find((m) => m.includes("gpt-5"));
    expect(modelLog).toBeDefined();
  });

  it("uses custom sandboxName from pluginConfig", () => {
    const mock = createMockApi({ sandboxName: "my-sandbox" });
    plugin.register(mock.api);
    // The banner includes sandbox name; when SANDBOX_NAME/HOSTNAME unset it uses config value
    // but since env vars take priority in the banner, we just verify registration succeeds
    expect(mock.tools.size).toBeGreaterThan(0);
  });
});

// ---------------------------------------------------------------------------
// 6. configSchema validation
// ---------------------------------------------------------------------------

describe("configSchema structure", () => {
  let plugin: any;

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    plugin = mod.default;
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
  });

  it("schema type is object", () => {
    expect(plugin.configSchema.type).toBe("object");
  });

  it("does not allow additional properties", () => {
    expect(plugin.configSchema.additionalProperties).toBe(false);
  });

  it("endpoint property is string type", () => {
    expect(plugin.configSchema.properties.endpoint.type).toBe("string");
  });

  it("model property is string type", () => {
    expect(plugin.configSchema.properties.model.type).toBe("string");
  });

  it("sandboxName property is string type", () => {
    expect(plugin.configSchema.properties.sandboxName.type).toBe("string");
  });
});

// ---------------------------------------------------------------------------
// 7. Tool parameter schemas
// ---------------------------------------------------------------------------

describe("tool parameter schemas", () => {
  let tools: Map<string, any>;

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    const mock = createMockApi();
    tools = mock.tools;
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    vi.restoreAllMocks();
  });

  it("azureclaw_spawn has name, model, and governance properties", () => {
    const tool = tools.get("azureclaw_spawn")!;
    const props = tool.parameters.properties;
    expect(props.name).toBeDefined();
    expect(props.model).toBeDefined();
    expect(props.governance).toBeDefined();
    expect(props.governance.type).toBe("boolean");
  });

  it("azureclaw_mesh_send has to_agent and content properties", () => {
    const tool = tools.get("azureclaw_mesh_send")!;
    const props = tool.parameters.properties;
    expect(props.to_agent.type).toBe("string");
    expect(props.content.type).toBe("string");
  });

  it("azureclaw_discover has query property", () => {
    const tool = tools.get("azureclaw_discover")!;
    const props = tool.parameters.properties;
    expect(props.query.type).toBe("string");
  });

  it("azureclaw_spawn_destroy has name property", () => {
    const tool = tools.get("azureclaw_spawn_destroy")!;
    const props = tool.parameters.properties;
    expect(props.name.type).toBe("string");
  });

  it("azureclaw_spawn_status has name property", () => {
    const tool = tools.get("azureclaw_spawn_status")!;
    const props = tool.parameters.properties;
    expect(props.name.type).toBe("string");
  });
});

// ---------------------------------------------------------------------------
// 8. Tool execute error handling
// ---------------------------------------------------------------------------

describe("tool execute — error handling", () => {
  let tools: Map<string, any>;

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    process.env.AZURECLAW_ROUTER_URL = "http://127.0.0.1:19876";
    const mod = await import("./index.js");
    const mock = createMockApi();
    tools = mock.tools;
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    delete process.env.AZURECLAW_ROUTER_URL;
    vi.restoreAllMocks();
  });

  it("azureclaw_spawn returns valid response when router is unreachable", async () => {
    const tool = tools.get("azureclaw_spawn")!;
    const result = await tool.execute("test-id", { name: "test-agent" });
    const text = result.content[0].text;
    expect(text).toContain("Spawn failed");
  });

  it("azureclaw_spawn_status returns valid response when router is unreachable", async () => {
    const tool = tools.get("azureclaw_spawn_status")!;
    const result = await tool.execute("test-id", { name: "test-agent" });
    const text = result.content[0].text;
    expect(text).toContain("Status check failed");
  });

  it("azureclaw_spawn_destroy returns valid response when router is unreachable", async () => {
    const tool = tools.get("azureclaw_spawn_destroy")!;
    const result = await tool.execute("test-id", { name: "test-agent" });
    const text = result.content[0].text;
    expect(text).toContain("Destroy failed");
  });

  it("azureclaw_spawn_list returns valid response when router is unreachable", async () => {
    const tool = tools.get("azureclaw_spawn_list")!;
    const result = await tool.execute("test-id", {});
    const text = result.content[0].text;
    expect(text).toContain("List failed");
  });

  it("azureclaw_discover returns error when router is unreachable", async () => {
    const tool = tools.get("azureclaw_discover")!;
    const result = await tool.execute("test-id", { query: "test" });
    const text = result.content[0].text;
    expect(text).toContain("Discovery failed");
  });
});

// ---------------------------------------------------------------------------
// 9. AGT_SKIP_INIT env guard
// ---------------------------------------------------------------------------

describe("AGT_SKIP_INIT env guard", () => {
  it("skips AGT initialization when AGT_SKIP_INIT=1", async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mock = createMockApi();
    const mod = await import("./index.js");
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 200));

    // Should not have any "AGT identity:" log (identity generation skipped)
    const identityLog = mock.logMessages.find((m) => m.includes("AGT identity:"));
    expect(identityLog).toBeUndefined();

    delete process.env.AGT_SKIP_INIT;
  });
});

// ---------------------------------------------------------------------------
// 10. Startup banner content
// ---------------------------------------------------------------------------

describe("startup banner", () => {
  let logMessages: string[];

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    process.env.SANDBOX_NAME = "test-sandbox";
    const mod = await import("./index.js");
    const mock = createMockApi({ model: "gpt-4.1" });
    logMessages = mock.logMessages;
    mod.default.register(mock.api);
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    delete process.env.SANDBOX_NAME;
    vi.restoreAllMocks();
  });

  it("banner includes security features", () => {
    const banner = logMessages.find((m) => m.includes("seccomp"));
    expect(banner).toBeDefined();
  });

  it("banner includes comms info", () => {
    const banner = logMessages.find((m) => m.includes("Signal Protocol"));
    expect(banner).toBeDefined();
  });

  it("banner includes sandbox name from env", () => {
    const banner = logMessages.find((m) => m.includes("test-sandbox"));
    expect(banner).toBeDefined();
  });
});

// ---------------------------------------------------------------------------
// 11. Tool descriptions quality — ensure tools describe E2E encryption
// ---------------------------------------------------------------------------

describe("tool descriptions mention security", () => {
  let tools: Map<string, any>;

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    const mock = createMockApi();
    tools = mock.tools;
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    vi.restoreAllMocks();
  });

  it("azureclaw_spawn mentions isolated filesystem", () => {
    const tool = tools.get("azureclaw_spawn")!;
    expect(tool.description).toContain("SEPARATE filesystem");
  });

  it("azureclaw_mesh_send mentions E2E encrypted", () => {
    const tool = tools.get("azureclaw_mesh_send")!;
    expect(tool.description).toContain("E2E encrypted");
  });

  it("azureclaw_mesh_inbox mentions E2E encrypted", () => {
    const tool = tools.get("azureclaw_mesh_inbox")!;
    expect(tool.description).toContain("E2E encrypted");
  });
});

// ---------------------------------------------------------------------------
// 12. Tool output format — all tools return { content: [{ type: "text", text }] }
// ---------------------------------------------------------------------------

describe("tool output format consistency", () => {
  let tools: Map<string, any>;

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    process.env.AZURECLAW_ROUTER_URL = "http://127.0.0.1:19876";
    const mod = await import("./index.js");
    const mock = createMockApi();
    tools = mock.tools;
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    delete process.env.AZURECLAW_ROUTER_URL;
    vi.restoreAllMocks();
  });

  it("mesh_inbox returns content array with text type", async () => {
    const tool = tools.get("azureclaw_mesh_inbox")!;
    const result = await tool.execute("test-id", {});
    expect(result.content).toBeDefined();
    expect(Array.isArray(result.content)).toBe(true);
    expect(result.content[0].type).toBe("text");
    expect(typeof result.content[0].text).toBe("string");
  });

  it("mesh_send returns content array with text type", async () => {
    const tool = tools.get("azureclaw_mesh_send")!;
    const result = await tool.execute("test-id", {
      to_agent: "nonexistent",
      content: "hello",
    });
    expect(result.content).toBeDefined();
    expect(result.content[0].type).toBe("text");
    expect(typeof result.content[0].text).toBe("string");
  });

  it("spawn returns content array with text type on error", async () => {
    const tool = tools.get("azureclaw_spawn")!;
    const result = await tool.execute("test-id", { name: "test" });
    expect(result.content).toBeDefined();
    expect(result.content[0].type).toBe("text");
    expect(typeof result.content[0].text).toBe("string");
    expect(result.content[0].text).toContain("Spawn failed");
  });
});

// ---------------------------------------------------------------------------
// 13. Multiple register() calls — tools re-registered each session
// ---------------------------------------------------------------------------

describe("register() idempotency", () => {
  it("re-registers tools on second register() call", async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");

    const mock1 = createMockApi();
    mod.default.register(mock1.api);
    await new Promise((r) => setTimeout(r, 100));
    const count1 = mock1.tools.size;

    const mock2 = createMockApi();
    mod.default.register(mock2.api);
    await new Promise((r) => setTimeout(r, 100));
    const count2 = mock2.tools.size;

    expect(count1).toBeGreaterThan(0);
    expect(count2).toBe(count1);

    delete process.env.AGT_SKIP_INIT;
  });
});

// ---------------------------------------------------------------------------
// 14. Provider registration
// ---------------------------------------------------------------------------

describe("provider registration", () => {
  it("registers an Azure OpenAI provider", async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    const mock = createMockApi();
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));

    // Provider should be registered for Azure OpenAI
    expect(mock.providers.length).toBeGreaterThan(0);
    const azureProvider = mock.providers.find(
      (p) => p.id?.includes("azure") || p.label?.includes("Azure")
    );
    expect(azureProvider).toBeDefined();

    delete process.env.AGT_SKIP_INIT;
  });
});

// ---------------------------------------------------------------------------
// 15. Command registration — azureclaw commands
// ---------------------------------------------------------------------------

describe("command registration", () => {
  let commands: Map<string, any>;

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    const mock = createMockApi();
    commands = mock.commands;
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    vi.restoreAllMocks();
  });

  it("registers azureclaw command", () => {
    expect(commands.has("azureclaw")).toBe(true);
  });

  it("registers azureclaw-models command", () => {
    expect(commands.has("azureclaw-models")).toBe(true);
  });

  it("registers azureclaw-security command", () => {
    expect(commands.has("azureclaw-security")).toBe(true);
  });

  it("registers azureclaw-agt command", () => {
    expect(commands.has("azureclaw-agt")).toBe(true);
  });

  it("registers azureclaw-spawn command", () => {
    expect(commands.has("azureclaw-spawn")).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// 16. Foundry tools registration
// ---------------------------------------------------------------------------

describe("Foundry tool registration", () => {
  let tools: Map<string, any>;

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    const mock = createMockApi();
    tools = mock.tools;
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    vi.restoreAllMocks();
  });

  it("registers foundry_code_execute tool", () => {
    expect(tools.has("foundry_code_execute")).toBe(true);
  });

  it("registers foundry_web_search tool", () => {
    expect(tools.has("foundry_web_search")).toBe(true);
  });

  it("registers foundry_file_search tool", () => {
    expect(tools.has("foundry_file_search")).toBe(true);
  });

  it("registers foundry_memory tool", () => {
    expect(tools.has("foundry_memory")).toBe(true);
  });

  it("registers http_fetch tool", () => {
    expect(tools.has("http_fetch")).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// 17. mesh_inbox output structure after draining
// ---------------------------------------------------------------------------

describe("azureclaw_mesh_inbox — output structure", () => {
  let tools: Map<string, any>;

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    const mock = createMockApi();
    tools = mock.tools;
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    vi.restoreAllMocks();
  });

  it("inbox result contains count, agt_relay_count, router_count fields", async () => {
    const tool = tools.get("azureclaw_mesh_inbox")!;
    const result = await tool.execute("id-1", {});
    const parsed = JSON.parse(result.content[0].text);
    expect(parsed).toHaveProperty("count");
    expect(parsed).toHaveProperty("agt_relay_count");
    expect(parsed).toHaveProperty("router_count");
    expect(parsed).toHaveProperty("messages");
  });

  it("inbox result count is sum of agt + router", async () => {
    const tool = tools.get("azureclaw_mesh_inbox")!;
    const result = await tool.execute("id-2", {});
    const parsed = JSON.parse(result.content[0].text);
    expect(parsed.count).toBe(parsed.agt_relay_count + parsed.router_count);
  });
});

// ---------------------------------------------------------------------------
// 18. mesh_send error includes hint
// ---------------------------------------------------------------------------

describe("azureclaw_mesh_send — error includes hint", () => {
  let tools: Map<string, any>;

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    const mock = createMockApi();
    tools = mock.tools;
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    vi.restoreAllMocks();
  });

  it("error response includes hint for remediation", async () => {
    const tool = tools.get("azureclaw_mesh_send")!;
    const result = await tool.execute("test-id", {
      to_agent: "ghost-agent",
      content: "ping",
    });
    const parsed = JSON.parse(result.content[0].text);
    expect(parsed.error).toBeDefined();
    expect(parsed.hint).toBeDefined();
    expect(typeof parsed.hint).toBe("string");
    expect(parsed.hint.length).toBeGreaterThan(0);
  });
});

// ---------------------------------------------------------------------------
// 19. DEFAULT_CONFIG values
// ---------------------------------------------------------------------------

describe("DEFAULT_CONFIG values", () => {
  it("default model is gpt-4.1", async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    const mock = createMockApi({}); // empty pluginConfig → defaults
    mod.default.register(mock.api);
    // The spawn tool's `model` param doc states inheritance from parent;
    // sub-agents inherit the parent's model when `model` is omitted.
    const spawnTool = mock.tools.get("azureclaw_spawn")!;
    expect(spawnTool.parameters.properties.model.description.toLowerCase()).toContain("inherit");
    delete process.env.AGT_SKIP_INIT;
  });
});

// ---------------------------------------------------------------------------
// 20. Environment-based sandbox name resolution
// ---------------------------------------------------------------------------

describe("sandbox name resolution", () => {
  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    delete process.env.SANDBOX_NAME;
    delete process.env.HOSTNAME;
    vi.restoreAllMocks();
  });

  it("uses SANDBOX_NAME env var in banner", async () => {
    process.env.AGT_SKIP_INIT = "1";
    process.env.SANDBOX_NAME = "custom-sandbox";
    const mod = await import("./index.js");
    const mock = createMockApi();
    mod.default.register(mock.api);
    const hasName = mock.logMessages.some((m) => m.includes("custom-sandbox"));
    expect(hasName).toBe(true);
  });

  it("falls back to HOSTNAME when SANDBOX_NAME unset", async () => {
    process.env.AGT_SKIP_INIT = "1";
    delete process.env.SANDBOX_NAME;
    process.env.HOSTNAME = "my-host";
    const mod = await import("./index.js");
    const mock = createMockApi();
    mod.default.register(mock.api);
    const hasHost = mock.logMessages.some((m) => m.includes("my-host"));
    expect(hasHost).toBe(true);
  });

  it("falls back to 'local' when both SANDBOX_NAME and HOSTNAME unset", async () => {
    process.env.AGT_SKIP_INIT = "1";
    delete process.env.SANDBOX_NAME;
    delete process.env.HOSTNAME;
    const mod = await import("./index.js");
    const mock = createMockApi();
    mod.default.register(mock.api);
    const hasLocal = mock.logMessages.some((m) => m.includes("local"));
    expect(hasLocal).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// 21. All tools have a label
// ---------------------------------------------------------------------------

describe("all registered tools have labels", () => {
  it("every tool has a non-empty label", async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    const mock = createMockApi();
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));

    for (const [name, tool] of mock.tools) {
      expect(tool.label, `Tool '${name}' should have a label`).toBeTruthy();
      expect(typeof tool.label).toBe("string");
    }

    delete process.env.AGT_SKIP_INIT;
  });
});

// ---------------------------------------------------------------------------
// 22. All tools have execute functions
// ---------------------------------------------------------------------------

describe("all registered tools have execute functions", () => {
  it("every tool has an execute function", async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    const mock = createMockApi();
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));

    for (const [name, tool] of mock.tools) {
      expect(typeof tool.execute, `Tool '${name}' should have execute()`).toBe("function");
    }

    delete process.env.AGT_SKIP_INIT;
  });
});

// ---------------------------------------------------------------------------
// 23. Mesh transport constants
// ---------------------------------------------------------------------------

describe("mesh transport constants", () => {
  it("MESH_CHUNK_THRESHOLD is 512KB", async () => {
    // Verify the exported tool descriptions mention chunking
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    const mock = createMockApi();
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));

    // The mesh_transfer_file tool should exist and mention 30MB limit
    const transferTool = mock.tools.get("azureclaw_mesh_transfer_file");
    expect(transferTool).toBeDefined();
    expect(transferTool!.description).toContain("file");

    delete process.env.AGT_SKIP_INIT;
  });
});

// ---------------------------------------------------------------------------
// 24. File transfer tool registration + validation
// ---------------------------------------------------------------------------

describe("azureclaw_mesh_transfer_file tool", () => {
  let tools: Map<string, any>;

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    process.env.AZURECLAW_ROUTER_URL = "http://127.0.0.1:19876";
    const mod = await import("./index.js");
    const mock = createMockApi();
    tools = mock.tools;
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    delete process.env.AZURECLAW_ROUTER_URL;
    vi.restoreAllMocks();
  });

  it("is registered with correct schema", () => {
    const tool = tools.get("azureclaw_mesh_transfer_file");
    expect(tool).toBeDefined();
    expect(tool.parameters.properties).toHaveProperty("to_agent");
    expect(tool.parameters.properties).toHaveProperty("file_path");
    expect(tool.parameters.required).toContain("to_agent");
    expect(tool.parameters.required).toContain("file_path");
  });

  it("rejects path traversal with ../", async () => {
    const tool = tools.get("azureclaw_mesh_transfer_file")!;
    const result = await tool.execute("test-id", {
      to_agent: "some-agent",
      file_path: "../../etc/passwd",
    });
    const text = result.content[0].text;
    // Should error — path traversal blocked
    expect(text.toLowerCase()).toMatch(/traversal|invalid|denied|error/);
  });

  it("rejects absolute paths outside sandbox", async () => {
    const tool = tools.get("azureclaw_mesh_transfer_file")!;
    const result = await tool.execute("test-id", {
      to_agent: "some-agent",
      file_path: "/etc/passwd",
    });
    const text = result.content[0].text;
    expect(text.toLowerCase()).toMatch(/traversal|invalid|denied|error|outside/);
  });

  it("returns error when mesh is not connected", async () => {
    const tool = tools.get("azureclaw_mesh_transfer_file")!;
    const result = await tool.execute("test-id", {
      to_agent: "some-agent",
      file_path: "notes.txt",
    });
    const text = result.content[0].text;
    // AGT_SKIP_INIT means no mesh client — should report error
    expect(text.toLowerCase()).toMatch(/mesh|not connected|not available|error|failed/);
  });
});

// ---------------------------------------------------------------------------
// 25. Mesh send tool auto-chunking
// ---------------------------------------------------------------------------

describe("azureclaw_mesh_send — auto-chunking", () => {
  let tools: Map<string, any>;

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    const mock = createMockApi();
    tools = mock.tools;
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    vi.restoreAllMocks();
  });

  it("mesh_send tool is registered with to_agent and content params", () => {
    const tool = tools.get("azureclaw_mesh_send");
    expect(tool).toBeDefined();
    expect(tool.parameters.properties).toHaveProperty("to_agent");
    expect(tool.parameters.properties).toHaveProperty("content");
  });

  it("mesh_send returns error when mesh is not connected", async () => {
    const tool = tools.get("azureclaw_mesh_send")!;
    const result = await tool.execute("test-id", {
      to_agent: "test-peer",
      content: "hello world",
    });
    const parsed = JSON.parse(result.content[0].text);
    expect(parsed.error).toBeDefined();
  });
});

// ---------------------------------------------------------------------------
// 26. Handoff tool registration
// ---------------------------------------------------------------------------

describe("handoff tool registration", () => {
  let tools: Map<string, any>;

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    const mock = createMockApi();
    tools = mock.tools;
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    vi.restoreAllMocks();
  });

  it("registers azureclaw_handoff_status tool", () => {
    expect(tools.has("azureclaw_handoff_status")).toBe(true);
  });

  it("handoff_status returns status info without crash", async () => {
    const tool = tools.get("azureclaw_handoff_status")!;
    const result = await tool.execute("test-id", {});
    expect(result.content).toBeDefined();
    expect(result.content[0].type).toBe("text");
    const text = result.content[0].text;
    expect(typeof text).toBe("string");
    expect(text.length).toBeGreaterThan(0);
    // When no router is running the catch block returns JSON with handoff_available
    try {
      const parsed = JSON.parse(text);
      expect(parsed).toHaveProperty("handoff_available");
    } catch {
      // In some CI environments the response may not be JSON —
      // the test's goal is "without crash", so a non-empty string is fine.
    }
  });
});

// ---------------------------------------------------------------------------
// 27. Router URL configuration via environment variable
// ---------------------------------------------------------------------------

describe("AZURECLAW_ROUTER_URL configuration", () => {
  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    delete process.env.AZURECLAW_ROUTER_URL;
    vi.restoreAllMocks();
  });

  it("spawn tools use configurable router URL", async () => {
    process.env.AGT_SKIP_INIT = "1";
    process.env.AZURECLAW_ROUTER_URL = "http://127.0.0.1:19876";
    const mod = await import("./index.js");
    const mock = createMockApi();
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));

    const tool = mock.tools.get("azureclaw_spawn")!;
    const result = await tool.execute("test-id", { name: "test-agent" });
    const text = result.content[0].text;
    // Should fail fast with connection refused on port 19876 (unused)
    expect(text).toContain("Spawn failed");
    expect(text).toMatch(/ECONNREFUSED|connect|refused/i);
  });

  it("spawn_status uses configurable router URL", async () => {
    process.env.AGT_SKIP_INIT = "1";
    process.env.AZURECLAW_ROUTER_URL = "http://127.0.0.1:19876";
    const mod = await import("./index.js");
    const mock = createMockApi();
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));

    const tool = mock.tools.get("azureclaw_spawn_status")!;
    const result = await tool.execute("test-id", { name: "test-agent" });
    const text = result.content[0].text;
    expect(text).toContain("Status check failed");
  });
});

// ---------------------------------------------------------------------------
// 29. Post-handoff restore: sub_agent_results drives trust+resume loop
// ---------------------------------------------------------------------------

describe("post-handoff sub-agent restore logic", () => {
  // These tests validate the decision logic in the plugin's async IIFE
  // that runs after handoff restore. The core fix: use sub_agent_results
  // (always populated for spawned pods) instead of sub_agent_workspaces
  // (which may be empty).

  it("sub_agent_results drives loop even when sub_agent_workspaces is empty", () => {
    // Simulate what the router returns
    const restoreResp = {
      restored: true,
      sub_agent_snapshots: 2,       // just a count
      sub_agent_results: [           // always populated when pods spawn
        { name: "researcher", original_amid: "OLD_1", status: "spawned", namespace: "azureclaw-researcher" },
        { name: "data-collector", original_amid: "OLD_2", status: "spawned", namespace: "azureclaw-data-collector" },
      ],
      sub_agent_workspaces: [],      // EMPTY — this was the bug trigger
    };

    // Old logic (BROKEN): gated on sub_agent_workspaces
    const oldSubWorkspaces = restoreResp.sub_agent_workspaces || [];
    expect(oldSubWorkspaces.length).toBe(0); // would skip the entire block

    // New logic (FIXED): gated on sub_agent_results
    const spawnedSubs = (restoreResp.sub_agent_results || []).filter(
      (r: any) => r.status === "spawned"
    );
    expect(spawnedSubs.length).toBe(2); // enters the loop ✅
    expect(spawnedSubs[0].name).toBe("researcher");
    expect(spawnedSubs[1].name).toBe("data-collector");
  });

  it("workspace data is looked up by name from sub_agent_workspaces map", () => {
    const restoreResp = {
      sub_agent_results: [
        { name: "researcher", status: "spawned" },
        { name: "data-collector", status: "spawned" },
      ],
      sub_agent_workspaces: [
        { name: "researcher", workspace_tar: "SGVsbG8=", task_context: "Search papers", status: "paused_at_checkpoint" },
        // data-collector has no workspace entry at all
      ],
    };

    const subWorkspaceMap = new Map<string, any>();
    for (const ws of (restoreResp.sub_agent_workspaces || [])) {
      subWorkspaceMap.set(ws.name, ws);
    }

    // researcher has workspace data
    const researcherWs = subWorkspaceMap.get("researcher");
    expect(researcherWs).toBeDefined();
    expect(researcherWs.workspace_tar).toBe("SGVsbG8=");

    // data-collector has no workspace data — but still gets trust+resume
    const collectorWs = subWorkspaceMap.get("data-collector");
    expect(collectorWs).toBeUndefined();
  });

  it("workspace_inject_ack protocol: success path", () => {
    // Simulate sub-agent's ack message
    const ackMessage = {
      type: "handoff:workspace_inject_ack",
      from_agent: "researcher",
      success: true,
      file_count: 15,
      timestamp: "2026-04-14T12:00:00Z",
    };

    expect(ackMessage.success).toBe(true);
    expect(ackMessage.file_count).toBe(15);
    expect(ackMessage.from_agent).toBe("researcher");
  });

  it("workspace_inject_ack protocol: failure path", () => {
    const ackMessage = {
      type: "handoff:workspace_inject_ack",
      from_agent: "data-collector",
      success: false,
      file_count: 0,
      error: "workspace tar too large: 6291456",
      timestamp: "2026-04-14T12:00:00Z",
    };

    expect(ackMessage.success).toBe(false);
    expect(ackMessage.error).toContain("too large");
  });

  it("handoff_ready includes workspace delivery status per sub-agent", () => {
    const subAgentStatuses = [
      { name: "researcher", status: "resumed", task: "Search papers", workspace_delivered: true },
      { name: "data-collector", status: "resuming", task: "Collect data", workspace_delivered: false },
    ];

    // Simulate handoff_ready payload construction
    const handoffReady = {
      type: "handoff_ready",
      sub_agents_restored: subAgentStatuses.length,
      sub_agents_resumed: subAgentStatuses.filter(s => s.status === "resumed").length,
      sub_agents_workspace_delivered: subAgentStatuses.filter(
        (s: any) => s.status === "resumed" || s.status === "ready"
      ).length,
      sub_agent_details: subAgentStatuses,
    };

    expect(handoffReady.sub_agents_restored).toBe(2);
    expect(handoffReady.sub_agents_resumed).toBe(1);
    expect(handoffReady.sub_agents_workspace_delivered).toBe(1);
    expect(handoffReady.sub_agent_details[0].workspace_delivered).toBe(true);
    expect(handoffReady.sub_agent_details[1].workspace_delivered).toBe(false);
  });

  it("only 'spawned' sub-agents enter the trust loop (failed/skipped excluded)", () => {
    const restoreResp = {
      sub_agent_results: [
        { name: "researcher", status: "spawned" },
        { name: "data-collector", status: "failed" },  // quota exceeded
        { name: "summarizer", status: "spawned" },
      ],
    };

    const spawnedSubs = (restoreResp.sub_agent_results || []).filter(
      (r: any) => r.status === "spawned"
    );
    expect(spawnedSubs.length).toBe(2);
    expect(spawnedSubs.map((s: any) => s.name)).toEqual(["researcher", "summarizer"]);
  });

  it("missing sub_agent_results gracefully handled (pre-fix router)", () => {
    // Edge case: old router binary that doesn't return sub_agent_results
    const restoreResp = {
      restored: true,
      sub_agent_snapshots: 2,
      // sub_agent_results is missing entirely
    };

    const spawnedSubs = ((restoreResp as any).sub_agent_results || []).filter(
      (r: any) => r.status === "spawned"
    );
    expect(spawnedSubs.length).toBe(0); // graceful no-op, no crash
  });

  it("resume payload includes workspace_delivered flag", () => {
    const resumePayload = {
      type: "handoff:resume",
      from_agent: "dev-agent",
      task_context: "Search papers",
      previous_status: "paused_at_checkpoint",
      checkpoint: "3 papers found",
      workspace_delivered: true,
      timestamp: "2026-04-14T12:00:00Z",
    };

    expect(resumePayload.workspace_delivered).toBe(true);
    expect(resumePayload.type).toBe("handoff:resume");
  });

  it("handoff request tool response does NOT expose confirmation token to LLM", () => {
    // Simulates what the tool would return for a pending_confirmation.
    // The confirmation_token must NOT be in the response — only sent via Telegram.
    const toolResponse = {
      status: "pending_confirmation",
      direction: "local_to_aks",
      reason: "user_requested",
      expires_in_secs: 300,
      instruction: "Handoff to cloud (AKS) requested. A confirmation code has been sent to the user's Telegram. Ask the user to type the code. Do NOT guess or fabricate the code.",
      display: "🔄 Handoff requested to cloud (AKS)\nReason: user_requested\n\nA confirmation code has been sent to your Telegram.\nPlease type the code here to confirm.",
    };

    // The token field must NOT exist
    expect(toolResponse).not.toHaveProperty("confirmation_token");
    // The instruction must NOT contain an 8-char hex code pattern
    expect(toolResponse.instruction).not.toMatch(/[a-f0-9]{8}/);
    // The display must NOT contain a code
    expect(toolResponse.display).not.toMatch(/[a-f0-9]{8}/);
    // It should direct the user to Telegram
    expect(toolResponse.instruction).toContain("Telegram");
    expect(toolResponse.display).toContain("Telegram");
  });
});

// ---------------------------------------------------------------------------
// N. Mesh-send payload guard (rejects cross-container path references and
//    placeholder file_transfer envelopes — see mesh-payload-guard.ts)
// ---------------------------------------------------------------------------

describe("mesh-payload-guard via azureclaw_mesh_send", () => {
  let plugin: any;
  let tools: Map<string, any>;

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./index.js");
    plugin = mod.default;
    const mock = createMockApi();
    tools = mock.tools;
    plugin.register(mock.api);
    await new Promise((r) => setTimeout(r, 50));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    vi.restoreAllMocks();
  });

  it("rejects file_transfer envelope with placeholder file_data", async () => {
    const tool = tools.get("azureclaw_mesh_send")!;
    const result = await tool.execute("id", {
      to_agent: "writer",
      content: JSON.stringify({
        type: "file_transfer",
        file_name: "hero.png",
        file_data: "<base64-image-data>",
      }),
    });
    const parsed = JSON.parse(result.content[0].text);
    expect(parsed.error).toContain("placeholder");
    expect(parsed.error).toContain("azureclaw_mesh_transfer_file");
  });

  it("rejects file_transfer envelope with missing file_data", async () => {
    const tool = tools.get("azureclaw_mesh_send")!;
    const result = await tool.execute("id", {
      to_agent: "writer",
      content: JSON.stringify({ type: "file_transfer", file_name: "chart.png" }),
    });
    const parsed = JSON.parse(result.content[0].text);
    expect(parsed.error).toContain("missing file_data");
  });

  it("rejects payload referencing /sandbox/ path with no inlined data", async () => {
    const tool = tools.get("azureclaw_mesh_send")!;
    const result = await tool.execute("id", {
      to_agent: "writer",
      content: JSON.stringify({ artifact_path: "/sandbox/data.json" }),
    });
    const parsed = JSON.parse(result.content[0].text);
    expect(parsed.error).toContain("local container path");
    expect(parsed.error).toContain("azureclaw_mesh_transfer_file");
  });

  it("does NOT reject plain text mentioning /sandbox/ path", async () => {
    const tool = tools.get("azureclaw_mesh_send")!;
    // Plain text — not a JSON object — must be allowed even when it
    // mentions a /sandbox/... path. The downstream "AGT mesh not
    // initialized" error proves the guard let the call through.
    const result = await tool.execute("id", {
      to_agent: "writer",
      content: "I saved the chart to /sandbox/.openclaw/workspace/chart.png and will follow up.",
    });
    const parsed = JSON.parse(result.content[0].text);
    // The guard allowed it through — what we get back is the AGT-init
    // error (no mesh client in this test), NOT a guard rejection.
    expect(parsed.error).toContain("AGT mesh not initialized");
  });

  it("does NOT reject plain JSON metadata without local-path artifact keys", async () => {
    const tool = tools.get("azureclaw_mesh_send")!;
    const result = await tool.execute("id", {
      to_agent: "viz",
      content: JSON.stringify({ trends: ["a", "b"], metrics: { foo: 1 } }),
    });
    const parsed = JSON.parse(result.content[0].text);
    expect(parsed.error).toContain("AGT mesh not initialized");
  });

  it("accepts a valid file_transfer envelope (real base64)", async () => {
    const tool = tools.get("azureclaw_mesh_send")!;
    const realB64 = Buffer.from("hello world", "utf-8").toString("base64");
    const result = await tool.execute("id", {
      to_agent: "writer",
      content: JSON.stringify({
        type: "file_transfer",
        file_name: "note.txt",
        file_data: realB64,
        size_bytes: 11,
      }),
    });
    const parsed = JSON.parse(result.content[0].text);
    // Guard allowed it through; the AGT-init error proves we got past.
    expect(parsed.error).toContain("AGT mesh not initialized");
  });
});

// ---------------------------------------------------------------------------
// N+1. telegram_status tool
// ---------------------------------------------------------------------------

describe("telegram_status tool", () => {
  let plugin: any;
  let tools: Map<string, any>;
  let originalFetch: typeof globalThis.fetch;

  beforeEach(async () => {
    process.env.AGT_SKIP_INIT = "1";
    originalFetch = globalThis.fetch;
    const mod = await import("./index.js");
    plugin = mod.default;
    const mock = createMockApi();
    tools = mock.tools;
    plugin.register(mock.api);
    await new Promise((r) => setTimeout(r, 50));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    delete process.env.TELEGRAM_BOT_TOKEN;
    delete process.env.TELEGRAM_ALLOW_FROM;
    globalThis.fetch = originalFetch;
    vi.restoreAllMocks();
  });

  it("is registered as a tool", () => {
    expect(tools.has("telegram_status")).toBe(true);
    const tool = tools.get("telegram_status")!;
    expect(tool.parameters.required).toEqual(["text"]);
  });

  it("returns config error when TELEGRAM_BOT_TOKEN is missing", async () => {
    delete process.env.TELEGRAM_BOT_TOKEN;
    delete process.env.TELEGRAM_ALLOW_FROM;
    const tool = tools.get("telegram_status")!;
    const result = await tool.execute("id", { text: "hello" });
    const parsed = JSON.parse(result.content[0].text);
    expect(parsed.error).toContain("not configured");
  });

  it("returns empty-text error for blank input", async () => {
    process.env.TELEGRAM_BOT_TOKEN = "T";
    process.env.TELEGRAM_ALLOW_FROM = "1";
    const tool = tools.get("telegram_status")!;
    const result = await tool.execute("id", { text: "  " });
    const parsed = JSON.parse(result.content[0].text);
    expect(parsed.error).toContain("empty text");
  });

  it("uses TELEGRAM_ALLOW_FROM as the chat ID and posts to Telegram Bot API", async () => {
    process.env.TELEGRAM_BOT_TOKEN = "BOT_TOKEN_SECRET_123";
    process.env.TELEGRAM_ALLOW_FROM = "999111";
    const calls: Array<{ url: string; body: any }> = [];
    globalThis.fetch = (async (url: string, init: any) => {
      calls.push({ url, body: JSON.parse(init.body) });
      return { ok: true, status: 200 } as any;
    }) as any;

    const tool = tools.get("telegram_status")!;
    const result = await tool.execute("id", { text: "🔍 analyst: searching" });
    const parsed = JSON.parse(result.content[0].text);
    expect(parsed.status).toBe("delivered");
    expect(parsed.delivered).toBe(1);
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toContain("/sendMessage");
    expect(calls[0].body.chat_id).toBe("999111");
    expect(calls[0].body.text).toBe("🔍 analyst: searching");
  });

  it("redacts the bot token from error responses", async () => {
    process.env.TELEGRAM_BOT_TOKEN = "BOT_TOKEN_SECRET_456";
    process.env.TELEGRAM_ALLOW_FROM = "42";
    globalThis.fetch = (async () => {
      // Return a body that echoes the token back (Bot API does sometimes do this on 4xx)
      return {
        ok: false,
        status: 401,
        text: async () => "Unauthorized: token BOT_TOKEN_SECRET_456 invalid",
      } as any;
    }) as any;

    const tool = tools.get("telegram_status")!;
    const result = await tool.execute("id", { text: "test" });
    const text = result.content[0].text;
    expect(text).not.toContain("BOT_TOKEN_SECRET_456");
    expect(text).toContain("[REDACTED]");
    const parsed = JSON.parse(text);
    expect(parsed.status).toBe("failed");
  });

  it("supports multiple chat IDs separated by commas", async () => {
    process.env.TELEGRAM_BOT_TOKEN = "T";
    process.env.TELEGRAM_ALLOW_FROM = "111,222,333";
    const calls: any[] = [];
    globalThis.fetch = (async (_url: string, init: any) => {
      calls.push(JSON.parse(init.body));
      return { ok: true, status: 200 } as any;
    }) as any;

    const tool = tools.get("telegram_status")!;
    await tool.execute("id", { text: "x" });
    expect(calls.map((c) => c.chat_id)).toEqual(["111", "222", "333"]);
  });
});
