export function safeReturnTo(value: string | null | undefined, fallback = "/workspace"): string {
  const candidate = value?.trim();
  if (!candidate || !candidate.startsWith("/") || candidate.startsWith("//") || candidate.includes("\\")) {
    return fallback;
  }
  try {
    const parsed = new URL(candidate, "http://bridge.local");
    if (parsed.origin !== "http://bridge.local") return fallback;
    return `${parsed.pathname}${parsed.search}${parsed.hash}`;
  } catch {
    return fallback;
  }
}

export function loginPath(returnTo: string): string {
  return `/auth/login?returnTo=${encodeURIComponent(safeReturnTo(returnTo))}`;
}
