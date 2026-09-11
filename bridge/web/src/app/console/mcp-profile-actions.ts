"use server";

import { revalidatePath } from "next/cache";
import { BffError, putMcpProfile, deleteMcpProfile } from "@/lib/bff";

export interface McpProfileState {
  error: string | null;
  ok: string | null;
}

/// Create/update an operator MCP profile (a vetted bundle of McpServers).
export async function saveMcpProfileAction(_prev: McpProfileState, form: FormData): Promise<McpProfileState> {
  const name = String(form.get("name") ?? "").trim();
  const summary = String(form.get("summary") ?? "").trim();
  const servers = form.getAll("servers").map((s) => String(s));
  if (!name) return { error: "Profile name is required.", ok: null };
  if (!/^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(name)) {
    return { error: "Name must be lowercase alphanumeric + hyphens.", ok: null };
  }
  if (servers.length === 0) return { error: "Select at least one server.", ok: null };
  try {
    await putMcpProfile({ name, summary: summary || null, servers });
    revalidatePath("/console/configuration");
    revalidatePath("/console/capabilities");
    return { error: null, ok: `Profile “${name}” saved (${servers.length} server${servers.length === 1 ? "" : "s"}).` };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "save failed", ok: null };
  }
}

export async function deleteMcpProfileAction(_prev: McpProfileState, form: FormData): Promise<McpProfileState> {
  const name = String(form.get("name") ?? "").trim();
  if (!name) return { error: "Name is required.", ok: null };
  try {
    await deleteMcpProfile(name);
    revalidatePath("/console/configuration");
    revalidatePath("/console/capabilities");
    return { error: null, ok: `Profile “${name}” removed.` };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "delete failed", ok: null };
  }
}
