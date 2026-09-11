// GatewayWatcher wire and collaborator types; no runtime initialization.

export interface Metadata {
  readonly name?: string | undefined;
  readonly namespace?: string | undefined;
  readonly resourceVersion?: string | undefined;
  readonly labels?: Readonly<Record<string, string>> | undefined;
  readonly annotations?: Readonly<Record<string, string>> | undefined;
}

export interface ApprovalDecisionResource {
  readonly verdict?: string | undefined;
  readonly decider?: string | undefined;
  readonly reason?: string | undefined;
}

export interface ApprovalResource {
  readonly metadata?: Metadata | undefined;
  readonly spec?: {
    readonly taskRef?: { readonly name?: string | undefined } | undefined;
    readonly action?: {
      readonly kind?: string | undefined;
      readonly summary?: string | undefined;
      readonly detail?: string | undefined;
      readonly requestedTier?: number | undefined;
    } | undefined;
    readonly decision?: ApprovalDecisionResource | undefined;
  } | undefined;
  readonly status?: {
    readonly phase?: string | undefined;
    readonly boundEnvelopeDigest?: string | undefined;
    readonly decider?: string | undefined;
    readonly decidedAt?: string | undefined;
  } | undefined;
}

export interface TaskResource {
  readonly metadata?: Metadata | undefined;
  readonly spec?: {
    readonly objective?: string | undefined;
    readonly displayName?: string | undefined;
  } | undefined;
  readonly status?: {
    readonly phase?: string | undefined;
    readonly executionPhase?: string | undefined;
    readonly executionDetail?: string | undefined;
    readonly assignmentSequence?: number | undefined;
    readonly assignment?: {
      readonly state?: string | undefined;
      readonly stage?: string | undefined;
      readonly error?: string | undefined;
    } | undefined;
  } | undefined;
}

export interface TeamResource {
  readonly metadata?: Metadata | undefined;
  readonly spec?: {
    readonly displayName?: string | undefined;
    readonly paused?: boolean | undefined;
    readonly charter?: string | undefined;
  } | undefined;
  readonly status?: {
    readonly phase?: string | undefined;
    readonly health?: string | undefined;
    readonly detail?: string | undefined;
    readonly runtimeState?: string | undefined;
    readonly generatedTaskCount?: number | undefined;
    readonly lastGeneratedTask?: string | undefined;
    readonly currentAssignmentTask?: string | undefined;
  } | undefined;
}

export interface KubernetesList<T> {
  readonly items?: readonly T[] | undefined;
  readonly metadata?: {
    readonly resourceVersion?: string | undefined;
  } | undefined;
}

export interface CustomObjectsApiLike {
  listNamespacedCustomObject(
    group: string,
    version: string,
    namespace: string,
    plural: string,
    pretty?: string,
    allowWatchBookmarks?: boolean,
    _continue?: string,
    fieldSelector?: string,
    labelSelector?: string,
    limit?: number,
    resourceVersion?: string,
    resourceVersionMatch?: string,
    timeoutSeconds?: number,
    watch?: boolean
  ): Promise<unknown>;
}

export interface WatchLike {
  watch(
    path: string,
    queryParams: Record<string, string | number | boolean | undefined>,
    callback: (phase: string, apiObj: unknown, watchObj?: unknown) => void,
    done: (err: unknown) => void
  ): Promise<AbortController>;
}

export interface TeamsMessenger {
  readonly api?: {
    readonly conversations?: {
      updateActivity(
        conversationId: string,
        activityId: string,
        activity: TeamsActivity
      ): Promise<unknown>;
    } | undefined;
  } | undefined;
  send(
    conversationId: string,
    activity: TeamsActivity
  ): Promise<{ id?: string | undefined } | null>;
}

export interface TeamsActivity {
  readonly type: "message";
  readonly attachments: readonly {
    readonly contentType: "application/vnd.microsoft.card.adaptive";
    readonly content: object;
  }[];
}
