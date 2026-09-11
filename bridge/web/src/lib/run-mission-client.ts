export async function runMissionClient(
  namespace: string,
  name: string,
): Promise<{ ok: boolean; error: string | null }> {
  try {
    const res = await fetch(
      `/api/namespaces/${encodeURIComponent(namespace)}/tasks/${encodeURIComponent(name)}/run`,
      {
        method: "POST",
        cache: "no-store",
        headers: { accept: "application/json" },
      },
    );
    const body = await res.json().catch(() => null);
    if (!res.ok || !body?.ok) {
      return { ok: false, error: body?.error ?? `Run failed (${res.status}).` };
    }
    return { ok: true, error: null };
  } catch (err) {
    return {
      ok: false,
      error: err instanceof Error ? err.message : "unknown error",
    };
  }
}
