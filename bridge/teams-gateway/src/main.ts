import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { pathToFileURL } from "node:url";
import { App } from "@microsoft/teams.apps";
import type {
  AdaptiveCardActionCardResponse,
  AdaptiveCardActionMessageResponse,
} from "@microsoft/teams.api";
import { BffClient, type TeamCommandName } from "./bff-client.js";
import {
  buildApprovalCard,
  buildDecidedCard,
  buildProgressCard,
  isCardActionPayload,
  type ApprovalEvent,
  type ProgressEvent,
} from "./cards.js";
import { loadConfig, type TeamsGatewayConfig } from "./config.js";
import {
  InMemoryConversationStore,
  KubernetesConversationStore,
  type ConversationStore,
} from "./conversation-store.js";
import { SIGNATURE_HEADER, verifyHmac } from "./hmac.js";
import { resolveIdentity, type TeamsIdentity } from "./identity.js";
import { log } from "./log.js";
import { GatewayWatcher } from "./watcher.js";

interface TeamsMessageContext {
  readonly activity: TeamsActivity;
  send(activity: TeamsOutboundActivity): Promise<unknown>;
}

interface TeamsCardActionContext {
  readonly activity: TeamsActivity;
}

interface TeamsActivity {
  readonly text?: string | undefined;
  readonly from?: {
    readonly aadObjectId?: string | undefined;
    readonly name?: string | undefined;
  } | undefined;
  readonly conversation?: {
    readonly id?: string | undefined;
    readonly tenantId?: string | undefined;
  } | undefined;
  readonly serviceUrl?: string | undefined;
  readonly value?: {
    readonly action?: {
      readonly data?: unknown;
    } | undefined;
  } | undefined;
}

interface TeamsOutboundActivity {
  readonly type: "message";
  readonly text?: string | undefined;
  readonly attachments?: readonly {
    readonly contentType: "application/vnd.microsoft.card.adaptive";
    readonly content: object;
  }[];
}

interface TeamsAppLike {
  on(route: string, handler: (context: unknown) => unknown): void;
  start(port?: number | string): Promise<void>;
  send(
    conversationId: string,
    activity: TeamsOutboundActivity
  ): Promise<{ id?: string | undefined } | null>;
  readonly api?: {
    readonly conversations?: {
      updateActivity(
        conversationId: string,
        activityId: string,
        activity: TeamsOutboundActivity
      ): Promise<unknown>;
    } | undefined;
  } | undefined;
}

export interface GatewayHandlerDependencies {
  readonly config: TeamsGatewayConfig;
  readonly store: ConversationStore;
  readonly bff: Pick<BffClient, "submitDecision" | "sendTeamCommand">;
  readonly reconcileTeamApprovals?: ((teamName: string) => Promise<void>) | undefined;
}

interface GatewayCommand {
  readonly name: "bind" | TeamCommandName;
  readonly args: string;
}

interface InternalNotifyRequest {
  readonly teamName?: string | undefined;
  readonly team?: string | undefined;
  readonly approval?: ApprovalEvent | undefined;
  readonly progress?: ProgressEvent | undefined;
  readonly event?: ApprovalEvent | undefined;
  readonly updateMessageId?: string | undefined;
}

const HELP_TEXT =
  "Commands: `/bind <team-name>`, `/status`, `/list-tasks`, `/add-task <title>`, `/run`, `/halt <run-name> [reason]`";

function adaptiveCardActivity(card: object): TeamsOutboundActivity {
  return {
    type: "message",
    attachments: [
      {
        contentType: "application/vnd.microsoft.card.adaptive",
        content: card,
      },
    ],
  };
}

function messageResponse(
  value: string
): AdaptiveCardActionMessageResponse {
  return {
    statusCode: 200,
    type: "application/vnd.microsoft.activity.message",
    value,
  };
}

export function parseGatewayCommand(text: string): GatewayCommand | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith("/")) {
    return null;
  }
  const withoutSlash = trimmed.slice(1);
  const [rawCommand, ...rest] = withoutSlash.split(/\s+/);
  const args = rest.join(" ").trim();
  switch (rawCommand) {
    case "bind":
    case "status":
    case "list-tasks":
    case "add-task":
    case "run":
    case "halt":
      return { name: rawCommand, args };
    default:
      return null;
  }
}

export function extractIdentity(
  activity: TeamsActivity
): TeamsIdentity | undefined {
  const aadObjectId = activity.from?.aadObjectId?.trim();
  if (!aadObjectId) {
    return undefined;
  }
  return {
    aadObjectId,
    displayName: activity.from?.name?.trim() || "Unknown",
  };
}

export async function handleApprovalCardAction(
  context: TeamsCardActionContext,
  dependencies: GatewayHandlerDependencies
): Promise<
  AdaptiveCardActionCardResponse | AdaptiveCardActionMessageResponse
> {
  const principal = resolveIdentity(
    dependencies.config,
    extractIdentity(context.activity)
  );
  if (!principal) {
    return messageResponse("⛔ You are not authorized for this action.");
  }

  const actionData = context.activity.value?.action?.data;
  if (!isCardActionPayload(actionData)) {
    return messageResponse("Invalid action payload.");
  }

  const requestChangesReason =
    typeof context.activity.value?.action?.data === "object" &&
    context.activity.value?.action?.data !== null &&
    typeof (
      context.activity.value.action.data as {
        requestChangesReason?: unknown;
      }
    ).requestChangesReason === "string"
      ? (
          context.activity.value.action.data as {
            requestChangesReason: string;
          }
        ).requestChangesReason.trim()
      : "";

  if (
    actionData.verdict === "request-changes" &&
    requestChangesReason.length === 0
  ) {
    return messageResponse(
      "⚠️ Request Changes requires written feedback. Fill in the Feedback field and try again."
    );
  }

  const decision = await dependencies.bff.submitDecision({
    approvalName: actionData.approvalName,
    approvalNamespace: actionData.approvalNamespace,
    verdict: actionData.verdict,
    reason:
      requestChangesReason.length > 0 ? requestChangesReason : undefined,
    resourceVersion: actionData.resourceVersion,
    ...(actionData.boundEnvelopeDigest !== undefined
      ? { boundEnvelopeDigest: actionData.boundEnvelopeDigest }
      : {}),
    principal,
  });

  if (!decision.success) {
    return messageResponse(
      `⚠️ Decision failed: ${decision.error ?? "unknown error"}`
    );
  }

  const decidedCard = buildDecidedCard(
    {
      name: actionData.approvalName,
      namespace: actionData.approvalNamespace,
      task: actionData.approvalName,
      actionKind: "approval",
      summary: `Approval ${actionData.approvalName}`,
      resourceVersion: actionData.resourceVersion,
      ...(actionData.boundEnvelopeDigest !== undefined
        ? { boundEnvelopeDigest: actionData.boundEnvelopeDigest }
        : {}),
    },
    actionData.verdict,
    principal.name,
    requestChangesReason.length > 0 ? requestChangesReason : undefined
  );

  return {
    statusCode: 200,
    type: "application/vnd.microsoft.card.adaptive",
    value: decidedCard,
  };
}

export function registerAppHandlers(
  app: TeamsAppLike,
  dependencies: GatewayHandlerDependencies
): void {
  app.on("message", async (context: unknown) => {
    await handleMessage(context as TeamsMessageContext, dependencies);
  });
  app.on("card.action.kars.approval.decision", async (context: unknown) => {
    return handleApprovalCardAction(
      context as TeamsCardActionContext,
      dependencies
    );
  });
  app.on("install.add", async (context: unknown) => {
    await handleInstall(context as TeamsMessageContext, dependencies);
  });
}

async function handleMessage(
  context: TeamsMessageContext,
  dependencies: GatewayHandlerDependencies
): Promise<void> {
  const principal = resolveIdentity(
    dependencies.config,
    extractIdentity(context.activity)
  );
  if (!principal) {
    await context.send({
      type: "message",
      text: "⛔ You are not authorized for this gateway.",
    });
    return;
  }

  const command = parseGatewayCommand(context.activity.text ?? "");
  if (!command) {
    await context.send({ type: "message", text: HELP_TEXT });
    return;
  }

  if (command.name === "bind") {
    const teamName = command.args.trim();
    if (!teamName) {
      await context.send({
        type: "message",
        text: "Usage: `/bind <team-name>`",
      });
      return;
    }
    const conversationId = context.activity.conversation?.id?.trim();
    if (!conversationId) {
      await context.send({
        type: "message",
        text: "Unable to bind this conversation.",
      });
      return;
    }
    const ownership = await dependencies.bff.sendTeamCommand({
      teamName,
      namespace: dependencies.config.watchNamespace,
      command: "bind",
      args: "",
      principal,
    });
    if (!ownership.success) {
      await context.send({
        type: "message",
        text: `Unable to bind team **${teamName}**: ${ownership.error ?? ownership.message}`,
      });
      return;
    }
    await dependencies.store.bind({
      conversationId,
      serviceUrl: context.activity.serviceUrl ?? "",
      tenantId:
        context.activity.conversation?.tenantId ??
        dependencies.config.tenantId,
      teamName,
      namespace: dependencies.config.watchNamespace,
      boundAt: new Date().toISOString(),
    });
    await dependencies.reconcileTeamApprovals?.(teamName);
    await context.send({
      type: "message",
      text: `Bound this conversation to team **${teamName}**.`,
    });
    return;
  }

  const conversationId = context.activity.conversation?.id?.trim() ?? "";
  const binding = conversationId
    ? await dependencies.store.getByConversation(conversationId)
    : undefined;
  if (!binding?.teamName) {
    await context.send({
      type: "message",
      text: "This conversation is not bound to a Kars team yet. Use `/bind <team-name>` first.",
    });
    return;
  }

  const result = await dependencies.bff.sendTeamCommand({
    teamName: binding.teamName,
    namespace: binding.namespace || dependencies.config.watchNamespace,
    command: command.name,
    args: command.args,
    principal,
  });
  await context.send({
    type: "message",
    text: result.success
      ? result.message
      : `⚠️ ${result.error ?? "Command failed."}`,
  });
}

async function handleInstall(
  context: TeamsMessageContext,
  dependencies: GatewayHandlerDependencies
): Promise<void> {
  const conversationId = context.activity.conversation?.id?.trim();
  if (!conversationId) {
    return;
  }
  await dependencies.store.bind({
    conversationId,
    serviceUrl: context.activity.serviceUrl ?? "",
    tenantId:
      context.activity.conversation?.tenantId ?? dependencies.config.tenantId,
    teamName: "",
    namespace: dependencies.config.watchNamespace,
    boundAt: new Date().toISOString(),
  });
  await context.send({
    type: "message",
    text: "Kars Teams Gateway installed. Use `/bind <team-name>` to connect this conversation.",
  });
}

async function initConversationStore(
  config: TeamsGatewayConfig
): Promise<ConversationStore> {
  if (process.env["KUBERNETES_SERVICE_HOST"]) {
    const store = new KubernetesConversationStore({
      namespace: config.conversationConfigMapNamespace,
      configMapName: config.conversationConfigMapName,
    });
    await store.initialize();
    return store;
  }
  log("info", "using in-memory Teams conversation store");
  return new InMemoryConversationStore();
}

async function readBody(request: IncomingMessage): Promise<string> {
  return new Promise<string>((resolve, reject) => {
    const chunks: Buffer[] = [];
    request.on("data", (chunk) => {
      chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
    });
    request.on("end", () => {
      resolve(Buffer.concat(chunks).toString("utf8"));
    });
    request.on("error", reject);
  });
}

function getHeader(
  request: IncomingMessage,
  headerName: string
): string | null {
  const value = request.headers[headerName.toLowerCase()];
  if (typeof value === "string") {
    return value;
  }
  if (Array.isArray(value) && value.length > 0) {
    return value[0] ?? null;
  }
  return null;
}

export async function handleInternalNotify(
  request: IncomingMessage,
  response: ServerResponse,
  dependencies: GatewayHandlerDependencies,
  app: TeamsAppLike
): Promise<void> {
  const rawBody = await readBody(request);
  const signature = getHeader(request, SIGNATURE_HEADER);
  if (!verifyHmac(dependencies.config.bffInternalSecret, rawBody, signature)) {
    response.writeHead(401, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ error: "unauthorized" }));
    return;
  }

  let payload: InternalNotifyRequest;
  try {
    payload = JSON.parse(rawBody) as InternalNotifyRequest;
  } catch {
    response.writeHead(400, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ error: "invalid JSON" }));
    return;
  }

  const teamName = payload.teamName ?? payload.team;
  if (!teamName) {
    response.writeHead(400, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ error: "missing team name" }));
    return;
  }
  const binding = await dependencies.store.getByTeam(teamName);
  if (!binding) {
    response.writeHead(404, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ error: "team is not bound" }));
    return;
  }

  const approval = payload.approval ?? payload.event;
  const progress = payload.progress;
  if (!approval && !progress) {
    response.writeHead(400, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ error: "missing approval or progress payload" }));
    return;
  }

  const card = approval
    ? buildApprovalCard(approval)
    : buildProgressCard(progress as ProgressEvent);
  const updateMessageId = payload.updateMessageId?.trim();

  if (
    updateMessageId &&
    app.api?.conversations?.updateActivity !== undefined
  ) {
    await app.api.conversations.updateActivity(
      binding.conversationId,
      updateMessageId,
      adaptiveCardActivity(card)
    );
    response.writeHead(200, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ sent: true, updated: true }));
    return;
  }

  const sent = await app.send(
    binding.conversationId,
    adaptiveCardActivity(card)
  );
  if (approval && approval.boundEnvelopeDigest) {
    await dependencies.store.recordApprovalMessage({
      approvalName: approval.name,
      approvalNamespace: approval.namespace,
      conversationId: binding.conversationId,
      teamName: binding.teamName,
      messageId: sent?.id ?? "",
      resourceVersion: approval.resourceVersion,
      boundEnvelopeDigest: approval.boundEnvelopeDigest,
      sentAt: new Date().toISOString(),
    });
  }
  response.writeHead(200, { "Content-Type": "application/json" });
  response.end(JSON.stringify({ sent: true, id: sent?.id ?? null }));
}

export async function main(): Promise<void> {
  const config = loadConfig();
  const store = await initConversationStore(config);
  const bff = new BffClient(config);
  const app = new App({
    clientId: config.clientId,
    clientSecret: config.clientSecret,
    tenantId: config.tenantId,
  });
  const watcher = process.env["KUBERNETES_SERVICE_HOST"]
    ? new GatewayWatcher(config, store, app)
    : undefined;

  registerAppHandlers(app as unknown as TeamsAppLike, {
    config,
    store,
    bff,
    reconcileTeamApprovals: watcher
      ? (teamName) => watcher.reconcileTeamApprovals(teamName)
      : undefined,
  });

  await app.start(config.port);

  const internalServer = createServer(async (request, response) => {
    const url = request.url ?? "";
    if (url === "/healthz" || url === "/healthz/") {
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(JSON.stringify({ status: "ok" }));
      return;
    }
    if (
      (url === "/api/notify" || url === "/api/notify/") &&
      request.method === "POST"
    ) {
      await handleInternalNotify(
        request,
        response,
        { config, store, bff },
        app as unknown as TeamsAppLike
      );
      return;
    }
    response.writeHead(404, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ error: "not found" }));
  });
  internalServer.listen(config.internalPort, "0.0.0.0", () => {
    log("info", "Teams gateway internal server listening", {
      port: String(config.internalPort),
    });
  });

  if (watcher) {
    await watcher.start();
  }

  log("info", "Teams gateway started", {
    port: String(config.port),
    internalPort: String(config.internalPort),
  });
}

const directRun =
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(process.argv[1]).href;

if (directRun) {
  void main().catch((error) => {
    log("error", "fatal Teams gateway startup failure", {
      error: error instanceof Error ? error.message : String(error),
    });
    process.exit(1);
  });
}
