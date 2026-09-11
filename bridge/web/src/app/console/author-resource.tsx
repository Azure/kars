"use client";

import { useActionState, useState } from "react";
import { applyGovernanceAction, type GovState } from "./governance-actions";

const init: GovState = { error: null, ok: null };

/** Per-kind spec templates so an operator starts from a valid skeleton instead
 *  of a blank box. Authoring uses Server-Side Apply, so the same form creates a
 *  new object and edits an existing one (pass `initialName` + `initialSpec`). */
const TEMPLATES: Record<string, string> = {
  ToolPolicy: JSON.stringify(
    {
      appliesTo: { sandboxMatchLabels: { "kars.azure.com/example": "true" } },
      agtProfile: { inline: "# AGT policy (agentmesh PolicyEngine format)\nallow inference:*\nallow tool:*\n" },
    },
    null,
    2,
  ),
  McpServer: JSON.stringify(
    { url: "https://example.internal/mcp", allowedTools: ["*"], displayName: "Example MCP server" },
    null,
    2,
  ),
  KarsSkill: JSON.stringify(
    {
      summary: "What this skill does",
      version: "0.1.0",
      boundingPolicy: "kars-default",
      recipe: "Standing instructions for using this capability well.",
      scripts: [
        { path: "scripts/helper.sh", content: "#!/usr/bin/env bash\necho hello", executable: true },
      ],
    },
    null,
    2,
  ),
  KarsProfile: JSON.stringify(
    {
      displayName: "Example team profile",
      domain: "eng",
      charterTemplate: "Keep the repo healthy: triage issues, watch PRs, report failing checks.",
      defaultEnvelope: { tier: 3, authorityCeiling: 2, delegationDepth: 1, toolPolicyRef: { name: "kars-default" } },
      toolPolicy: "kars-default",
      roles: [{ name: "triager", systemPrompt: "Triage incoming issues.", skills: [] }],
    },
    null,
    2,
  ),
  InferencePolicy: JSON.stringify(
    {
      appliesTo: { sandboxMatchLabels: { "kars.azure.com/example": "true" } },
      tokenBudget: { dailyTokens: 50000 },
      contentSafety: { hate: "medium", violence: "medium" },
      displayName: "Example inference policy",
    },
    null,
    2,
  ),
};

const LABEL: Record<string, string> = {
  ToolPolicy: "tool policy",
  McpServer: "MCP server",
  KarsSkill: "skill",
  KarsProfile: "team profile",
  InferencePolicy: "inference policy",
};

export function AuthorResource({
  kind,
  initialName,
  initialSpec,
}: {
  kind: "ToolPolicy" | "McpServer" | "KarsSkill" | "KarsProfile" | "InferencePolicy";
  initialName?: string;
  initialSpec?: string;
}) {
  const [open, setOpen] = useState(false);
  const [state, action, pending] = useActionState(applyGovernanceAction, init);
  const editing = Boolean(initialName);

  if (!open) {
    return (
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium text-foreground-muted hover:text-foreground"
      >
        {editing ? `Edit ${initialName}` : `+ Add ${LABEL[kind]}`}
      </button>
    );
  }

  return (
    <form action={action} className="mt-2 space-y-2 rounded-lg border border-border bg-surface-muted p-3">
      <input type="hidden" name="kind" value={kind} />
      <div className="flex items-center justify-between">
        <p className="text-xs font-medium">{editing ? `Edit ${LABEL[kind]}` : `New ${LABEL[kind]}`}</p>
        <button type="button" onClick={() => setOpen(false)} className="text-xs text-foreground-muted hover:text-foreground">Cancel</button>
      </div>
      <input
        name="name"
        defaultValue={initialName}
        readOnly={editing}
        placeholder="name (lowercase-with-hyphens)"
        required
        className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-sm read-only:opacity-70"
      />
      <textarea
        name="spec"
        defaultValue={initialSpec ?? TEMPLATES[kind]}
        rows={10}
        spellCheck={false}
        className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs leading-relaxed"
      />
      <p className="text-[11px] text-foreground-muted">
        Applied with <span className="font-mono">kubectl apply</span> semantics (field manager <span className="font-mono">kars-bridge</span>). The cluster validates it — invalid specs are rejected with the API server&apos;s own message.
      </p>
      <label className="flex items-center gap-2 text-[11px] text-foreground-muted">
        <input type="checkbox" name="force" /> Force — take ownership of fields another manager owns (only on a conflict)
      </label>
      <div className="flex items-center gap-3">
        <button type="submit" disabled={pending} className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg disabled:opacity-50">
          {pending ? "Applying…" : editing ? "Save changes" : `Create ${LABEL[kind]}`}
        </button>
        {state.error && <p className="text-xs text-danger">{state.error}</p>}
        {state.ok && <p className="text-xs text-ok">{state.ok}</p>}
      </div>
    </form>
  );
}
