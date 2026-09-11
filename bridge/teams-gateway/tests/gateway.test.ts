import { afterEach, describe, expect, it, vi } from "vitest";
import {
  BffClient,
  type DecisionRequest,
  type TeamCommandRequest,
} from "../src/bff-client.js";
import {
  buildApprovalCard,
  buildDecidedCard,
  buildProgressCard,
  isCardActionPayload,
  type ApprovalEvent,
} from "../src/cards.js";
import { loadConfig, parseRoleMappings, type TeamsGatewayConfig } from "../src/config.js";
import {
  approvalMessageKey,
  InMemoryConversationStore,
  KubernetesConversationStore,
} from "../src/conversation-store.js";
import { computeHmac, SIGNATURE_HEADER, verifyHmac } from "../src/hmac.js";
import { resolveIdentity, type TeamsIdentity } from "../src/identity.js";
import { log } from "../src/log.js";
import { registerAppHandlers } from "../src/main.js";

function mockConfig(
  overrides?: Partial<TeamsGatewayConfig> | undefined
): TeamsGatewayConfig {
  return {
    clientId: "client-id",
    clientSecret: "client-secret",
    tenantId: "tenant-id",
    entraRoleMappings: [
      {
        entraSubject: "oid-operator",
        bridgeSubject: "bridge-sub-operator",
        bridgeRoles: ["operator", "user"],
        displayName: "Alice Operator",
      },
      {
        entraSubject: "oid-user",
        bridgeSubject: "bridge-sub-user",
        bridgeRoles: ["user"],
        displayName: "Bob User",
      },
    ],
    bffBaseUrl: "https://bridge.example.test",
    bffInternalSecret: "bridge-internal-secret",
    port: 3978,
    internalPort: 3979,
    conversationConfigMapNamespace: "kars-system",
    conversationConfigMapName: "teams-gateway-store",
    watchNamespace: "kars-system",
    ...overrides,
  };
}

function withEnv(
  values: Record<string, string | undefined>,
  callback: () => void
): void {
  const saved = Object.fromEntries(
    Object.keys(values).map((key) => [key, process.env[key]])
  );
  for (const [key, value] of Object.entries(values)) {
    if (value === undefined) {
      delete process.env[key];
    } else {
      process.env[key] = value;
    }
  }
  try {
    callback();
  } finally {
    for (const [key, value] of Object.entries(saved)) {
      if (value === undefined) {
        delete process.env[key];
      } else {
        process.env[key] = value;
      }
    }
  }
}

const approvalEvent: ApprovalEvent = {
  name: "approval-1",
  namespace: "kars-system",
  task: "task-1",
  team: "engineering",
  actionKind: "checkpoint",
  summary: "Review deliverable",
  detail: "Please review the generated output.",
  requestedTier: 2,
  resourceVersion: "42",
  boundEnvelopeDigest: "sha256:abc123",
};

afterEach(() => {
  vi.restoreAllMocks();
});

describe("config validation", () => {
  it("fails closed when required settings are missing", () => {
    withEnv(
      {
        TEAMS_CLIENT_ID: undefined,
        TEAMS_CLIENT_SECRET: undefined,
        TEAMS_TENANT_ID: undefined,
        TEAMS_BFF_BASE_URL: undefined,
        TEAMS_BFF_INTERNAL_SECRET: undefined,
        TEAMS_ENTRA_ROLE_MAP: undefined,
      },
      () => {
        expect(() => loadConfig()).toThrow("FATAL");
      }
    );
  });

  it("parses a valid role map", () => {
    const result = parseRoleMappings(
      '[{"entra_subject":"oid1","bridge_subject":"bridge-sub-1","roles":["operator"],"name":"Alice"}]'
    );
    expect(result).toEqual([
      {
        entraSubject: "oid1",
        bridgeSubject: "bridge-sub-1",
        bridgeRoles: ["operator"],
        displayName: "Alice",
      },
    ]);
  });
});

describe("log redaction", () => {
  it("redacts secret-like fields", () => {
    const lines: string[] = [];
    const originalWrite = process.stdout.write;
    process.stdout.write = ((chunk: string | Uint8Array) => {
      lines.push(String(chunk));
      return true;
    }) as typeof process.stdout.write;
    try {
      log("info", "test", {
        token: "abc",
        authorization: "Bearer secret",
        safeValue: "visible",
      });
    } finally {
      process.stdout.write = originalWrite;
    }
    const output = lines.join("");
    expect(output).toContain("[REDACTED]");
    expect(output).not.toContain("Bearer secret");
    expect(output).toContain("visible");
  });
});

describe("identity resolution", () => {
  const config = mockConfig();

  it("resolves a mapped operator subject", () => {
    const identity: TeamsIdentity = {
      aadObjectId: "oid-operator",
      displayName: "Alice",
    };
    expect(resolveIdentity(config, identity)).toEqual({
      entraSubject: "oid-operator",
      sub: "bridge-sub-operator",
      name: "Alice Operator",
      roles: ["operator", "user"],
    });
  });

  it("rejects an unmapped subject", () => {
    expect(
      resolveIdentity(config, {
        aadObjectId: "oid-missing",
        displayName: "Eve",
      })
    ).toBeNull();
  });
});

describe("HMAC auth", () => {
  it("computes and verifies signatures", () => {
    const secret = "shared-secret";
    const body = '{"hello":"world"}';
    const signature = computeHmac(secret, body);
    expect(signature).toHaveLength(64);
    expect(verifyHmac(secret, body, signature)).toBe(true);
    expect(verifyHmac(secret, body, null)).toBe(false);
    expect(verifyHmac(secret, body, "deadbeef")).toBe(false);
    expect(SIGNATURE_HEADER).toBe("x-teams-internal-signature");
  });
});

describe("cards", () => {
  it("builds approval cards with merged execute payload routing", () => {
    const card = buildApprovalCard(approvalEvent);
    const body = card.body as Array<Record<string, unknown>>;
    const input = body.find((item) => item.type === "Input.Text");
    expect(input).toBeDefined();
    expect(input?.id).toBe("requestChangesReason");

    const actions = (card.actions ?? []) as Array<Record<string, unknown>>;
    expect(actions).toHaveLength(3);
    expect(actions.map((action) => action.verb)).toEqual([
      "kars.approve",
      "kars.request-changes",
      "kars.deny",
    ]);
    for (const action of actions) {
      expect((action.data as { action?: string }).action).toBe(
        "kars.approval.decision"
      );
    }
  });

  it("validates card action payloads", () => {
    expect(
      isCardActionPayload({
        action: "kars.approval.decision",
        approvalName: "approval-1",
        approvalNamespace: "kars-system",
        verdict: "request-changes",
        resourceVersion: "7",
        requestChangesReason: "Need more detail",
      })
    ).toBe(true);
    expect(
      isCardActionPayload({
        action: "kars.approval.decision",
        approvalName: "approval-1",
        approvalNamespace: "kars-system",
        verdict: "maybe",
        resourceVersion: "7",
      })
    ).toBe(false);
  });

  it("builds decided and progress cards", () => {
    const decided = buildDecidedCard(
      approvalEvent,
      "request-changes",
      "Alice Operator",
      "Please tighten the write-up"
    );
    const progress = buildProgressCard({
      kind: "task",
      teamName: "engineering",
      resourceName: "task-1",
      title: "Run deliverable",
      status: "Ready / Running",
      summary: "The task delivered a result.",
      detail: "A pull request is ready for review.",
      stage: "delivery",
    });
    expect((decided.body?.[0] as { text?: string } | undefined)?.text).toContain(
      "Changes Requested"
    );
    expect((progress.body?.[0] as { text?: string } | undefined)?.text).toContain(
      "Run deliverable"
    );
  });
});

describe("conversation store", () => {
  it("tracks bindings, dedupe records, and last resource versions in memory", async () => {
    const store = new InMemoryConversationStore();
    await store.bind({
      conversationId: "conv-1",
      serviceUrl: "https://service.example.test",
      tenantId: "tenant-id",
      teamName: "engineering",
      namespace: "kars-system",
      boundAt: "2026-01-01T00:00:00Z",
    });
    await store.recordApprovalMessage({
      approvalName: "approval-1",
      approvalNamespace: "kars-system",
      conversationId: "conv-1",
      teamName: "engineering",
      messageId: "message-1",
      resourceVersion: "42",
      boundEnvelopeDigest: "sha256:abc123",
      sentAt: "2026-01-01T00:01:00Z",
    });
    await store.setLastResourceVersion("watch.karsapprovals", "99");

    expect((await store.getByTeam("engineering"))?.conversationId).toBe(
      "conv-1"
    );
    expect(
      await store.getApprovalMessage("kars-system", "approval-1")
    ).toEqual(
      expect.objectContaining({
        messageId: "message-1",
        boundEnvelopeDigest: "sha256:abc123",
      })
    );
    expect(await store.getLastResourceVersion("watch.karsapprovals")).toBe(
      "99"
    );
  });

  it("persists bindings, message ids, and resource versions via ConfigMap API", async () => {
    class FakeCoreV1Api {
      public configMap:
        | {
            apiVersion: string;
            kind: string;
            metadata: { name: string; namespace: string; resourceVersion?: string | undefined };
            data: Record<string, string>;
          }
        | undefined;

      public async readNamespacedConfigMap(): Promise<unknown> {
        if (!this.configMap) {
          const error = Object.assign(new Error("Not Found"), {
            code: 404,
            statusCode: 404,
          });
          throw error;
        }
        return this.configMap;
      }

      public async createNamespacedConfigMap(
        namespace: string,
        body: {
          apiVersion?: string;
          kind?: string;
          metadata?: { name?: string; namespace?: string };
          data?: Record<string, string>;
        }
      ): Promise<unknown> {
        this.configMap = {
          apiVersion: body.apiVersion ?? "v1",
          kind: body.kind ?? "ConfigMap",
          metadata: {
            name: body.metadata?.name ?? "teams-store",
            namespace,
            resourceVersion: "1",
          },
          data: body.data ?? {},
        };
        return this.configMap;
      }

      public async replaceNamespacedConfigMap(
        _name: string,
        namespace: string,
        body: {
          apiVersion?: string;
          kind?: string;
          metadata?: { name?: string; namespace?: string; resourceVersion?: string | undefined };
          data?: Record<string, string>;
        }
      ): Promise<unknown> {
        this.configMap = {
          apiVersion: body.apiVersion ?? "v1",
          kind: body.kind ?? "ConfigMap",
          metadata: {
            name: body.metadata?.name ?? "teams-store",
            namespace,
            resourceVersion: String(
              Number(this.configMap?.metadata.resourceVersion ?? "0") + 1
            ),
          },
          data: body.data ?? {},
        };
        return this.configMap;
      }
    }

    const api = new FakeCoreV1Api();
    const store = new KubernetesConversationStore({
      namespace: "kars-system",
      configMapName: "teams-store",
      api,
    });
    await store.initialize();
    await store.bind({
      conversationId: "conv-2",
      serviceUrl: "https://service.example.test",
      tenantId: "tenant-id",
      teamName: "engineering",
      namespace: "kars-system",
      boundAt: "2026-01-01T00:00:00Z",
    });
    await store.recordApprovalMessage({
      approvalName: "approval-2",
      approvalNamespace: "kars-system",
      conversationId: "conv-2",
      teamName: "engineering",
      messageId: "message-2",
      resourceVersion: "100",
      boundEnvelopeDigest: "sha256:def456",
      sentAt: "2026-01-01T00:02:00Z",
    });
    await store.setLastResourceVersion("watch.karsapprovals", "101");

    const reloaded = new KubernetesConversationStore({
      namespace: "kars-system",
      configMapName: "teams-store",
      api,
    });
    await reloaded.initialize();

    expect((await reloaded.getByConversation("conv-2"))?.teamName).toBe(
      "engineering"
    );
    expect(
      approvalMessageKey("kars-system", "approval-2")
    ).toBe("kars-system/approval-2");
    expect(
      (await reloaded.getApprovalMessage("kars-system", "approval-2"))
        ?.messageId
    ).toBe("message-2");
    expect(
      await reloaded.getLastResourceVersion("watch.karsapprovals")
    ).toBe("101");
  });
});

describe("BFF client payloads", () => {
  it("sends decisions with entra subject/name and no principal object", async () => {
    const requests: Array<{ url: string; init: RequestInit }> = [];
    globalThis.fetch = vi.fn(async (url: string | URL | Request, init?: RequestInit) => {
      requests.push({ url: String(url), init: init ?? {} });
      return new Response(JSON.stringify({ phase: "Denied" }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }) as unknown as typeof fetch;

    const client = new BffClient(mockConfig());
    const result = await client.submitDecision({
      approvalName: "approval-1",
      approvalNamespace: "kars-system",
      verdict: "request-changes",
      reason: "Need more detail",
      resourceVersion: "42",
      boundEnvelopeDigest: "sha256:abc123",
      principal: {
        entraSubject: "oid-operator",
        sub: "bridge-sub-operator",
        name: "Alice Operator",
        roles: ["operator"],
      },
    } satisfies DecisionRequest);

    expect(result.success).toBe(true);
    const body = JSON.parse(String(requests[0]?.init.body)) as Record<string, unknown>;
    expect(body).toMatchObject({
      approval_name: "approval-1",
      approval_namespace: "kars-system",
      verdict: "request-changes",
      reason: "Need more detail",
      resource_version: "42",
      bound_envelope_digest: "sha256:abc123",
      entra_subject: "oid-operator",
      entra_name: "Alice Operator",
    });
    expect(body.principal).toBeUndefined();
  });

  it("sends commands with entra subject/name and no principal object", async () => {
    const requests: Array<{ url: string; init: RequestInit }> = [];
    globalThis.fetch = vi.fn(async (url: string | URL | Request, init?: RequestInit) => {
      requests.push({ url: String(url), init: init ?? {} });
      return new Response(JSON.stringify({ message: "ok" }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }) as unknown as typeof fetch;

    const client = new BffClient(mockConfig());
    const result = await client.sendTeamCommand({
      teamName: "engineering",
      namespace: "kars-system",
      command: "status",
      args: "",
      principal: {
        entraSubject: "oid-user",
        sub: "bridge-sub-user",
        name: "Bob User",
        roles: ["user"],
      },
    } satisfies TeamCommandRequest);

    expect(result).toEqual({ success: true, message: "ok" });
    const body = JSON.parse(String(requests[0]?.init.body)) as Record<string, unknown>;
    expect(body).toMatchObject({
      team_name: "engineering",
      namespace: "kars-system",
      command: "status",
      args: "",
      entra_subject: "oid-user",
      entra_name: "Bob User",
    });
    expect(body.principal).toBeUndefined();
  });
});

describe("main handler wiring", () => {
  it("registers one routed card action handler", () => {
    class FakeApp {
      public readonly handlers = new Map<string, (context: unknown) => unknown>();
      public readonly api = {
        conversations: {
          updateActivity: vi.fn(async () => undefined),
        },
      };
      public on(route: string, handler: (context: unknown) => unknown): void {
        this.handlers.set(route, handler);
      }
      public async start(): Promise<void> {
        return undefined;
      }
      public async send(): Promise<{ id: string }> {
        return { id: "message-1" };
      }
    }

    const app = new FakeApp();
    registerAppHandlers(app, {
      config: mockConfig(),
      store: new InMemoryConversationStore(),
      bff: {
        submitDecision: vi.fn(async () => ({ success: true, phase: "Denied" })),
        sendTeamCommand: vi.fn(async () => ({ success: true, message: "ok" })),
      },
    });

    expect([...app.handlers.keys()].filter((key) => key.startsWith("card.action"))).toEqual([
      "card.action.kars.approval.decision",
    ]);
  });

  it("binds an unbound conversation via /bind before requiring an existing binding", async () => {
    class FakeApp {
      public readonly handlers = new Map<string, (context: unknown) => unknown>();
      public readonly api = {
        conversations: {
          updateActivity: vi.fn(async () => undefined),
        },
      };
      public on(route: string, handler: (context: unknown) => unknown): void {
        this.handlers.set(route, handler);
      }
      public async start(): Promise<void> {
        return undefined;
      }
      public async send(): Promise<{ id: string }> {
        return { id: "message-1" };
      }
    }

    const app = new FakeApp();
    const store = new InMemoryConversationStore();
    const sendTeamCommand = vi.fn(async () => ({ success: true, message: "ok" }));
    const reconcileTeamApprovals = vi.fn(async () => undefined);
    registerAppHandlers(app, {
      config: mockConfig(),
      store,
      bff: {
        submitDecision: vi.fn(async () => ({ success: true, phase: "Denied" })),
        sendTeamCommand,
      },
      reconcileTeamApprovals,
    });
    const messageHandler = app.handlers.get("message");
    const sent: TeamsOutboundActivity[] = [];
    await messageHandler?.({
      activity: {
        text: "/bind engineering",
        from: { aadObjectId: "oid-operator", name: "Alice" },
        conversation: { id: "conv-3", tenantId: "tenant-id" },
        serviceUrl: "https://service.example.test",
      },
      send: async (activity: TeamsOutboundActivity) => {
        sent.push(activity);
        return undefined;
      },
    });

    expect((await store.getByConversation("conv-3"))?.teamName).toBe(
      "engineering"
    );
    expect(sendTeamCommand).toHaveBeenCalledWith(
      expect.objectContaining({
        teamName: "engineering",
        command: "bind",
      })
    );
    expect(reconcileTeamApprovals).toHaveBeenCalledWith("engineering");
    expect(sent[0]?.text).toContain("Bound this conversation");
  });

  it("reads requestChangesReason from action.data in the routed handler", async () => {
    class FakeApp {
      public readonly handlers = new Map<string, (context: unknown) => unknown>();
      public readonly api = {
        conversations: {
          updateActivity: vi.fn(async () => undefined),
        },
      };
      public on(route: string, handler: (context: unknown) => unknown): void {
        this.handlers.set(route, handler);
      }
      public async start(): Promise<void> {
        return undefined;
      }
      public async send(): Promise<{ id: string }> {
        return { id: "message-1" };
      }
    }

    const app = new FakeApp();
    const submitDecision = vi.fn(async () => ({
      success: true,
      phase: "Denied",
    }));
    registerAppHandlers(app, {
      config: mockConfig(),
      store: new InMemoryConversationStore(),
      bff: {
        submitDecision,
        sendTeamCommand: vi.fn(async () => ({ success: true, message: "ok" })),
      },
    });

    const handler = app.handlers.get("card.action.kars.approval.decision");
    const result = await handler?.({
      activity: {
        from: { aadObjectId: "oid-operator", name: "Alice" },
        value: {
          action: {
            data: {
              action: "kars.approval.decision",
              approvalName: "approval-1",
              approvalNamespace: "kars-system",
              verdict: "request-changes",
              resourceVersion: "42",
              boundEnvelopeDigest: "sha256:abc123",
              requestChangesReason: "Please add more detail.",
            },
          },
        },
      },
    });

    expect(submitDecision).toHaveBeenCalledWith(
      expect.objectContaining({
        verdict: "request-changes",
        reason: "Please add more detail.",
      })
    );
    expect(result).toMatchObject({
      statusCode: 200,
      type: "application/vnd.microsoft.card.adaptive",
    });
  });
});

interface TeamsOutboundActivity {
  readonly type: "message";
  readonly text?: string | undefined;
  readonly attachments?: readonly {
    readonly contentType: "application/vnd.microsoft.card.adaptive";
    readonly content: object;
  }[];
}
