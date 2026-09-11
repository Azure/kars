import {
  CoreV1Api,
  KubeConfig,
  type V1ConfigMap,
} from "@kubernetes/client-node";
import { log } from "./log.js";

export interface ConversationBinding {
  readonly conversationId: string;
  readonly serviceUrl: string;
  readonly tenantId: string;
  readonly teamName: string;
  readonly namespace: string;
  readonly boundAt: string;
  readonly rootMessageId?: string | undefined;
}

export interface ApprovalMessageRecord {
  readonly approvalName: string;
  readonly approvalNamespace: string;
  readonly conversationId: string;
  readonly teamName: string;
  readonly messageId: string;
  readonly resourceVersion: string;
  readonly boundEnvelopeDigest: string;
  readonly sentAt: string;
}

export interface ConversationStore {
  getByTeam(teamName: string): Promise<ConversationBinding | undefined>;
  getByConversation(
    conversationId: string
  ): Promise<ConversationBinding | undefined>;
  bind(binding: ConversationBinding): Promise<void>;
  listBindings(): Promise<readonly ConversationBinding[]>;
  getApprovalMessage(
    approvalNamespace: string,
    approvalName: string
  ): Promise<ApprovalMessageRecord | undefined>;
  recordApprovalMessage(record: ApprovalMessageRecord): Promise<void>;
  getLastResourceVersion(stream: string): Promise<string | undefined>;
  setLastResourceVersion(stream: string, value: string): Promise<void>;
}

interface CoreV1ApiLike {
  readNamespacedConfigMap(
    name: string,
    namespace: string
  ): Promise<unknown>;
  createNamespacedConfigMap(
    namespace: string,
    body: V1ConfigMap
  ): Promise<unknown>;
  replaceNamespacedConfigMap(
    name: string,
    namespace: string,
    body: V1ConfigMap
  ): Promise<unknown>;
}

export interface KubernetesConversationStoreOptions {
  readonly namespace: string;
  readonly configMapName: string;
  readonly api?: CoreV1ApiLike | undefined;
  readonly kubeConfig?: KubeConfig | undefined;
}

interface PersistedStore {
  readonly bindings: readonly ConversationBinding[];
  readonly approvalMessages: readonly ApprovalMessageRecord[];
  readonly resourceVersions: Readonly<Record<string, string>>;
}

const BINDINGS_KEY = "bindings.json";
const APPROVAL_MESSAGES_KEY = "approval-messages.json";
const RESOURCE_VERSIONS_KEY = "resource-versions.json";

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

function asObject(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null
    ? (value as Record<string, unknown>)
    : undefined;
}

function normalizeBinding(value: unknown): ConversationBinding | undefined {
  const object = asObject(value);
  if (!object) {
    return undefined;
  }
  const conversationId = normalizeString(object.conversationId);
  const serviceUrl = normalizeString(object.serviceUrl);
  const tenantId = normalizeString(object.tenantId);
  const teamName = (object.teamName as string | undefined)?.trim() ?? "";
  const namespace = (object.namespace as string | undefined)?.trim() ?? "";
  const boundAt = normalizeString(object.boundAt);
  if (!conversationId || !serviceUrl || !tenantId || !boundAt) {
    return undefined;
  }
  const rootMessageId = normalizeString(object.rootMessageId);
  return {
    conversationId,
    serviceUrl,
    tenantId,
    teamName,
    namespace,
    boundAt,
    ...(rootMessageId !== undefined ? { rootMessageId } : {}),
  };
}

function normalizeApprovalRecord(
  value: unknown
): ApprovalMessageRecord | undefined {
  const object = asObject(value);
  if (!object) {
    return undefined;
  }
  const approvalName = normalizeString(object.approvalName);
  const approvalNamespace = normalizeString(object.approvalNamespace);
  const conversationId = normalizeString(object.conversationId);
  const teamName = normalizeString(object.teamName);
  const messageId = typeof object.messageId === "string" ? object.messageId : undefined;
  const resourceVersion = normalizeString(object.resourceVersion);
  const boundEnvelopeDigest = normalizeString(object.boundEnvelopeDigest);
  const sentAt = normalizeString(object.sentAt);
  if (
    !approvalName ||
    !approvalNamespace ||
    !conversationId ||
    !teamName ||
    messageId === undefined ||
    !resourceVersion ||
    !boundEnvelopeDigest ||
    !sentAt
  ) {
    return undefined;
  }
  return {
    approvalName,
    approvalNamespace,
    conversationId,
    teamName,
    messageId,
    resourceVersion,
    boundEnvelopeDigest,
    sentAt,
  };
}

export function approvalMessageKey(
  approvalNamespace: string,
  approvalName: string
): string {
  return `${approvalNamespace}/${approvalName}`;
}

export class InMemoryConversationStore implements ConversationStore {
  private readonly byConversation = new Map<string, ConversationBinding>();
  private readonly byTeam = new Map<string, ConversationBinding>();
  private readonly approvalMessages = new Map<string, ApprovalMessageRecord>();
  private readonly resourceVersions = new Map<string, string>();

  public async getByTeam(
    teamName: string
  ): Promise<ConversationBinding | undefined> {
    return this.byTeam.get(teamName.trim());
  }

  public async getByConversation(
    conversationId: string
  ): Promise<ConversationBinding | undefined> {
    return this.byConversation.get(conversationId.trim());
  }

  public async bind(binding: ConversationBinding): Promise<void> {
    const normalized = normalizeBinding(binding);
    if (!normalized) {
      throw new Error("invalid conversation binding");
    }
    const previous = this.byConversation.get(normalized.conversationId);
    if (previous?.teamName && previous.teamName !== normalized.teamName) {
      this.byTeam.delete(previous.teamName);
    }
    this.byConversation.set(normalized.conversationId, normalized);
    if (normalized.teamName) {
      this.byTeam.set(normalized.teamName, normalized);
    }
    log("info", "stored Teams conversation binding", {
      conversationId: normalized.conversationId,
      teamName: normalized.teamName || "(unbound)",
      namespace: normalized.namespace || "(default)",
    });
  }

  public async listBindings(): Promise<readonly ConversationBinding[]> {
    return [...this.byConversation.values()];
  }

  public async getApprovalMessage(
    approvalNamespace: string,
    approvalName: string
  ): Promise<ApprovalMessageRecord | undefined> {
    return this.approvalMessages.get(
      approvalMessageKey(approvalNamespace, approvalName)
    );
  }

  public async recordApprovalMessage(
    record: ApprovalMessageRecord
  ): Promise<void> {
    this.approvalMessages.set(
      approvalMessageKey(record.approvalNamespace, record.approvalName),
      record
    );
  }

  public async getLastResourceVersion(
    stream: string
  ): Promise<string | undefined> {
    return this.resourceVersions.get(stream);
  }

  public async setLastResourceVersion(
    stream: string,
    value: string
  ): Promise<void> {
    const normalizedStream = stream.trim();
    const normalizedValue = value.trim();
    if (!normalizedStream || !normalizedValue) {
      return;
    }
    this.resourceVersions.set(normalizedStream, normalizedValue);
  }
}

export class KubernetesConversationStore implements ConversationStore {
  private readonly namespace: string;
  private readonly configMapName: string;
  private readonly api: CoreV1ApiLike;
  private readonly byConversation = new Map<string, ConversationBinding>();
  private readonly byTeam = new Map<string, ConversationBinding>();
  private readonly approvalMessages = new Map<string, ApprovalMessageRecord>();
  private readonly resourceVersions = new Map<string, string>();
  private persistTail: Promise<void> = Promise.resolve();

  public constructor(options: KubernetesConversationStoreOptions) {
    this.namespace = options.namespace;
    this.configMapName = options.configMapName;
    if (options.api) {
      this.api = options.api;
    } else {
      const kubeConfig = options.kubeConfig ?? new KubeConfig();
      if (!options.kubeConfig) {
        kubeConfig.loadFromCluster();
      }
      this.api = kubeConfig.makeApiClient(CoreV1Api) as unknown as CoreV1ApiLike;
    }
  }

  public async initialize(): Promise<void> {
    const configMap = await this.readOrCreateConfigMap();
    this.loadFromData(configMap.data ?? {});
  }

  public async getByTeam(
    teamName: string
  ): Promise<ConversationBinding | undefined> {
    return this.byTeam.get(teamName.trim());
  }

  public async getByConversation(
    conversationId: string
  ): Promise<ConversationBinding | undefined> {
    return this.byConversation.get(conversationId.trim());
  }

  public async bind(binding: ConversationBinding): Promise<void> {
    const normalized = normalizeBinding(binding);
    if (!normalized) {
      throw new Error("invalid conversation binding");
    }
    const previous = this.byConversation.get(normalized.conversationId);
    if (previous?.teamName && previous.teamName !== normalized.teamName) {
      this.byTeam.delete(previous.teamName);
    }
    this.byConversation.set(normalized.conversationId, normalized);
    if (normalized.teamName) {
      this.byTeam.set(normalized.teamName, normalized);
    }
    await this.persist();
  }

  public async listBindings(): Promise<readonly ConversationBinding[]> {
    return [...this.byConversation.values()];
  }

  public async getApprovalMessage(
    approvalNamespace: string,
    approvalName: string
  ): Promise<ApprovalMessageRecord | undefined> {
    return this.approvalMessages.get(
      approvalMessageKey(approvalNamespace, approvalName)
    );
  }

  public async recordApprovalMessage(
    record: ApprovalMessageRecord
  ): Promise<void> {
    this.approvalMessages.set(
      approvalMessageKey(record.approvalNamespace, record.approvalName),
      record
    );
    await this.persist();
  }

  public async getLastResourceVersion(
    stream: string
  ): Promise<string | undefined> {
    return this.resourceVersions.get(stream.trim());
  }

  public async setLastResourceVersion(
    stream: string,
    value: string
  ): Promise<void> {
    const normalizedStream = stream.trim();
    const normalizedValue = value.trim();
    if (!normalizedStream || !normalizedValue) {
      return;
    }
    this.resourceVersions.set(normalizedStream, normalizedValue);
    await this.persist();
  }

  private async readOrCreateConfigMap(): Promise<V1ConfigMap> {
    const existing = await this.readConfigMap();
    if (existing) {
      return existing;
    }
    return this.createConfigMap();
  }

  private async readConfigMap(): Promise<V1ConfigMap | undefined> {
    try {
      return unwrapResponse<V1ConfigMap>(
        await this.api.readNamespacedConfigMap(
          this.configMapName,
          this.namespace
        )
      );
    } catch (error) {
      if (statusCodeOf(error) === 404) {
        return undefined;
      }
      throw error;
    }
  }

  private async createConfigMap(): Promise<V1ConfigMap> {
    const configMap: V1ConfigMap = {
      apiVersion: "v1",
      kind: "ConfigMap",
      metadata: {
        name: this.configMapName,
        namespace: this.namespace,
      },
      data: this.serialize(),
    };
    try {
      return unwrapResponse<V1ConfigMap>(
        await this.api.createNamespacedConfigMap(this.namespace, configMap)
      );
    } catch (error) {
      if (statusCodeOf(error) === 409) {
        const existing = await this.readConfigMap();
        if (existing) {
          return existing;
        }
      }
      throw error;
    }
  }

  private async persist(): Promise<void> {
    const operation = this.persistTail.then(() => this.persistOnce());
    this.persistTail = operation.catch(() => undefined);
    return operation;
  }

  private async persistOnce(): Promise<void> {
    const current = await this.readOrCreateConfigMap();
    const resourceVersion = normalizeString(current.metadata?.resourceVersion);
    const body: V1ConfigMap = {
      apiVersion: "v1",
      kind: "ConfigMap",
      metadata: {
        ...current.metadata,
        name: this.configMapName,
        namespace: this.namespace,
        ...(resourceVersion !== undefined ? { resourceVersion } : {}),
      },
      data: this.serialize(),
    };
    await this.api.replaceNamespacedConfigMap(
      this.configMapName,
      this.namespace,
      body
    );
  }

  private serialize(): Record<string, string> {
    const data: PersistedStore = {
      bindings: [...this.byConversation.values()],
      approvalMessages: [...this.approvalMessages.values()],
      resourceVersions: Object.fromEntries(this.resourceVersions),
    };
    return {
      [BINDINGS_KEY]: JSON.stringify(data.bindings),
      [APPROVAL_MESSAGES_KEY]: JSON.stringify(data.approvalMessages),
      [RESOURCE_VERSIONS_KEY]: JSON.stringify(data.resourceVersions),
    };
  }

  private loadFromData(data: Record<string, string>): void {
    this.byConversation.clear();
    this.byTeam.clear();
    this.approvalMessages.clear();
    this.resourceVersions.clear();

    const rawBindings = data[BINDINGS_KEY];
    if (rawBindings) {
      try {
        const parsed = JSON.parse(rawBindings) as unknown[];
        for (const entry of parsed) {
          const binding = normalizeBinding(entry);
          if (!binding) {
            continue;
          }
          this.byConversation.set(binding.conversationId, binding);
          if (binding.teamName) {
            this.byTeam.set(binding.teamName, binding);
          }
        }
      } catch {
        log("warn", "failed to parse persisted Teams conversation bindings");
      }
    }

    const rawApprovalMessages = data[APPROVAL_MESSAGES_KEY];
    if (rawApprovalMessages) {
      try {
        const parsed = JSON.parse(rawApprovalMessages) as unknown[];
        for (const entry of parsed) {
          const record = normalizeApprovalRecord(entry);
          if (!record) {
            continue;
          }
          this.approvalMessages.set(
            approvalMessageKey(
              record.approvalNamespace,
              record.approvalName
            ),
            record
          );
        }
      } catch {
        log("warn", "failed to parse persisted approval message records");
      }
    }

    const rawResourceVersions = data[RESOURCE_VERSIONS_KEY];
    if (rawResourceVersions) {
      try {
        const parsed = JSON.parse(rawResourceVersions) as Record<string, unknown>;
        for (const [key, value] of Object.entries(parsed)) {
          const normalizedValue = normalizeString(value);
          if (normalizedValue) {
            this.resourceVersions.set(key, normalizedValue);
          }
        }
      } catch {
        log("warn", "failed to parse persisted watch resource versions");
      }
    }
  }
}
