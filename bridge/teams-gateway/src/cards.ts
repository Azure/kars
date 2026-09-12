import type { IAdaptiveCard } from "@microsoft/teams.cards";

export interface ApprovalEvent {
  readonly name: string;
  readonly namespace: string;
  readonly task: string;
  readonly team?: string | undefined;
  readonly actionKind: string;
  readonly summary: string;
  readonly detail?: string | undefined;
  readonly requestedTier?: number | undefined;
  readonly resourceVersion: string;
  readonly boundEnvelopeDigest?: string | undefined;
}

export type CardVerdict = "approve" | "request-changes" | "deny";

export interface CardActionPayload {
  readonly action: "kars.approval.decision";
  readonly approvalName: string;
  readonly approvalNamespace: string;
  readonly verdict: CardVerdict;
  readonly resourceVersion: string;
  readonly boundEnvelopeDigest?: string | undefined;
  readonly requestChangesReason?: string | undefined;
}

export interface CardFact {
  readonly title: string;
  readonly value: string;
}

export interface ProgressEvent {
  readonly kind: "team" | "task";
  readonly teamName: string;
  readonly resourceName: string;
  readonly title: string;
  readonly status: string;
  readonly summary: string;
  readonly detail?: string | undefined;
  readonly stage?: string | undefined;
  readonly facts?: readonly CardFact[] | undefined;
}

type CardElement = Record<string, unknown>;
type CardAction = Record<string, unknown>;

const ROUTE_ACTION = "kars.approval.decision";

function textBlock(
  text: string,
  options?: {
    readonly weight?: "Bolder" | "Default" | undefined;
    readonly size?: "Medium" | "Default" | "Small" | undefined;
    readonly color?: "Attention" | "Default" | "Good" | "Warning" | undefined;
    readonly isSubtle?: boolean | undefined;
  }
): CardElement {
  return {
    type: "TextBlock",
    text,
    wrap: true,
    ...(options?.weight !== undefined ? { weight: options.weight } : {}),
    ...(options?.size !== undefined ? { size: options.size } : {}),
    ...(options?.color !== undefined ? { color: options.color } : {}),
    ...(options?.isSubtle !== undefined ? { isSubtle: options.isSubtle } : {}),
  };
}

function buildCard(
  body: readonly CardElement[],
  actions?: readonly CardAction[] | undefined
): IAdaptiveCard {
  return {
    type: "AdaptiveCard",
    version: "1.4",
    body: [...body],
    ...(actions && actions.length > 0 ? { actions: [...actions] } : {}),
  } as unknown as IAdaptiveCard;
}

function factSet(facts: readonly CardFact[]): CardElement {
  return {
    type: "FactSet",
    facts: facts.map((fact) => ({ title: fact.title, value: fact.value })),
  };
}

function verdictLabel(verdict: CardVerdict): string {
  switch (verdict) {
    case "approve":
      return "Approved";
    case "request-changes":
      return "Changes Requested";
    case "deny":
      return "Denied";
  }
}

function verdictIcon(verdict: CardVerdict): string {
  switch (verdict) {
    case "approve":
      return "✅";
    case "request-changes":
      return "↩️";
    case "deny":
      return "❌";
  }
}

function progressIcon(kind: ProgressEvent["kind"], status: string): string {
  const normalized = status.toLowerCase();
  if (normalized.includes("ready") || normalized.includes("success")) {
    return "✅";
  }
  if (normalized.includes("degraded") || normalized.includes("error")) {
    return "⚠️";
  }
  if (kind === "team") {
    return "👥";
  }
  return "🚀";
}

function buildApprovalAction(
  title: string,
  verb: string,
  verdict: CardVerdict,
  event: ApprovalEvent,
  style?: "positive" | "destructive" | undefined
): CardAction {
  return {
    type: "Action.Execute",
    title,
    verb,
    ...(style !== undefined ? { style } : {}),
    data: {
      action: ROUTE_ACTION,
      approvalName: event.name,
      approvalNamespace: event.namespace,
      verdict,
      resourceVersion: event.resourceVersion,
      ...(event.boundEnvelopeDigest !== undefined
        ? { boundEnvelopeDigest: event.boundEnvelopeDigest }
        : {}),
    } satisfies CardActionPayload,
  };
}

export function buildApprovalCard(event: ApprovalEvent): IAdaptiveCard {
  const facts: CardFact[] = [
    { title: "Task", value: event.task },
    { title: "Kind", value: event.actionKind },
    { title: "Namespace", value: event.namespace },
  ];
  if (event.team) {
    facts.push({ title: "Team", value: event.team });
  }
  if (event.requestedTier !== undefined) {
    facts.push({ title: "Requested Tier", value: String(event.requestedTier) });
  }

  const body: CardElement[] = [
    textBlock(`🔐 Approval Required: ${event.summary}`, {
      weight: "Bolder",
      size: "Medium",
    }),
    factSet(facts),
  ];
  if (event.detail) {
    body.push(textBlock(event.detail, { size: "Small" }));
  }
  body.push({
    type: "Input.Text",
    id: "requestChangesReason",
    label: "Feedback",
    isMultiline: true,
    placeholder: "Describe the requested changes",
  });

  return buildCard(body, [
    buildApprovalAction("Approve", "kars.approve", "approve", event, "positive"),
    buildApprovalAction(
      "Request Changes",
      "kars.request-changes",
      "request-changes",
      event
    ),
    buildApprovalAction("Deny", "kars.deny", "deny", event, "destructive"),
  ]);
}

export function buildDecidedCard(
  event: ApprovalEvent,
  verdict: CardVerdict,
  deciderName: string,
  reason?: string | undefined
): IAdaptiveCard {
  const facts: CardFact[] = [
    { title: "Task", value: event.task },
    { title: "Decision", value: verdictLabel(verdict) },
    { title: "By", value: deciderName },
  ];
  if (event.team) {
    facts.push({ title: "Team", value: event.team });
  }
  if (reason) {
    facts.push({ title: "Reason", value: reason });
  }

  const body: CardElement[] = [
    textBlock(`${verdictIcon(verdict)} ${verdictLabel(verdict)}: ${event.summary}`, {
      weight: "Bolder",
      size: "Medium",
      color: verdict === "approve" ? "Good" : verdict === "deny" ? "Attention" : "Warning",
    }),
    factSet(facts),
  ];
  if (event.detail) {
    body.push(textBlock(event.detail, { size: "Small", isSubtle: true }));
  }
  return buildCard(body);
}

export function buildProgressCard(event: ProgressEvent): IAdaptiveCard {
  const facts: CardFact[] = [
    { title: "Team", value: event.teamName },
    { title: event.kind === "team" ? "Standing Team" : "Task", value: event.resourceName },
  ];
  if (event.stage) {
    facts.push({ title: "Stage", value: event.stage });
  }
  for (const fact of event.facts ?? []) {
    facts.push(fact);
  }

  const body: CardElement[] = [
    textBlock(`${progressIcon(event.kind, event.status)} ${event.title}`, {
      weight: "Bolder",
      size: "Medium",
    }),
    textBlock(event.status, { color: "Default", weight: "Bolder" }),
    factSet(facts),
    textBlock(event.summary),
  ];
  if (event.detail) {
    body.push(textBlock(event.detail, { size: "Small", isSubtle: true }));
  }
  return buildCard(body);
}

export function isCardActionPayload(data: unknown): data is CardActionPayload {
  if (typeof data !== "object" || data === null) {
    return false;
  }
  const payload = data as Record<string, unknown>;
  if (payload["action"] !== ROUTE_ACTION) {
    return false;
  }
  if (
    typeof payload["approvalName"] !== "string" ||
    payload["approvalName"].trim().length === 0 ||
    typeof payload["approvalNamespace"] !== "string" ||
    payload["approvalNamespace"].trim().length === 0 ||
    typeof payload["resourceVersion"] !== "string" ||
    payload["resourceVersion"].trim().length === 0 ||
    typeof payload["verdict"] !== "string"
  ) {
    return false;
  }
  if (
    payload["verdict"] !== "approve" &&
    payload["verdict"] !== "request-changes" &&
    payload["verdict"] !== "deny"
  ) {
    return false;
  }
  if (
    payload["boundEnvelopeDigest"] !== undefined &&
    typeof payload["boundEnvelopeDigest"] !== "string"
  ) {
    return false;
  }
  if (
    payload["requestChangesReason"] !== undefined &&
    typeof payload["requestChangesReason"] !== "string"
  ) {
    return false;
  }
  return true;
}
