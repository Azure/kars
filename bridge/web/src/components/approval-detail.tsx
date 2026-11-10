// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export function ApprovalDetail({ actionKind, detail }: { actionKind: string; detail?: string | null }) {
  if (!detail) return null;
  let request: unknown;
  try { request = JSON.parse(detail); } catch { /* Ordinary human-written details are not JSON. */ }
  if (actionKind !== "egress" || !request || typeof request !== "object" || Array.isArray(request)
    || !("source" in request) || request.source !== "router-access-request") {
    return <p className="mt-0.5 whitespace-pre-wrap break-words text-xs text-foreground-muted">{detail}</p>;
  }
  const data = request as Record<string, unknown>;
  const valid = data.kind === "egress" && typeof data.target === "string" && data.target.trim().length > 0
    && Number.isInteger(data.port) && (data.port as number) > 0 && (data.port as number) <= 65535
    && typeof data.reason === "string" && data.reason.trim().length > 0;
  const expires = typeof data.expiresAtUnix === "number" && Number.isSafeInteger(data.expiresAtUnix)
    && data.expiresAtUnix > 0 ? new Date(data.expiresAtUnix * 1000) : null;
  const expiry = expires && Number.isFinite(expires.getTime()) ? expires.toISOString() : null;
  return (
    <div className="mt-2 space-y-2 text-xs text-foreground-muted">
      {valid ? (
        <>
          <dl className="space-y-1">
            <div><dt className="inline font-medium">Destination: </dt><dd className="inline break-all">{String(data.target)} · port {String(data.port)}</dd></div>
            <div><dt className="inline font-medium">Why it is asking: </dt><dd className="inline break-words">{data.reason === "Egress policy blocked this destination"
              ? "This work tried to contact a destination that is not currently allowed."
              : String(data.reason)}</dd></div>
            <div><dt className="inline font-medium">Request expires: </dt><dd className="inline">{expiry
              ? <time dateTime={expiry}>{expiry.replace("T", " ").replace(".000Z", " UTC")}</time>
              : "Not recorded"}</dd></div>
          </dl>
          <p>Approve only if this destination is needed for the work. Kars will recheck whether the request is still valid; other access rules still apply. This is not permission for every future run.</p>
        </>
      ) : (
        <p>The access-request details are incomplete. Review the technical record or ask the person who manages Kars before deciding.</p>
      )}
      <details>
        <summary className="cursor-pointer font-medium focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal">Technical details</summary>
        <pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap break-all rounded-lg border border-border bg-surface-muted p-3">{detail}</pre>
      </details>
    </div>
  );
}
