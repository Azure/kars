// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { createServer, type ServerResponse } from "node:http";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import {
  CoreV1Api, CustomObjectsApi, KubeConfig, Watch, loadYaml,
  type KubernetesObject, type V1ConfigMap, type V1Deployment,
} from "@kubernetes/client-node";
import { afterEach, describe, expect, it, vi } from "vitest";
import { loadConfig, type TeamsGatewayConfig } from "../src/config.js";
import { KubernetesConversationStore } from "../src/conversation-store.js";
import { registerAppHandlers } from "../src/main.js";
import { GatewayWatcher } from "../src/watcher.js";

const storeName = "kars-teams-conversations";
const plurals = ["karsapprovals", "karstasks", "karsteams"];
const coreNamespace = "kars-system";
const resourcePath = (plural: string) =>
  `/apis/kars.azure.com/v1alpha1/namespaces/${coreNamespace}/${plural}`;

function apiStatus(code: number) {
  const reasons: Record<number, string> = {
    403: "Forbidden", 404: "NotFound", 409: "Conflict", 410: "Gone", 500: "InternalError", 503: "ServiceUnavailable",
  };
  return {
    apiVersion: "v1", kind: "Status", status: "Failure", code,
    reason: reasons[code], message: `fixture API error ${code}`,
  };
}

function sdkFailure(code: number) {
  return { code, body: JSON.stringify(apiStatus(code)) };
}

interface ApiRequest {
  method: string;
  path: string;
  query: URLSearchParams;
  body: V1ConfigMap | undefined;
}

// A loopback API fixture, not a replacement SDK interface. Every operation goes
// through the installed client, including JSON serialization and watch streams.
class KubernetesApiFixture {
  readonly kubeConfig = new KubeConfig();
  readonly requests: ApiRequest[] = [];
  readonly lists = new Map<string, unknown[]>(plurals.map((plural) => [plural, []]));
  readonly watches = new Map<string, ServerResponse[]>();
  readonly failures: Array<{ method: string; path: string; code: number; watch: boolean }> = [];
  configMap: V1ConfigMap | undefined;
  listResourceVersion: string | undefined = "10";
  createRace = false;
  readonly server = createServer(async (request, response) => {
    const url = new URL(request.url!, "http://localhost");
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const rawBody = Buffer.concat(chunks).toString("utf8");
    const body: V1ConfigMap | undefined = rawBody ? JSON.parse(rawBody) : undefined;
    const method = request.method!;
    this.requests.push({ method, path: url.pathname, query: url.searchParams, body });
    const reply = (code: number, value: unknown) => {
      response.writeHead(code, { "content-type": "application/json" });
      response.end(JSON.stringify(value));
    };
    const fail = (code: number) => reply(code, apiStatus(code));
    const failureIndex = this.failures.findIndex((failure) =>
      failure.method === method && failure.path === url.pathname
      && failure.watch === (url.searchParams.get("watch") === "true"));
    if (failureIndex >= 0) {
      fail(this.failures.splice(failureIndex, 1)[0]!.code);
      return;
    }
    if (url.pathname === this.configMapPath && method === "GET") {
      if (this.configMap) reply(200, this.configMap);
      else fail(404);
      return;
    }
    if (url.pathname === this.configMapPath && method === "PUT" && body) {
      if (body.metadata?.resourceVersion !== this.configMap?.metadata?.resourceVersion) {
        fail(409);
        return;
      }
      this.configMap = {
        ...body, metadata: { ...body.metadata, resourceVersion: String(Number(body.metadata?.resourceVersion) + 1) },
      };
      reply(200, this.configMap);
      return;
    }
    if (url.pathname === this.configMapCollectionPath && method === "POST" && body) {
      this.configMap = { ...body, metadata: { ...body.metadata, resourceVersion: "1" } };
      if (this.createRace) fail(409);
      else reply(201, this.configMap);
      return;
    }
    const plural = plurals.find((candidate) => resourcePath(candidate) === url.pathname);
    if (method === "GET" && plural) {
      if (url.searchParams.get("watch") === "true") {
        response.writeHead(200, { "content-type": "application/json" });
        response.flushHeaders();
        this.watches.set(plural, [...(this.watches.get(plural) ?? []), response]);
      } else {
        reply(200, { apiVersion: "kars.azure.com/v1alpha1", kind: "List",
          metadata: { resourceVersion: this.listResourceVersion }, items: this.lists.get(plural) });
      }
      return;
    }
    fail(404);
  });

  constructor(readonly storageNamespace = "bridge-private") {
    this.configMap = {
      apiVersion: "v1", kind: "ConfigMap",
      metadata: { name: storeName, namespace: storageNamespace, resourceVersion: "1" },
      data: { "bindings.json": "[]", "approval-messages.json": "[]", "resource-versions.json": "{}" },
    };
  }

  get configMapCollectionPath(): string {
    return `/api/v1/namespaces/${this.storageNamespace}/configmaps`;
  }

  get configMapPath(): string {
    return `${this.configMapCollectionPath}/${storeName}`;
  }

  async start(): Promise<void> {
    await new Promise<void>((resolve) => this.server.listen(0, "127.0.0.1", resolve));
    const address = this.server.address();
    if (!address || typeof address === "string") throw new Error("fixture did not bind");
    this.kubeConfig.loadFromOptions({
      clusters: [{ name: "fixture", server: `http://127.0.0.1:${address.port}`, skipTLSVerify: true }],
      users: [{ name: "fixture" }],
      contexts: [{ name: "fixture", cluster: "fixture", user: "fixture" }],
      currentContext: "fixture",
    });
  }

  async close(): Promise<void> {
    this.server.closeAllConnections();
    await new Promise<void>((resolve, reject) => this.server.close((error) => error ? reject(error) : resolve()));
  }

  store(): KubernetesConversationStore {
    return new KubernetesConversationStore({
      namespace: this.storageNamespace, configMapName: storeName, kubeConfig: this.kubeConfig,
    });
  }

  activeWatches(plural: string): ServerResponse[] {
    return (this.watches.get(plural) ?? []).filter((response) => !response.destroyed && !response.writableEnded);
  }

  emit(plural: string, type: string, object: unknown): void {
    const responses = this.activeWatches(plural);
    expect(responses).toHaveLength(1);
    responses[0]!.write(`${JSON.stringify({ type, object })}\n`);
  }

  listCount(plural: string): number {
    return this.requests.filter((request) =>
      request.path === resourcePath(plural) && request.query.get("watch") !== "true").length;
  }
}

const binding = {
  conversationId: "conversation-1", serviceUrl: "https://teams.example.test",
  tenantId: "tenant-1", teamName: "engineering", namespace: coreNamespace, boundAt: "2026-01-01T00:00:00Z",
};
const approval = {
  metadata: { name: "approval-1", namespace: coreNamespace, resourceVersion: "10",
    labels: { "kars.azure.com/team": "engineering" } },
  spec: { taskRef: { name: "task-1" }, action: { kind: "checkpoint", summary: "Review result" } },
  status: { phase: "Pending", boundEnvelopeDigest: "sha256:bound" },
};
const fixtures: KubernetesApiFixture[] = [];
const watchers: GatewayWatcher[] = [];

async function fixture(namespace?: string): Promise<KubernetesApiFixture> {
  const api = new KubernetesApiFixture(namespace);
  fixtures.push(api);
  await api.start();
  return api;
}

function watcher(api: KubernetesApiFixture, store = api.store()) {
  const config: TeamsGatewayConfig = {
    clientId: "client-1", clientSecret: "fixture", tenantId: "tenant-1", entraRoleMappings: [],
    bffBaseUrl: "http://bff.example.test", bffInternalSecret: "fixture", port: 3978, internalPort: 3979,
    conversationConfigMapName: storeName, conversationConfigMapNamespace: api.storageNamespace,
    watchNamespace: coreNamespace,
  };
  const messenger = {
    send: vi.fn(async () => ({ id: "message-1" })),
    api: { conversations: { updateActivity: vi.fn(async () => undefined) } },
  };
  const gateway = new GatewayWatcher(config, store, messenger, {
    customObjectsApi: api.kubeConfig.makeApiClient(CustomObjectsApi),
    watch: new Watch(api.kubeConfig),
    reconnectDelayMs: 5,
  });
  watchers.push(gateway);
  return { gateway, messenger };
}

async function allWatching(api: KubernetesApiFixture): Promise<void> {
  await vi.waitFor(() => {
    for (const plural of plurals) expect(api.activeWatches(plural)).toHaveLength(1);
  });
}

afterEach(async () => {
  for (const gateway of watchers.splice(0)) gateway.stop();
  for (const api of fixtures.splice(0)) await api.close();
  vi.restoreAllMocks();
  vi.unstubAllEnvs();
});

describe("locked Kubernetes SDK request-object integration", () => {
  it("uses the SDK version from the unchanged gateway lock", () => {
    const require = createRequire(import.meta.url);
    const installed = JSON.parse(readFileSync(require.resolve("@kubernetes/client-node/package.json"), "utf8"));
    const locked = JSON.parse(readFileSync(new URL("../package-lock.json", import.meta.url), "utf8"));
    expect(installed.version).toBe(locked.packages["node_modules/@kubernetes/client-node"].version);
  });

  it.each(["kars-system", "bridge-private"])(
    "uses actual rendered %s configuration for /bind, command routing, SDK storage and watches", async (namespace) => {
      const chart = fileURLToPath(new URL("../../deploy/helm/kars-bridge", import.meta.url));
      const manifest = execFileSync("helm", [
        "template", "kars-bridge", chart, "--namespace", namespace,
        "--set", `namespace=${namespace},core.namespace=${coreNamespace},teamsGateway.enabled=true`,
        "--show-only", "templates/teams-gateway.yaml",
      ], { encoding: "utf8", timeout: 10_000 });
      const resources = manifest.split(/^---\s*$/m)
        .filter((document) => document.split("\n").some((line) => line.trim() && !line.trimStart().startsWith("#")))
        .map((document) => loadYaml<KubernetesObject>(document));
      const deployment = resources.find((object) => object.kind === "Deployment") as V1Deployment;
      for (const env of deployment.spec!.template.spec!.containers[0]!.env!) {
        if (env.value !== undefined) vi.stubEnv(env.name, env.value);
      }
      vi.stubEnv("TEAMS_CLIENT_ID", "fixture");
      vi.stubEnv("TEAMS_CLIENT_SECRET", "fixture");
      vi.stubEnv("TEAMS_TENANT_ID", binding.tenantId);
      vi.stubEnv("TEAMS_BFF_INTERNAL_SECRET", "fixture");
      vi.stubEnv("TEAMS_ENTRA_ROLE_MAP", JSON.stringify([{
        entra_subject: "operator", bridge_subject: "operator", roles: ["operator"], name: "Fixture Operator",
      }]));
      const config = loadConfig();
      const api = await fixture(namespace);
      const store = new KubernetesConversationStore({
        namespace: config.conversationConfigMapNamespace,
        configMapName: config.conversationConfigMapName, kubeConfig: api.kubeConfig,
      });
      await store.initialize();
      const handlers = new Map<string, (context: unknown) => unknown>();
      const app = {
        on: (route: string, handler: (context: unknown) => unknown) => { handlers.set(route, handler); },
        start: async () => undefined,
        send: vi.fn(async () => ({ id: "message-1" })),
      };
      const gateway = new GatewayWatcher(config, store, app, {
        customObjectsApi: api.kubeConfig.makeApiClient(CustomObjectsApi), watch: new Watch(api.kubeConfig),
      });
      watchers.push(gateway);
      const sendTeamCommand = vi.fn(async () => ({ success: true, message: "ok" }));
      registerAppHandlers(app, {
        config, store, bff: { submitDecision: vi.fn(), sendTeamCommand },
        reconcileTeamApprovals: (team) => gateway.reconcileTeamApprovals(team),
      });
      for (const text of ["/bind engineering", "/status"]) {
        await handlers.get("message")!({
          activity: { text, from: { aadObjectId: "operator" },
            conversation: { id: binding.conversationId, tenantId: binding.tenantId }, serviceUrl: binding.serviceUrl },
          send: async () => undefined,
        });
      }
      expect(sendTeamCommand).toHaveBeenNthCalledWith(1,
        expect.objectContaining({ command: "bind", namespace: coreNamespace }));
      expect(sendTeamCommand).toHaveBeenNthCalledWith(2,
        expect.objectContaining({ command: "status", namespace: coreNamespace }));
      expect(await store.getByConversation(binding.conversationId))
        .toMatchObject({ teamName: "engineering", namespace: coreNamespace });
      await gateway.start();
      await allWatching(api);
      expect(api.requests.some((request) => request.method === "PUT" && request.path === api.configMapPath)).toBe(true);
      expect(api.requests.every((request) =>
        request.path === api.configMapPath || plurals.some((plural) => request.path === resourcePath(plural)))).toBe(true);
    });

  it.each(["kars-system", "bridge-private"])(
    "initializes and persists/reloads core bindings using only the %s ConfigMap API", async (namespace) => {
      const api = await fixture(namespace);
      const store = api.store();
      await store.initialize();
      await store.bind(binding);
      const record = {
        approvalName: "approval-1", approvalNamespace: coreNamespace, conversationId: binding.conversationId,
        teamName: binding.teamName, messageId: "message-1", resourceVersion: "10",
        boundEnvelopeDigest: "sha256:bound", sentAt: binding.boundAt,
      };
      await Promise.all([
        store.recordApprovalMessage(record),
        store.setLastResourceVersion("watch.karsapprovals", "12"),
      ]);
      const reloaded = api.store();
      await reloaded.initialize();
      expect(await reloaded.getByConversation(binding.conversationId)).toEqual(binding);
      expect(await reloaded.getApprovalMessage(coreNamespace, "approval-1")).toEqual(record);
      expect(await reloaded.getLastResourceVersion("watch.karsapprovals")).toBe("12");
      expect(api.requests.every((request) => request.path === api.configMapPath)).toBe(true);
      const writes = api.requests.filter((request) => request.method === "PUT");
      expect(writes).toHaveLength(3);
      expect(writes.map((request) => request.body?.metadata?.resourceVersion)).toEqual(["1", "2", "3"]);
      for (const request of writes) {
        expect(request.body).toMatchObject({
          apiVersion: "v1", kind: "ConfigMap", metadata: { name: storeName, namespace },
          data: { "bindings.json": JSON.stringify([binding]) },
        });
      }
      await reloaded.bind({ ...binding, teamName: "operations" });
      expect(await reloaded.getByTeam("engineering")).toBeUndefined();
      expect((await reloaded.getByTeam("operations"))?.conversationId).toBe(binding.conversationId);
    });

  it.each([false, true])("retains 404 creation and 409 concurrent-creation recovery (race=%s)", async (race) => {
    const api = await fixture();
    api.configMap = undefined;
    api.createRace = race;
    await api.store().initialize();
    expect(api.requests.map(({ method, path }) => `${method} ${path}`)).toEqual([
      `GET ${api.configMapPath}`, `POST ${api.configMapCollectionPath}`,
      ...(race ? [`GET ${api.configMapPath}`] : []),
    ]);
    expect(api.requests[1]?.body).toEqual({
      apiVersion: "v1", kind: "ConfigMap", metadata: { name: storeName, namespace: api.storageNamespace },
      data: { "bindings.json": "[]", "approval-messages.json": "[]", "resource-versions.json": "{}" },
    });
  });

  it.each([403, 500])("propagates exact SDK read errors rather than creating on %s", async (code) => {
    const api = await fixture();
    api.failures.push({ method: "GET", path: api.configMapPath, code, watch: false });
    await expect(api.store().initialize()).rejects.toMatchObject(sdkFailure(code));
    expect(api.requests.map((request) => request.method)).toEqual(["GET"]);
  });

  it("propagates create/replace errors and keeps the persistence queue usable", async () => {
    const api = await fixture();
    api.configMap = undefined;
    api.failures.push({ method: "POST", path: api.configMapCollectionPath, code: 403, watch: false });
    await expect(api.store().initialize()).rejects.toMatchObject(sdkFailure(403));
    const store = api.store();
    await store.initialize();
    api.failures.push({ method: "PUT", path: api.configMapPath, code: 409, watch: false });
    await expect(store.bind(binding)).rejects.toMatchObject(sdkFailure(409));
    await store.bind(binding);
    expect(JSON.parse(api.configMap!.data!["bindings.json"]!)).toEqual([binding]);
  });

  it("preserves list errors and the missing resourceVersion error", async () => {
    const api = await fixture();
    const { gateway } = watcher(api);
    api.failures.push({ method: "GET", path: resourcePath("karsapprovals"), code: 403, watch: false });
    await expect(gateway.reconcileTeamApprovals("engineering")).rejects.toMatchObject(sdkFailure(403));
    api.listResourceVersion = undefined;
    await expect(gateway.reconcileTeamApprovals("engineering")).rejects.toThrow(
      "list karsapprovals did not return a resourceVersion");
    expect(api.requests.every((request) => request.path === resourcePath("karsapprovals"))).toBe(true);
  });

  it("lists/watches all core resources, persists bookmarks, updates cards and deduplicates after restart", async () => {
    const api = await fixture();
    api.lists.set("karsapprovals", [approval]);
    const task = { metadata: { ...approval.metadata, name: "task-1" }, status: { phase: "Pending" } };
    const team = { metadata: { ...approval.metadata, name: "engineering" }, status: { phase: "Forming" } };
    api.lists.set("karstasks", [task]);
    api.lists.set("karsteams", [team]);
    const store = api.store();
    await store.initialize();
    await store.bind(binding);
    const { gateway, messenger } = watcher(api, store);
    await gateway.start();
    await allWatching(api);
    expect(messenger.send).toHaveBeenCalledTimes(1);
    for (const plural of plurals) {
      expect(api.listCount(plural)).toBe(1);
      const request = api.requests.find((item) => item.path === resourcePath(plural) && item.query.get("watch") === "true")!;
      expect(Object.fromEntries(request.query)).toEqual({
        allowWatchBookmarks: "true", resourceVersion: "10", timeoutSeconds: "300", watch: "true",
      });
    }
    const updated = { ...approval, metadata: { ...approval.metadata, resourceVersion: "11" } };
    api.lists.set("karsapprovals", [updated]);
    api.emit("karsapprovals", "MODIFIED", updated);
    api.emit("karstasks", "MODIFIED", { ...task, status: { phase: "Ready" } });
    api.emit("karsteams", "MODIFIED", { ...team, status: { phase: "Active" } });
    for (const plural of plurals) api.emit(plural, "BOOKMARK", { metadata: { resourceVersion: "12" } });
    await vi.waitFor(async () => {
      for (const plural of plurals) expect(await store.getLastResourceVersion(`watch.${plural}`)).toBe("12");
      expect(messenger.send).toHaveBeenCalledTimes(3);
      expect(messenger.api.conversations.updateActivity).toHaveBeenCalledTimes(1);
    });
    expect(messenger.api.conversations.updateActivity).toHaveBeenCalledWith(
      binding.conversationId, "message-1", expect.objectContaining({ type: "message" }));
    gateway.stop();
    await vi.waitFor(() => {
      for (const plural of plurals) expect(api.activeWatches(plural)).toHaveLength(0);
    });
    const reloaded = api.store();
    await reloaded.initialize();
    expect(await reloaded.getLastResourceVersion("watch.karsapprovals")).toBe("12");
    expect((await reloaded.getApprovalMessage(coreNamespace, "approval-1"))?.resourceVersion).toBe("11");
    const restarted = watcher(api, reloaded);
    await restarted.gateway.start();
    await allWatching(api);
    expect(restarted.messenger.send).not.toHaveBeenCalled();
    expect(restarted.messenger.api.conversations.updateActivity).not.toHaveBeenCalled();
    expect(api.requests.every((request) =>
      request.path === api.configMapPath || plurals.some((plural) => request.path === resourcePath(plural)))).toBe(true);
  });

  it("retries list failures, resumes a closed watch from bookmarks and relists after HTTP 410/500", async () => {
    const api = await fixture();
    const store = api.store();
    await store.initialize();
    api.failures.push({ method: "GET", path: resourcePath("karsapprovals"), code: 503, watch: false });
    const { gateway } = watcher(api, store);
    await gateway.start();
    await allWatching(api);
    expect(api.listCount("karsapprovals")).toBe(2);
    api.emit("karsapprovals", "BOOKMARK", { metadata: { resourceVersion: "12" } });
    await vi.waitFor(async () => expect(await store.getLastResourceVersion("watch.karsapprovals")).toBe("12"));
    api.activeWatches("karsapprovals")[0]!.end();
    await allWatching(api);
    expect(api.listCount("karsapprovals")).toBe(2);
    expect(api.requests.filter((request) => request.path === resourcePath("karsapprovals")).at(-1)?.query.get("resourceVersion")).toBe("12");
    for (const code of [410, 500]) {
      const count = api.listCount("karsapprovals");
      api.failures.push({ method: "GET", path: resourcePath("karsapprovals"), code, watch: true });
      api.activeWatches("karsapprovals")[0]!.end();
      await vi.waitFor(() => expect(api.listCount("karsapprovals")).toBe(count + 1));
      await allWatching(api);
    }
  });

  it("accepts real SDK clients at the typed injection boundary without compatibility casts", async () => {
    const api = await fixture();
    const store = new KubernetesConversationStore({
      namespace: api.storageNamespace, configMapName: storeName,
      api: api.kubeConfig.makeApiClient(CoreV1Api),
    });
    await store.initialize();
    expect(api.requests[0]?.path).toBe(api.configMapPath);
  });
});
