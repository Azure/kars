export type CredentialKind = "KarsSandbox" | "KarsTask" | "KarsTeam";
export interface CredentialInput {
  kind: CredentialKind;
  namespace: string;
  target: string;
  targetUid?: string;
  key: string;
}
export interface StoredCredentialSource {
  name: string; uid: string; version: string; metadataDigest: string;
}
export interface CredentialContinuation {
  token: string;
  outcome: "source-stored" | "no-write-attempted";
  source: StoredCredentialSource | null;
}
export interface CredentialReview {
  token: string;
  expiresAt: number;
  submission: number;
  continuation: boolean;
  bindingOnly: boolean;
  metadata: {
    target: { kind: CredentialKind; namespace: string; name: string; uid: string | null;
      generation: number | null; version: string | null; intent: string | null };
    grant: { uid: string; generation: number; version: string; intent: string;
      workspaceUid: string; legacyInventory: string };
    source: { name: string; uid: string | null; version: string | null; metadataDigest: string | null; keys: string[] };
    key: string;
  };
}
export interface CredentialFormState {
  error: string | null;
  ok: string | null;
  review: CredentialReview | null;
  pending: { receipt: CredentialContinuation; input: CredentialInput; origin: CredentialReview } | null;
}
export interface CredentialFailure {
  status: number; code: string; message: string; continuation?: CredentialContinuation;
}

const object = (value: unknown): Record<string, unknown> | null =>
  value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
const text = (value: unknown): value is string => typeof value === "string" && value.length > 0 && value.length <= 256;
const hash = (value: unknown): value is string => typeof value === "string" && /^sha256:[a-f0-9]{64}$/.test(value);
const token = (value: unknown): value is string =>
  typeof value === "string" && value.length <= 32768 && /^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/.test(value);
const kind = (value: unknown): value is CredentialKind =>
  value === "KarsSandbox" || value === "KarsTask" || value === "KarsTeam";
const generation = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value > 0;

export function parseCredentialContinuation(value: unknown): CredentialContinuation | undefined {
  const record = object(value), source = object(record?.source);
  if (!record || !token(record.token)) return undefined;
  if (record.outcome === "no-write-attempted" && record.source === null)
    return { token: record.token, outcome: "no-write-attempted", source: null };
  if (record.outcome !== "source-stored" || !source || !text(source.name) || !text(source.uid)
    || !text(source.version) || !hash(source.metadataDigest)) return undefined;
  return { token: record.token, outcome: "source-stored", source: { name: source.name, uid: source.uid,
    version: source.version, metadataDigest: source.metadataDigest } };
}

export function parseCredentialReview(value: unknown): CredentialReview | undefined {
  const record = object(value), metadata = object(record?.metadata);
  const target = object(metadata?.target), grant = object(metadata?.grant), source = object(metadata?.source);
  if (!record || !metadata || !target || !grant || !source || !token(record.token)
    || typeof record.expiresAt !== "number" || !Number.isSafeInteger(record.expiresAt)
    || !generation(record.submission) || record.submission > 3 || typeof record.continuation !== "boolean"
    || typeof record.bindingOnly !== "boolean"
    || !kind(target.kind) || !text(target.namespace) || !text(target.name)
    || !(target.uid === null ? target.generation === null && target.version === null && target.intent === null
      : text(target.uid) && generation(target.generation) && text(target.version) && hash(target.intent))
    || !text(grant.uid) || !generation(grant.generation) || !text(grant.version) || !hash(grant.intent)
    || !text(grant.workspaceUid) || !hash(grant.legacyInventory) || !text(source.name)
    || !(source.uid === null ? source.version === null && source.metadataDigest === null
      : text(source.uid) && text(source.version) && hash(source.metadataDigest))
    || !Array.isArray(source.keys) || !source.keys.every(text) || !text(metadata.key)) return undefined;
  return {
    token: record.token, expiresAt: record.expiresAt, submission: record.submission,
    continuation: record.continuation, bindingOnly: record.bindingOnly,
    metadata: {
      target: { kind: target.kind, namespace: target.namespace, name: target.name,
        uid: target.uid as string | null, generation: target.generation as number | null,
        version: target.version as string | null, intent: target.intent as string | null },
      grant: { uid: grant.uid, generation: grant.generation, version: grant.version, intent: grant.intent,
        workspaceUid: grant.workspaceUid, legacyInventory: grant.legacyInventory },
      source: { name: source.name, uid: source.uid as string | null, version: source.version as string | null,
        metadataDigest: source.metadataDigest as string | null, keys: [...source.keys] },
      key: metadata.key,
    },
  };
}

export function credentialReviewMatches(review: CredentialReview, input: CredentialInput): boolean {
  const target = review.metadata.target;
  return target.kind === input.kind && target.namespace === input.namespace && target.name === input.target
    && review.metadata.key === input.key && (!input.targetUid || target.uid === input.targetUid);
}

function parseInput(value: unknown): CredentialInput | undefined {
  const input = object(value);
  if (!input) return undefined;
  const { target, namespace, targetUid, key, kind: targetKind } = input;
  const dns = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/;
  if (!kind(targetKind) || typeof namespace !== "string" || typeof target !== "string"
    || typeof key !== "string" || !dns.test(namespace) || !dns.test(target)
    || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(key) || (targetUid !== undefined && !text(targetUid))) return undefined;
  return { kind: targetKind, namespace, target, ...(targetUid ? { targetUid } : {}), key };
}

export function credentialContinuationMatches(origin: CredentialReview, current: CredentialReview,
  receipt: CredentialContinuation): boolean {
  const target = (review: CredentialReview) => ({ ...review.metadata.target, version: null });
  const grant = (review: CredentialReview) => ({ ...review.metadata.grant, version: null });
  const keys = [...new Set([...origin.metadata.source.keys, origin.metadata.key])].sort();
  const sameAuthority = current.continuation && current.submission === origin.submission + 1 && current.expiresAt === origin.expiresAt
    && JSON.stringify(target(origin)) === JSON.stringify(target(current))
    && JSON.stringify(grant(origin)) === JSON.stringify(grant(current))
    && current.metadata.key === origin.metadata.key;
  if (!sameAuthority) return false;
  if (!receipt.source)
    return receipt.outcome === "no-write-attempted" && !current.bindingOnly
      && JSON.stringify(origin.metadata.source) === JSON.stringify(current.metadata.source);
  return receipt.outcome === "source-stored" && current.bindingOnly
    && current.metadata.source.name === receipt.source.name && current.metadata.source.uid === receipt.source.uid
    && current.metadata.source.version === receipt.source.version
    && current.metadata.source.metadataDigest === receipt.source.metadataDigest
    && JSON.stringify([...current.metadata.source.keys].sort()) === JSON.stringify(keys);
}

export async function credentialFormTransition(
  previous: CredentialFormState,
  form: FormData,
  api: {
    review: (input: CredentialInput & { continuation?: string }) => Promise<CredentialReview>;
    write: (input: CredentialInput & { value: string; review: string }) => Promise<{ stored: boolean; note: string }>;
    failure: (error: unknown) => CredentialFailure | undefined;
    now?: () => number;
  },
): Promise<CredentialFormState> {
  const empty: CredentialFormState = { error: null, ok: null, review: null, pending: null };
  const pendingRecord = object(previous?.pending);
  const receipt = parseCredentialContinuation(pendingRecord?.receipt);
  const pendingInput = parseInput(pendingRecord?.input);
  const origin = parseCredentialReview(pendingRecord?.origin);
  previous = { ...empty, review: parseCredentialReview(previous?.review) ?? null,
    pending: receipt && pendingInput && origin ? { receipt, input: pendingInput, origin } : null };
  const operation = form.get("operation");
  if (operation === "reset") return empty;
  const targetUid = String(form.get("targetUid") ?? "").trim();
  const input = parseInput({ target: String(form.get("target") ?? "").trim().toLowerCase(),
    namespace: String(form.get("namespace") ?? "").trim(), key: String(form.get("key") ?? "").trim(),
    kind: form.get("kind"), ...(targetUid ? { targetUid } : {}) });
  if (!input) return { ...previous, ok: null, error: "Select a valid workspace, target kind/name and credential key." };
  try {
    if (operation === "review") {
      const pending = previous.pending;
      if (pending && (pending.input.namespace !== input.namespace || pending.input.kind !== input.kind
        || pending.input.target !== input.target || pending.input.key !== input.key
        || (input.targetUid && input.targetUid !== pending.input.targetUid))) {
        return { ...previous, error: "Restore the original target and key, or explicitly start a new change.", ok: null };
      }
      const reviewed = parseCredentialReview(await api.review({ ...input,
        ...(pending ? { targetUid: pending.input.targetUid, continuation: pending.receipt.token } : {}) }));
      if (!reviewed || !credentialReviewMatches(reviewed, input)
        || (pending ? !credentialContinuationMatches(pending.origin, reviewed, pending.receipt)
          : reviewed.continuation || reviewed.bindingOnly || reviewed.submission !== 1)) {
        return { ...previous, review: null, ok: null, error: "Credential review did not match the selected authority." };
      }
      return { ...empty, review: reviewed };
    }
    const reviewed = previous.review;
    if (operation !== "store" || previous.pending || !reviewed || !credentialReviewMatches(reviewed, input)
      || reviewed.expiresAt <= (api.now?.() ?? Math.floor(Date.now() / 1000)) || form.get("confirmed") !== "on") {
      return { ...previous, ok: null, error: "Refresh and explicitly confirm the current metadata review before storing." };
    }
    const value = String(form.get("value") ?? "");
    if (!value) return { ...previous, ok: null, error: "Enter the credential value; it is never read back." };
    const result = await api.write({ ...input, targetUid: reviewed.metadata.target.uid ?? undefined,
      value, review: reviewed.token });
    if (!result.stored) return { ...empty, error: "Credential storage was not confirmed." };
    return { ...empty, ok: result.note };
  } catch (error) {
    const failure = api.failure(error);
    const receipt = parseCredentialContinuation(failure?.continuation);
    if (operation === "store" && failure?.status === 409 && failure.code === "conflict" && receipt
      && previous.review) {
      return { ...empty, error: receipt.source
        ? "The source was stored but binding conflicted. Explicitly refresh/review its acknowledgement, then re-enter the same value to resume."
        : "The review changed before any source write was attempted. Explicitly refresh/review the same authority before resubmitting.",
        pending: { receipt, origin: previous.review, input: { ...input,
          targetUid: previous.review.metadata.target.uid ?? undefined } } };
    }
    return { ...empty, ...(operation === "review" ? { pending: previous.pending } : {}),
      error: failure?.message || "Credential operation failed; no automatic resubmission was attempted." };
  }
}
