import { CustomObjectsApi, KubeConfig, Watch } from "@kubernetes/client-node";
import type { App } from "@microsoft/teams.apps";
import {
  buildApprovalCard,
  buildDecidedCard,
  buildProgressCard,
  type ApprovalEvent,
  type CardVerdict,
  type ProgressEvent,
} from "./cards.js";
import type { TeamsGatewayConfig } from "./config.js";
import type {
  ApprovalMessageRecord,
  ConversationStore,
} from "./conversation-store.js";
import { log } from "./log.js";
import type {
  Metadata,
  ApprovalResource,
  TaskResource,
  TeamResource,
  KubernetesList,
  CustomObjectsApiLike,
  WatchLike,
  TeamsMessenger,
  TeamsActivity,
} from "./watcher-types.js";

const GROUP = "kars.azure.com";
const VERSION = "v1alpha1";
const APPROVALS_PLURAL = "karsapprovals";
const TASKS_PLURAL = "karstasks";
const TEAMS_PLURAL = "karsteams";
const APPROVALS_STREAM = "watch.karsapprovals";
const TASKS_STREAM = "watch.karstasks";
const TEAMS_STREAM = "watch.karsteams";
const WATCH_TIMEOUT_SECONDS = 300;
const TEAM_METADATA_KEY = "kars.azure.com/team";

export interface GatewayWatcherOptions {
  readonly customObjectsApi?: CustomObjectsApiLike | undefined;
  readonly watch?: WatchLike | undefined;
  readonly reconnectDelayMs?: number | undefined;
}

function normalizeString(value: unknown): string | undefined {
  if (typeof value !== "string") {
    return undefined;
  }
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

function unwrapResponse<T>(response: unknown): T {
  if (
    typeof response === "object" &&
    response !== null &&
    "body" in response
  ) {
    return (response as { body: T }).body;
  }
  return response as T;
}

function statusCodeOf(error: unknown): number | undefined {
  if (typeof error !== "object" || error === null) {
    return undefined;
  }
  const statusCode = (error as { statusCode?: unknown }).statusCode;
  if (typeof statusCode === "number") {
    return statusCode;
  }
  const code = (error as { code?: unknown }).code;
  return typeof code === "number" ? code : undefined;
}

function isAbortError(error: unknown): boolean {
  return (
    error instanceof Error &&
    (error.name === "AbortError" || error.message.includes("aborted"))
  );
}

function metadataValue(
  metadata: Metadata | undefined,
  key: string
): string | undefined {
  return metadata?.labels?.[key] ?? metadata?.annotations?.[key];
}

function isTerminalPhase(phase: string): boolean {
  return phase !== "Pending";
}

function toActivity(card: object): TeamsActivity {
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

function toApprovalEvent(resource: ApprovalResource): ApprovalEvent | undefined {
  const metadata = resource.metadata;
  const name = normalizeString(metadata?.name);
  const namespace = normalizeString(metadata?.namespace);
  const task = normalizeString(resource.spec?.taskRef?.name);
  const actionKind = normalizeString(resource.spec?.action?.kind);
  const summary = normalizeString(resource.spec?.action?.summary);
  const resourceVersion = normalizeString(metadata?.resourceVersion);
  if (!name || !namespace || !task || !actionKind || !summary || !resourceVersion) {
    return undefined;
  }
  const detail = normalizeString(resource.spec?.action?.detail);
  const team = normalizeString(metadataValue(metadata, TEAM_METADATA_KEY));
  const boundEnvelopeDigest = normalizeString(
    resource.status?.boundEnvelopeDigest
  );
  return {
    name,
    namespace,
    task,
    ...(team !== undefined ? { team } : {}),
    actionKind,
    summary,
    ...(detail !== undefined ? { detail } : {}),
    ...(resource.spec?.action?.requestedTier !== undefined
      ? { requestedTier: resource.spec.action.requestedTier }
      : {}),
    resourceVersion,
    ...(boundEnvelopeDigest !== undefined ? { boundEnvelopeDigest } : {}),
  };
}

function taskSignature(resource: TaskResource): string {
  return JSON.stringify({
    phase: resource.status?.phase ?? "",
    executionPhase: resource.status?.executionPhase ?? "",
    executionDetail: resource.status?.executionDetail ?? "",
    assignmentSequence: resource.status?.assignmentSequence ?? 0,
    assignmentState: resource.status?.assignment?.state ?? "",
    assignmentStage: resource.status?.assignment?.stage ?? "",
    assignmentError: resource.status?.assignment?.error ?? "",
  });
}

function teamSignature(resource: TeamResource): string {
  return JSON.stringify({
    phase: resource.status?.phase ?? "",
    health: resource.status?.health ?? "",
    detail: resource.status?.detail ?? "",
    runtimeState: resource.status?.runtimeState ?? "",
    generatedTaskCount: resource.status?.generatedTaskCount ?? 0,
    lastGeneratedTask: resource.status?.lastGeneratedTask ?? "",
    currentAssignmentTask: resource.status?.currentAssignmentTask ?? "",
    paused: resource.spec?.paused ?? false,
  });
}

function toTaskProgressEvent(resource: TaskResource): ProgressEvent | undefined {
  const metadata = resource.metadata;
  const taskName = normalizeString(metadata?.name);
  const teamName = normalizeString(metadataValue(metadata, TEAM_METADATA_KEY));
  if (!taskName || !teamName) {
    return undefined;
  }
  const title =
    normalizeString(resource.spec?.displayName) ??
    normalizeString(resource.spec?.objective) ??
    taskName;
  const phase = normalizeString(resource.status?.phase) ?? "Pending";
  const executionPhase = normalizeString(resource.status?.executionPhase) ?? "Idle";
  const stage = normalizeString(resource.status?.assignment?.stage);
  const detail = normalizeString(resource.status?.executionDetail);
  const summary =
    detail ??
    normalizeString(resource.status?.assignment?.error) ??
    normalizeString(resource.status?.assignment?.state) ??
    `${title} changed state.`;
  return {
    kind: "task",
    teamName,
    resourceName: taskName,
    title,
    status: `${phase} / ${executionPhase}`,
    summary,
    ...(detail !== undefined ? { detail } : {}),
    ...(stage !== undefined ? { stage } : {}),
  };
}

function toTeamProgressEvent(resource: TeamResource): ProgressEvent | undefined {
  const metadata = resource.metadata;
  const teamName = normalizeString(metadata?.name);
  if (!teamName) {
    return undefined;
  }
  const title = normalizeString(resource.spec?.displayName) ?? teamName;
  const phase = normalizeString(resource.status?.phase) ?? "Forming";
  const detail = normalizeString(resource.status?.detail);
  const summary =
    detail ??
    normalizeString(resource.status?.health) ??
    (resource.spec?.paused ? "Standing team paused." : "Standing team updated.");
  const runtimeState = normalizeString(resource.status?.runtimeState);
  return {
    kind: "team",
    teamName,
    resourceName: teamName,
    title,
    status: phase,
    summary,
    ...(detail !== undefined ? { detail } : {}),
    ...(runtimeState !== undefined ? { stage: runtimeState } : {}),
  };
}

export class GatewayWatcher {
  private readonly config: TeamsGatewayConfig;
  private readonly store: ConversationStore;
  private readonly app: TeamsMessenger;
  private readonly customObjectsApi: CustomObjectsApiLike;
  private readonly watch: WatchLike;
  private readonly reconnectDelayMs: number;
  private readonly abortControllers = new Set<AbortController>();
  private readonly taskState = new Map<string, string>();
  private readonly teamState = new Map<string, string>();
  private running = false;

  public constructor(
    config: TeamsGatewayConfig,
    store: ConversationStore,
    app: App | TeamsMessenger,
    options?: GatewayWatcherOptions | undefined
  ) {
    this.config = config;
    this.store = store;
    this.app = app as TeamsMessenger;
    this.reconnectDelayMs = options?.reconnectDelayMs ?? 2000;
    if (options?.customObjectsApi && options.watch) {
      this.customObjectsApi = options.customObjectsApi;
      this.watch = options.watch;
    } else {
      const kubeConfig = new KubeConfig();
      kubeConfig.loadFromCluster();
      this.customObjectsApi =
        (options?.customObjectsApi ??
          (kubeConfig.makeApiClient(
            CustomObjectsApi
          ) as unknown as CustomObjectsApiLike));
      this.watch = options?.watch ?? new Watch(kubeConfig);
    }
  }

  public async start(): Promise<void> {
    if (this.running) {
      return;
    }

    this.running = true;
    void this.runApprovalLoop();
    void this.runTaskLoop();
    void this.runTeamLoop();
    log("info", "started Teams gateway Kubernetes watchers", {
      namespace: this.config.watchNamespace,
    });
  }

  public async reconcileTeamApprovals(teamName: string): Promise<void> {
    const listed = await this.listResource<ApprovalResource>(APPROVALS_PLURAL);
    for (const approval of listed.items) {
      if (
        normalizeString(approval.metadata?.labels?.[TEAM_METADATA_KEY]) ===
        teamName
      ) {
        await this.reconcileApproval(approval);
      }
    }
  }

  public stop(): void {
    this.running = false;
    for (const controller of this.abortControllers) {
      controller.abort();
    }
    this.abortControllers.clear();
  }

  private async runApprovalLoop(): Promise<void> {
    let shouldRelist = true;
    while (this.running) {
      try {
        let resourceVersion = await this.store.getLastResourceVersion(
          APPROVALS_STREAM
        );
        if (shouldRelist || !resourceVersion) {
          const listed = await this.listResource<ApprovalResource>(
            APPROVALS_PLURAL
          );
          resourceVersion = listed.resourceVersion;
          await this.store.setLastResourceVersion(
            APPROVALS_STREAM,
            resourceVersion
          );
          for (const approval of listed.items) {
            await this.reconcileApproval(approval);
          }
          shouldRelist = false;
        }
        await this.watchResource<ApprovalResource>(
          APPROVALS_STREAM,
          APPROVALS_PLURAL,
          resourceVersion,
          async (phase, approval) => {
            if (phase === "ADDED" || phase === "MODIFIED") {
              await this.reconcileApproval(approval);
            }
          }
        );
      } catch (error) {
        if (!this.running || isAbortError(error)) {
          return;
        }
        if (statusCodeOf(error) === 410) {
          shouldRelist = true;
          continue;
        }
        shouldRelist = true;
        log("warn", "approval watch disconnected; retrying", {
          error: error instanceof Error ? error.message : String(error),
        });
        await this.sleep(this.reconnectDelayMs);
      }
    }
  }

  private async runTaskLoop(): Promise<void> {
    let shouldRelist = true;
    while (this.running) {
      try {
        let resourceVersion = await this.store.getLastResourceVersion(TASKS_STREAM);
        if (shouldRelist || !resourceVersion) {
          const listed = await this.listResource<TaskResource>(TASKS_PLURAL);
          resourceVersion = listed.resourceVersion;
          await this.store.setLastResourceVersion(TASKS_STREAM, resourceVersion);
          const recovering = this.taskState.size > 0;
          for (const task of listed.items) {
            await this.handleTaskProgress(task, !recovering);
          }
          shouldRelist = false;
        }
        await this.watchResource<TaskResource>(
          TASKS_STREAM,
          TASKS_PLURAL,
          resourceVersion,
          async (phase, task) => {
            if (phase !== "ADDED" && phase !== "MODIFIED") {
              return;
            }
            await this.handleTaskProgress(task, phase === "ADDED");
          }
        );
      } catch (error) {
        if (!this.running || isAbortError(error)) {
          return;
        }
        if (statusCodeOf(error) === 410) {
          shouldRelist = true;
          continue;
        }
        shouldRelist = true;
        log("warn", "task watch disconnected; retrying", {
          error: error instanceof Error ? error.message : String(error),
        });
        await this.sleep(this.reconnectDelayMs);
      }
    }
  }

  private async runTeamLoop(): Promise<void> {
    let shouldRelist = true;
    while (this.running) {
      try {
        let resourceVersion = await this.store.getLastResourceVersion(TEAMS_STREAM);
        if (shouldRelist || !resourceVersion) {
          const listed = await this.listResource<TeamResource>(TEAMS_PLURAL);
          resourceVersion = listed.resourceVersion;
          await this.store.setLastResourceVersion(TEAMS_STREAM, resourceVersion);
          const recovering = this.teamState.size > 0;
          for (const team of listed.items) {
            await this.handleTeamProgress(team, !recovering);
          }
          shouldRelist = false;
        }
        await this.watchResource<TeamResource>(
          TEAMS_STREAM,
          TEAMS_PLURAL,
          resourceVersion,
          async (phase, team) => {
            if (phase !== "ADDED" && phase !== "MODIFIED") {
              return;
            }
            await this.handleTeamProgress(team, phase === "ADDED");
          }
        );
      } catch (error) {
        if (!this.running || isAbortError(error)) {
          return;
        }
        if (statusCodeOf(error) === 410) {
          shouldRelist = true;
          continue;
        }
        shouldRelist = true;
        log("warn", "team watch disconnected; retrying", {
          error: error instanceof Error ? error.message : String(error),
        });
        await this.sleep(this.reconnectDelayMs);
      }
    }
  }

  private async listResource<T>(plural: string): Promise<{
    readonly items: readonly T[];
    readonly resourceVersion: string;
  }> {
    const response = unwrapResponse<KubernetesList<T>>(
      await this.customObjectsApi.listNamespacedCustomObject(
        GROUP,
        VERSION,
        this.config.watchNamespace,
        plural
      )
    );
    const resourceVersion = normalizeString(
      response.metadata?.resourceVersion
    );
    if (!resourceVersion) {
      throw new Error(`list ${plural} did not return a resourceVersion`);
    }
    return {
      items: response.items ?? [],
      resourceVersion,
    };
  }

  private async watchResource<T>(
    stream: string,
    plural: string,
    resourceVersion: string,
    onEvent: (phase: string, resource: T) => Promise<void>
  ): Promise<void> {
    const path = `/apis/${GROUP}/${VERSION}/namespaces/${this.config.watchNamespace}/${plural}`;
    let pending = Promise.resolve();
    let controller: AbortController | undefined;
    let processingError: unknown;
    let resolveDone!: () => void;
    let rejectDone!: (error: unknown) => void;
    const done = new Promise<void>((resolve, reject) => {
      resolveDone = resolve;
      rejectDone = reject;
    });
    controller = await this.watch.watch(
      path,
      {
        allowWatchBookmarks: true,
        resourceVersion,
        timeoutSeconds: WATCH_TIMEOUT_SECONDS,
      },
      (phase, resource) => {
        pending = pending
          .then(async () => {
            const metadata =
              typeof resource === "object" && resource !== null
                ? (resource as { metadata?: Metadata }).metadata
                : undefined;
            const nextResourceVersion = normalizeString(
              metadata?.resourceVersion
            );
            if (phase === "BOOKMARK") {
              if (nextResourceVersion) {
                await this.store.setLastResourceVersion(
                  stream,
                  nextResourceVersion
                );
              }
              return;
            }
            await onEvent(phase, resource as T);
            if (nextResourceVersion) {
              await this.store.setLastResourceVersion(
                stream,
                nextResourceVersion
              );
            }
          })
          .catch((error) => {
            processingError = error;
            controller?.abort();
          });
      },
      (error) => {
        void pending.finally(() => {
          if (processingError) {
            rejectDone(processingError);
          } else if (error && !isAbortError(error)) {
            rejectDone(error);
          } else {
            resolveDone();
          }
        });
      }
    );
    if (processingError) {
      controller.abort();
    }
    this.abortControllers.add(controller);
    await done.finally(() => {
      if (controller) {
        this.abortControllers.delete(controller);
      }
    });
  }

  private async reconcileApproval(resource: ApprovalResource): Promise<void> {
    const event = toApprovalEvent(resource);
    if (!event) {
      return;
    }
    const phase = normalizeString(resource.status?.phase) ?? "Pending";
    const decision = resource.spec?.decision;
    const existing = await this.store.getApprovalMessage(
      event.namespace,
      event.name
    );

    if (phase === "Pending") {
      if (!event.boundEnvelopeDigest || !event.team) {
        return;
      }
      if (!existing) {
        await this.sendApprovalCard(event);
        return;
      }
      const binding = await this.store.getByTeam(event.team);
      if (binding && existing.conversationId !== binding.conversationId) {
        await this.sendApprovalCard(event);
        return;
      }
      if (
        existing.boundEnvelopeDigest !== event.boundEnvelopeDigest ||
        existing.resourceVersion !== event.resourceVersion
      ) {
        await this.updateApprovalCard(existing, event);
      }
      return;
    }

    if (!decision || !isTerminalPhase(phase) || !existing) {
      return;
    }

    const decisionKind = normalizeString(
      resource.metadata?.annotations?.["kars.azure.com/review-decision-kind"]
    );
    const verdict: CardVerdict =
      decision.verdict === "approve"
        ? "approve"
        : decisionKind === "request-changes"
          ? "request-changes"
          : "deny";
    await this.updateApprovalDecision(existing, event, verdict, {
      decider:
        normalizeString(decision.decider) ??
        normalizeString(resource.status?.decider) ??
        "Unknown",
      reason: normalizeString(decision.reason),
    });
  }

  private async sendApprovalCard(event: ApprovalEvent): Promise<void> {
    const binding = await this.store.getByTeam(event.team ?? "");
    if (!binding) {
      return;
    }
    const sent = await this.app.send(
      binding.conversationId,
      toActivity(buildApprovalCard(event))
    );
    const record: ApprovalMessageRecord = {
      approvalName: event.name,
      approvalNamespace: event.namespace,
      conversationId: binding.conversationId,
      teamName: binding.teamName,
      messageId: normalizeString(sent?.id) ?? "",
      resourceVersion: event.resourceVersion,
      boundEnvelopeDigest: event.boundEnvelopeDigest ?? "",
      sentAt: new Date().toISOString(),
    };
    await this.store.recordApprovalMessage(record);
  }

  private async updateApprovalCard(
    record: ApprovalMessageRecord,
    event: ApprovalEvent
  ): Promise<void> {
    if (!record.messageId || !this.app.api?.conversations) {
      await this.store.recordApprovalMessage({
        ...record,
        resourceVersion: event.resourceVersion,
        boundEnvelopeDigest: event.boundEnvelopeDigest ?? record.boundEnvelopeDigest,
        sentAt: new Date().toISOString(),
      });
      return;
    }
    await this.app.api.conversations.updateActivity(
      record.conversationId,
      record.messageId,
      toActivity(buildApprovalCard(event))
    );
    await this.store.recordApprovalMessage({
      ...record,
      resourceVersion: event.resourceVersion,
      boundEnvelopeDigest: event.boundEnvelopeDigest ?? record.boundEnvelopeDigest,
      sentAt: new Date().toISOString(),
    });
  }

  private async updateApprovalDecision(
    record: ApprovalMessageRecord,
    event: ApprovalEvent,
    verdict: CardVerdict,
    decision: {
      readonly decider: string;
      readonly reason?: string | undefined;
    }
  ): Promise<void> {
    if (!record.messageId || !this.app.api?.conversations) {
      return;
    }
    await this.app.api.conversations.updateActivity(
      record.conversationId,
      record.messageId,
      toActivity(
        buildDecidedCard(event, verdict, decision.decider, decision.reason)
      )
    );
    await this.store.recordApprovalMessage({
      ...record,
      resourceVersion: event.resourceVersion,
      boundEnvelopeDigest:
        event.boundEnvelopeDigest ?? record.boundEnvelopeDigest,
      sentAt: new Date().toISOString(),
    });
  }

  private async handleTaskProgress(
    resource: TaskResource,
    isInitialAdd: boolean
  ): Promise<void> {
    const name = normalizeString(resource.metadata?.name);
    const namespace = normalizeString(resource.metadata?.namespace);
    if (!name || !namespace) {
      return;
    }
    const key = `${namespace}/${name}`;
    const next = taskSignature(resource);
    const previous = this.taskState.get(key);
    if (isInitialAdd) {
      this.taskState.set(key, next);
      return;
    }
    if (previous === next) {
      return;
    }
    const progress = toTaskProgressEvent(resource);
    if (progress && !(await this.sendProgressCard(progress))) {
      return;
    }
    this.taskState.set(key, next);
  }

  private async handleTeamProgress(
    resource: TeamResource,
    isInitialAdd: boolean
  ): Promise<void> {
    const name = normalizeString(resource.metadata?.name);
    const namespace = normalizeString(resource.metadata?.namespace);
    if (!name || !namespace) {
      return;
    }
    const key = `${namespace}/${name}`;
    const next = teamSignature(resource);
    const previous = this.teamState.get(key);
    if (isInitialAdd) {
      this.teamState.set(key, next);
      return;
    }
    if (previous === next) {
      return;
    }
    const progress = toTeamProgressEvent(resource);
    if (progress && !(await this.sendProgressCard(progress))) {
      return;
    }
    this.teamState.set(key, next);
  }

  private async sendProgressCard(event: ProgressEvent): Promise<boolean> {
    const binding = await this.store.getByTeam(event.teamName);
    if (!binding) {
      return false;
    }
    await this.app.send(
      binding.conversationId,
      toActivity(buildProgressCard(event))
    );
    return true;
  }

  private async sleep(ms: number): Promise<void> {
    await new Promise<void>((resolve) => setTimeout(resolve, ms));
  }
}

export { GatewayWatcher as ApprovalWatcher };
