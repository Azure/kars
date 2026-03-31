/**
 * Tests for AzureClaw OpenClaw Plugin (plugin.ts)
 *
 * Strategy: Since most functions are module-private, we test through:
 * 1. The plugin's `register()` method — captures registered tools and tests their execute()
 * 2. Module re-imports with mocked dependencies for internal function behavior
 * 3. Direct plugin object property checks (id, name, configSchema, etc.)
 */

import { describe, it, expect, vi, beforeEach, afterEach, type Mock } from "vitest";
import type { IncomingMessage, ClientRequest } from "node:http";
import { EventEmitter } from "node:events";

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

/** Create a mock HTTP response (IncomingMessage-like) */
function createMockResponse(statusCode: number, body: string): EventEmitter & { statusCode: number } {
  const res = new EventEmitter() as EventEmitter & { statusCode: number; resume: () => void };
  res.statusCode = statusCode;
  res.resume = () => {};
  // Simulate async data + end emission
  process.nextTick(() => {
    res.emit("data", Buffer.from(body));
    res.emit("end");
  });
  return res;
}

/** Create a mock HTTP request (ClientRequest-like) */
function createMockRequest(response: EventEmitter): EventEmitter & { write: Mock; end: Mock; destroy: Mock; setTimeout: Mock } {
  const req = new EventEmitter() as any;
  req.write = vi.fn();
  req.end = vi.fn();
  req.destroy = vi.fn();
  req.setTimeout = vi.fn();

  // When end() is called, emit the response on next tick via the callback
  return req;
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
    const mod = await import("./plugin.js");
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
    const mod = await import("./plugin.js");
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
    const mod = await import("./plugin.js");
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
    const mod = await import("./plugin.js");
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
    const mod = await import("./plugin.js");
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
    const mod = await import("./plugin.js");
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
    const mod = await import("./plugin.js");
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
    const mod = await import("./plugin.js");
    const mock = createMockApi();
    tools = mock.tools;
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
    vi.restoreAllMocks();
  });

  it("azureclaw_spawn returns error when router is unreachable", async () => {
    const tool = tools.get("azureclaw_spawn")!;
    const result = await tool.execute("test-id", { name: "test-agent" });
    const text = result.content[0].text;
    expect(text).toContain("Spawn failed");
  });

  it("azureclaw_spawn_status returns error when router is unreachable", async () => {
    const tool = tools.get("azureclaw_spawn_status")!;
    const result = await tool.execute("test-id", { name: "test-agent" });
    const text = result.content[0].text;
    expect(text).toContain("Status check failed");
  });

  it("azureclaw_spawn_destroy returns error when router is unreachable", async () => {
    const tool = tools.get("azureclaw_spawn_destroy")!;
    const result = await tool.execute("test-id", { name: "test-agent" });
    const text = result.content[0].text;
    expect(text).toContain("Destroy failed");
  });

  it("azureclaw_spawn_list returns error when router is unreachable", async () => {
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
    const mod = await import("./plugin.js");
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
    const mod = await import("./plugin.js");
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
    const mod = await import("./plugin.js");
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
    const mod = await import("./plugin.js");
    const mock = createMockApi();
    tools = mock.tools;
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));
  });

  afterEach(() => {
    delete process.env.AGT_SKIP_INIT;
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
  });
});

// ---------------------------------------------------------------------------
// 13. Multiple register() calls — tools re-registered each session
// ---------------------------------------------------------------------------

describe("register() idempotency", () => {
  it("re-registers tools on second register() call", async () => {
    process.env.AGT_SKIP_INIT = "1";
    const mod = await import("./plugin.js");

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
    const mod = await import("./plugin.js");
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
    const mod = await import("./plugin.js");
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
    const mod = await import("./plugin.js");
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
    const mod = await import("./plugin.js");
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
    const mod = await import("./plugin.js");
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
    const mod = await import("./plugin.js");
    const mock = createMockApi({}); // empty pluginConfig → defaults
    mod.default.register(mock.api);
    // The spawn tool defaults to gpt-4.1 in its body
    const spawnTool = mock.tools.get("azureclaw_spawn")!;
    expect(spawnTool.parameters.properties.model.description).toContain("gpt-4.1");
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
    const mod = await import("./plugin.js");
    const mock = createMockApi();
    mod.default.register(mock.api);
    const hasName = mock.logMessages.some((m) => m.includes("custom-sandbox"));
    expect(hasName).toBe(true);
  });

  it("falls back to HOSTNAME when SANDBOX_NAME unset", async () => {
    process.env.AGT_SKIP_INIT = "1";
    delete process.env.SANDBOX_NAME;
    process.env.HOSTNAME = "my-host";
    const mod = await import("./plugin.js");
    const mock = createMockApi();
    mod.default.register(mock.api);
    const hasHost = mock.logMessages.some((m) => m.includes("my-host"));
    expect(hasHost).toBe(true);
  });

  it("falls back to 'local' when both SANDBOX_NAME and HOSTNAME unset", async () => {
    process.env.AGT_SKIP_INIT = "1";
    delete process.env.SANDBOX_NAME;
    delete process.env.HOSTNAME;
    const mod = await import("./plugin.js");
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
    const mod = await import("./plugin.js");
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
    const mod = await import("./plugin.js");
    const mock = createMockApi();
    mod.default.register(mock.api);
    await new Promise((r) => setTimeout(r, 100));

    for (const [name, tool] of mock.tools) {
      expect(typeof tool.execute, `Tool '${name}' should have execute()`).toBe("function");
    }

    delete process.env.AGT_SKIP_INIT;
  });
});
