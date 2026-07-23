import { afterEach, describe, expect, it } from "vitest";
import { createServer, type Server } from "node:http";
import { createHash } from "node:crypto";
import { existsSync, rmSync } from "node:fs";
import { resolve } from "node:path";

import {
  agtEvaluateFailOpenGrace,
  compactSubAgentExecutionContract,
  compactTaskLoopMessages,
  createAGTPolicyEvaluator,
  githubMcpRoutingError,
  normalizeTaskContract,
  processTaskWithTools,
  type AGTEvaluateTransport,
  type TaskLoopMessage,
} from "./agt-task-loop.js";

const log = { info: () => {}, warn: () => {} };
let server: Server | undefined;
const sideEffectPath = resolve(".agt-shell-side-effect-test");

afterEach(async () => {
  delete process.env.KARS_AGT_EVALUATE_FAIL_OPEN_GRACE;
  delete process.env.KARS_ROUTER_URL;
  delete process.env.KARS_PROVIDER;
  if (server) {
    await new Promise<void>((done) => server!.close(() => done()));
    server = undefined;
  }
  rmSync(sideEffectPath, { force: true });
});

describe("createAGTPolicyEvaluator", () => {
  it("blocks the first failure when the grace env is absent", async () => {
    delete process.env.KARS_AGT_EVALUATE_FAIL_OPEN_GRACE;
    const transport: AGTEvaluateTransport = async () => {
      throw new Error("connection refused");
    };

    const decision = await createAGTPolicyEvaluator(log, transport)("tool:test:");

    expect(decision.allowed).toBe(false);
    expect(decision.reason).toContain("fail-closed");
  });

  it("blocks a timeout by default", async () => {
    const evaluate = createAGTPolicyEvaluator(log, async () => {
      throw new Error("timeout");
    });

    const decision = await evaluate("tool:test:");

    expect(decision.allowed).toBe(false);
    expect(decision.reason).toContain("timeout");
  });

  it("grace 2 allows exactly two failures and blocks the third", async () => {
    process.env.KARS_AGT_EVALUATE_FAIL_OPEN_GRACE = "2";
    const transport: AGTEvaluateTransport = async () => {
      throw new Error("connection refused");
    };
    const evaluate = createAGTPolicyEvaluator(log, transport);

    const decisions = [
      await evaluate("tool:test:"),
      await evaluate("tool:test:"),
      await evaluate("tool:test:"),
    ];

    expect(decisions.map((decision) => decision.allowed)).toEqual([true, true, false]);
  });

  it("resets only after a valid parsed successful response", async () => {
    process.env.KARS_AGT_EVALUATE_FAIL_OPEN_GRACE = "2";
    const responses = [
      new Error("connection refused"),
      new Error("connection refused"),
      { statusCode: 200, body: '{"allowed":true,"decision":"allow"}' },
      new Error("connection refused"),
      new Error("connection refused"),
      new Error("connection refused"),
    ];
    const transport: AGTEvaluateTransport = async () => {
      const response = responses.shift();
      if (response instanceof Error) throw response;
      return response!;
    };
    const evaluate = createAGTPolicyEvaluator(log, transport);
    const decisions = [];

    for (let i = 0; i < 6; i += 1) {
      decisions.push(await evaluate("tool:test:"));
    }

    expect(decisions.map((decision) => decision.allowed)).toEqual([
      true, true, true, true, true, false,
    ]);
  });

  it("blocks 503 and malformed JSON responses by default", async () => {
    const unavailable = createAGTPolicyEvaluator(log, async () => ({
      statusCode: 503,
      body: "unavailable",
    }));
    const malformed = createAGTPolicyEvaluator(log, async () => ({
      statusCode: 200,
      body: "{not-json",
    }));

    expect((await unavailable("tool:test:")).allowed).toBe(false);
    expect((await malformed("tool:test:")).allowed).toBe(false);
  });

  it("does not reset after repeated connection failures", async () => {
    process.env.KARS_AGT_EVALUATE_FAIL_OPEN_GRACE = "2";
    const evaluate = createAGTPolicyEvaluator(log, async () => {
      throw new Error("connection refused");
    });
    const decisions = [];

    for (let i = 0; i < 4; i += 1) {
      decisions.push(await evaluate("tool:test:"));
    }

    expect(decisions.map((decision) => decision.allowed)).toEqual([true, true, false, false]);
  });

  it("clamps the configured grace to 0..10", () => {
    expect(agtEvaluateFailOpenGrace()).toBe(0);
    expect(agtEvaluateFailOpenGrace("-2")).toBe(0);
    expect(agtEvaluateFailOpenGrace("2")).toBe(2);
    expect(agtEvaluateFailOpenGrace("99")).toBe(10);
    expect(agtEvaluateFailOpenGrace("invalid")).toBe(0);
    expect(agtEvaluateFailOpenGrace("2.5")).toBe(0);
  });
});

describe("task-loop context bounds", () => {
  it("requires GitHub MCP for GitHub API and page requests", () => {
    expect(githubMcpRoutingError(
      "https://api.github.com/repos/Azure/kars/pulls",
      "GET",
      "github",
    )).toMatch(/GitHub MCP is configured/);
    expect(githubMcpRoutingError(
      "https://github.com/Azure/kars/pull/1",
      "GET",
      "github-mcp",
    )).toMatch(/GitHub MCP is configured/);
    expect(githubMcpRoutingError(
      "https://raw.githubusercontent.com/Azure/kars/main/README.md",
      "GET",
      "github",
    )).toBeNull();
    expect(githubMcpRoutingError(
      "https://api.github.com/repos/Azure/kars/pulls",
      "GET",
      "playwright",
    )).toBeNull();
  });

  it("verifies and expands a versioned task contract", () => {
    const objective = "Inspect the repository.";
    const instructions = "Return evidence from every selected role.";
    const checkpoint_json = JSON.stringify({
      milestone_id: "inventory",
      status: "in_progress",
    });
    const frame = (value: string) => `${Buffer.byteLength(value, "utf8")}:${value}`;
    const digest = createHash("sha256")
      .update(["kars.task/v1", objective, instructions, checkpoint_json].map(frame).join(""))
      .digest("hex");

    const normalized = normalizeTaskContract(JSON.stringify({
      schema: "kars.task/v1",
      digest,
      objective,
      instructions,
      checkpoint_json,
    }));

    expect(normalized.contract?.digest).toBe(digest);
    expect(normalized.userContent).toContain("execution-contract.json");
    expect(normalized.userContent).toContain(objective);
    expect(normalized.userContent).toContain(instructions);
    expect(normalized.userContent).toContain("Resume checkpoint");
    expect(normalized.userContent).toContain("inventory");
  });

  it("rejects a tampered versioned task contract", () => {
    expect(() => normalizeTaskContract(JSON.stringify({
      schema: "kars.task/v1",
      digest: "0".repeat(64),
      objective: "Do something else.",
      instructions: "",
    }))).toThrow(/digest mismatch/);
  });

  it("keeps the default execution contract compact and source-specific", () => {
    const contract = compactSubAgentExecutionContract();

    expect(contract.length).toBeLessThan(3_000);
    expect(contract).toContain("GitHub MCP");
    expect(contract).toContain("work-packet ID");
    expect(contract).toContain("Do not redo a completed work packet");
  });

  it("compacts old round history while preserving the task and recent tool pair", () => {
    const pinnedSystem = `system contract:${"s".repeat(6_000)}`;
    const pinnedTask = `original task:${"u".repeat(6_000)}`;
    const messages: TaskLoopMessage[] = [
      { role: "system", content: pinnedSystem },
      { role: "user", content: pinnedTask },
    ];
    for (let index = 0; index < 12; index += 1) {
      messages.push({
        role: "assistant",
        tool_calls: [{
          id: `call-${index}`,
          function: {
            name: "http_fetch",
            arguments: JSON.stringify({
              url: `https://example.com/${index}`,
              payload: "y".repeat(3_000),
            }),
          },
        }],
      });
      messages.push({
        role: "tool",
        tool_call_id: `call-${index}`,
        content: `result-${index}:${"x".repeat(10_000)}`,
      });
    }

    const compacted = compactTaskLoopMessages(messages, 40_000, 4);

    expect(compacted.length).toBeLessThan(messages.length);
    expect(compacted[0].content).toBe(pinnedSystem);
    expect(compacted[1].content).toBe(pinnedTask);
    expect(compacted[2].content).toContain("earlier round messages were compacted");
    expect(compacted.at(-1)?.tool_call_id).toBe("call-11");
    const finalAssistant = compacted.slice().reverse().find((message) => message.role === "assistant");
    expect(JSON.parse(finalAssistant?.tool_calls?.[0]?.function?.arguments)).toEqual({
      _compacted: true,
    });
    expect(JSON.stringify(compacted).length).toBeLessThan(40_000);
  });
});

describe("processTaskWithTools shell governance", () => {
  it("does not execute fallback shell when governance returns 503", async () => {
    let chatCalls = 0;
    server = createServer((req, res) => {
      if (req.url === "/agt/evaluate") {
        res.writeHead(503, { "content-type": "text/plain" });
        res.end("unavailable");
        return;
      }
      if (req.url === "/v1/chat/completions") {
        chatCalls += 1;
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({
          choices: [{
            finish_reason: chatCalls === 1 ? "tool_calls" : "stop",
            message: chatCalls === 1
              ? {
                  role: "assistant",
                  content: null,
                  tool_calls: [{
                    id: "shell-1",
                    type: "function",
                    function: {
                      name: "exec_command",
                      arguments: JSON.stringify({
                        command: `node -e "require('fs').writeFileSync('${sideEffectPath}', 'ran')"`,
                      }),
                    },
                  }],
                }
              : { role: "assistant", content: "done" },
          }],
        }));
        return;
      }
      res.writeHead(404);
      res.end();
    });
    await new Promise<void>((done) => server!.listen(0, "127.0.0.1", () => done()));
    const address = server.address();
    const port = typeof address === "object" && address ? address.port : 0;
    process.env.KARS_ROUTER_URL = `http://127.0.0.1:${port}`;
    process.env.KARS_PROVIDER = "github-models";

    const result = await processTaskWithTools("run the command", {
      meshClient: () => null,
      isInterruptRequested: () => false,
      interruptReason: () => "",
      setInterrupt: () => {},
    }, log);

    expect(result).toBe("done");
    expect(existsSync(sideEffectPath)).toBe(false);
  });
});
