"use client";

// kars Bridge — copyable digest block.
//
// The envelope digest is cryptographic evidence — it must read as such:
// full-width monospace, never truncated mid-hash, with one-click copy. This is
// the value an auditor reconciles against the Governance Receipt, so it gets
// first-class, precise treatment.

import { useState } from "react";

export function CopyDigest({
  digest,
  label = "Copy digest",
}: {
  digest: string | null;
  label?: string;
}) {
  const [copied, setCopied] = useState(false);

  if (!digest) {
    return (
      <span className="text-sm text-foreground-muted italic">pending…</span>
    );
  }

  async function copy() {
    try {
      await navigator.clipboard.writeText(digest!);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard unavailable (e.g. insecure context) — no-op; the value is
      // still fully visible and selectable.
    }
  }

  return (
    <div className="flex items-stretch overflow-hidden rounded-lg border border-border bg-surface-muted">
      <code className="min-w-0 flex-1 break-all px-3 py-2 font-mono text-xs leading-relaxed">
        {digest}
      </code>
      <button
        type="button"
        onClick={copy}
        aria-label={label}
        className="shrink-0 border-l border-border px-3 text-xs font-medium text-foreground-muted hover:bg-surface hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
      >
        {copied ? "Copied" : "Copy"}
      </button>
    </div>
  );
}
