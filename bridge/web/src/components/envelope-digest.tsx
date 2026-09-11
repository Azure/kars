// kars Bridge web — render an envelope digest as a verifiable, copyable
// monospace chip. The digest is the value a Governance Receipt binds to, so
// it is presented as evidence, not decoration.

export function EnvelopeDigest({ digest }: { digest: string | null }) {
  if (!digest) {
    return <span className="text-foreground-muted">digest pending…</span>;
  }
  const short = digest.replace(/^sha256:/, "").slice(0, 12);
  return (
    <span
      title={digest}
      className="inline-flex items-center gap-1 font-mono text-xs text-foreground-muted"
    >
      <svg
        aria-hidden
        viewBox="0 0 16 16"
        className="h-3 w-3 text-signal"
        fill="currentColor"
      >
        <path d="M8 1a3 3 0 0 0-3 3v2H4a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1V7a1 1 0 0 0-1-1h-1V4a3 3 0 0 0-3-3Zm-1 5V4a1 1 0 1 1 2 0v2H7Z" />
      </svg>
      sha256:{short}…
    </span>
  );
}
