// Liveness/readiness endpoint. Intentionally does NO upstream (BFF) work so the
// kubelet probe reflects "this web server can accept traffic", not "the BFF is
// reachable" — the SSR pages (e.g. /workspace) do a BFF round-trip and are far
// too heavy for a 1s readiness probe.
export const dynamic = "force-dynamic";

export function GET() {
  return new Response("ok", {
    status: 200,
    headers: { "content-type": "text/plain", "cache-control": "no-store" },
  });
}
