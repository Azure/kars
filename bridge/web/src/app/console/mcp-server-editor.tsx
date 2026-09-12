"use client";

// kars Bridge Operator Console — visual McpServer editor. Replaces the raw-
// JSON textarea (AuthorResource) for CUSTOM server registration with real
// fields matching the actual CRD schema (controller/src/mcp_server.rs): url,
// displayName, allowedTools, allowedSandboxes (label selector), production
// mode + OAuth 2.1 (issuer/audience/resource), scopes, and bearerFromEnv
// (static-bearer outbound auth for e.g. GitHub Copilot's dev token). Submits
// through the SAME applyGovernanceAction the textarea used, so the backend
// contract is unchanged; only the authoring experience is visual. Popular
// known servers should still go through the one-click McpCatalog picker —
// this editor is for a custom/self-hosted server or fine-grained editing.
// An "Edit as JSON" escape hatch stays available for bundleRef (a signed OCI
// server bundle — mutually exclusive with the inline fields this form sets).

import { useActionState, useState } from "react";
import { applyGovernanceAction, type GovState } from "./governance-actions";
import { Icon } from "@/components/icon";

const init: GovState = { error: null, ok: null };

interface FormShape {
  url: string;
  displayName: string;
  allowedTools: string; // comma-separated; "*" = all
  labels: { key: string; value: string }[];
  productionMode: boolean;
  issuer: string;
  audience: string;
  resource: string;
  scopes: string; // comma-separated
  bearerFromEnv: string;
}

function parseSpec(spec: Record<string, unknown> | undefined): FormShape {
  const oauth = (spec?.oauth as Record<string, unknown>) ?? {};
  const allowedSandboxes = (spec?.allowedSandboxes as Record<string, unknown>) ?? {};
  const matchLabels = (allowedSandboxes.matchLabels as Record<string, string>) ?? {};
  return {
    url: (spec?.url as string) ?? "",
    displayName: (spec?.displayName as string) ?? "",
    allowedTools: ((spec?.allowedTools as string[]) ?? []).join(", "),
    labels: Object.entries(matchLabels).map(([key, value]) => ({ key, value })),
    productionMode: spec?.productionMode === true,
    issuer: (oauth.issuer as string) ?? "",
    audience: (oauth.audience as string) ?? "",
    resource: (oauth.resource as string) ?? "",
    scopes: ((spec?.scopes as string[]) ?? []).join(", "),
    bearerFromEnv: (spec?.bearerFromEnv as string) ?? "",
  };
}

function buildSpec(f: FormShape): Record<string, unknown> {
  const spec: Record<string, unknown> = {};
  if (f.url.trim()) spec.url = f.url.trim();
  if (f.displayName.trim()) spec.displayName = f.displayName.trim();

  const allowedTools = f.allowedTools.split(",").map((t) => t.trim()).filter(Boolean);
  if (allowedTools.length) spec.allowedTools = allowedTools;

  const matchLabels: Record<string, string> = {};
  for (const { key, value } of f.labels) {
    if (key.trim()) matchLabels[key.trim()] = value.trim();
  }
  if (Object.keys(matchLabels).length > 0) spec.allowedSandboxes = { matchLabels };

  const scopes = f.scopes.split(",").map((s) => s.trim()).filter(Boolean);
  if (scopes.length) spec.scopes = scopes;

  if (f.productionMode) {
    spec.productionMode = true;
    const oauth: Record<string, unknown> = { issuer: f.issuer.trim() };
    if (f.audience.trim()) oauth.audience = f.audience.trim();
    if (f.resource.trim()) oauth.resource = f.resource.trim();
    spec.oauth = oauth;
  }

  if (f.bearerFromEnv.trim()) spec.bearerFromEnv = f.bearerFromEnv.trim();

  return spec;
}

export function McpServerEditor({
  initialName,
  initialSpec,
}: {
  initialName?: string;
  initialSpec?: Record<string, unknown>;
}) {
  const [open, setOpen] = useState(false);
  const [state, action, pending] = useActionState(applyGovernanceAction, init);
  const editing = Boolean(initialName);
  const managed = Boolean(initialSpec?.managed);
  // Managed presets are a typed controller-owned shape; the external endpoint
  // visual form must never silently replace `spec.managed` with an empty URL.
  const [advanced, setAdvanced] = useState(managed);
  const [f, setF] = useState<FormShape>(() => parseSpec(initialSpec));
  const [rawSpec, setRawSpec] = useState(() => JSON.stringify(initialSpec ?? {}, null, 2));

  if (!open) {
    return (
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium text-foreground-muted hover:text-foreground"
      >
        {editing ? `Edit ${initialName}` : "+ Add custom MCP server"}
      </button>
    );
  }

  const specJson = advanced ? rawSpec : JSON.stringify(buildSpec(f));
  const patch = (p: Partial<FormShape>) => setF((prev) => ({ ...prev, ...p }));
  const productionBlocked = f.productionMode && !advanced && f.issuer.trim() === "";

  return (
    <form action={action} className="mt-2 space-y-4 rounded-lg border border-border bg-surface-muted p-4">
      <input type="hidden" name="kind" value="McpServer" />
      <input type="hidden" name="spec" value={specJson} />
      <div className="flex items-center justify-between">
        <p className="text-xs font-medium">{editing ? `Edit MCP server` : "New custom MCP server"}</p>
        <div className="flex items-center gap-3">
          {managed ? (
            <span className="text-xs text-foreground-muted">Managed preset JSON</span>
          ) : (
            <button
              type="button"
              onClick={() => setAdvanced((v) => !v)}
              className="text-xs text-foreground-muted hover:text-foreground"
            >
              {advanced ? "Use visual form" : "Edit as JSON"}
            </button>
          )}
          <button type="button" onClick={() => setOpen(false)} className="text-xs text-foreground-muted hover:text-foreground">Cancel</button>
        </div>
      </div>

      <input
        name="name"
        defaultValue={initialName}
        readOnly={editing}
        placeholder="name (lowercase-with-hyphens)"
        required
        className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-sm read-only:opacity-70"
      />

      {advanced ? (
        <textarea
          value={rawSpec}
          onChange={(e) => setRawSpec(e.target.value)}
          rows={14}
          spellCheck={false}
          className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs leading-relaxed"
        />
      ) : (
        <div className="space-y-4">
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="Server URL">
              <input
                value={f.url}
                onChange={(e) => patch({ url: e.target.value })}
                placeholder="https://mcp.example.internal"
                className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs"
              />
            </Field>
            <Field label="Display name">
              <input
                value={f.displayName}
                onChange={(e) => patch({ displayName: e.target.value })}
                placeholder="e.g. Internal docs search"
                className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm"
              />
            </Field>
          </div>

          <Field label="Allowed tools (comma-separated — use * for all, governed by ToolPolicy)">
            <input
              value={f.allowedTools}
              onChange={(e) => patch({ allowedTools: e.target.value })}
              placeholder="search, fetch_page"
              className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs"
            />
          </Field>

          <fieldset className="rounded-lg border border-border p-3">
            <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="target" size={13} /> Allowed sandboxes</legend>
            <p className="mb-1 text-xs text-foreground-muted">Label selector (AND). Empty = same-namespace only.</p>
            {f.labels.map((row, i) => (
              <div key={i} className="mb-1.5 flex items-center gap-2">
                <input
                  value={row.key}
                  onChange={(e) => patch({ labels: f.labels.map((r, j) => (j === i ? { ...r, key: e.target.value } : r)) })}
                  placeholder="kars.azure.com/team"
                  className="w-1/2 rounded-lg border border-border bg-surface px-2.5 py-1.5 font-mono text-xs"
                />
                <input
                  value={row.value}
                  onChange={(e) => patch({ labels: f.labels.map((r, j) => (j === i ? { ...r, value: e.target.value } : r)) })}
                  placeholder="value"
                  className="w-1/2 rounded-lg border border-border bg-surface px-2.5 py-1.5 font-mono text-xs"
                />
                <button type="button" onClick={() => patch({ labels: f.labels.filter((_, j) => j !== i) })} className="shrink-0 text-foreground-muted hover:text-danger">
                  <Icon name="cross" size={13} />
                </button>
              </div>
            ))}
            <button
              type="button"
              onClick={() => patch({ labels: [...f.labels, { key: "", value: "" }] })}
              className="text-xs text-signal hover:underline"
            >
              + Add label
            </button>
          </fieldset>

          <fieldset className="rounded-lg border border-border p-3">
            <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="lock" size={13} /> Authentication</legend>
            <label className="flex items-center gap-2 text-xs text-foreground-muted">
              <input type="checkbox" checked={f.productionMode} onChange={(e) => patch({ productionMode: e.target.checked })} />
              Production mode — require OAuth 2.1 bearer auth (dev-only if unchecked)
            </label>
            {f.productionMode && (
              <div className="mt-2 grid gap-3 sm:grid-cols-3">
                <Field label="OAuth issuer (required)">
                  <input
                    value={f.issuer}
                    onChange={(e) => patch({ issuer: e.target.value })}
                    placeholder="https://issuer.example.com"
                    className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs"
                  />
                </Field>
                <Field label="Audience (optional)">
                  <input
                    value={f.audience}
                    onChange={(e) => patch({ audience: e.target.value })}
                    className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs"
                  />
                </Field>
                <Field label="Resource indicator (optional)">
                  <input
                    value={f.resource}
                    onChange={(e) => patch({ resource: e.target.value })}
                    className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs"
                  />
                </Field>
              </div>
            )}
            <div className="mt-2 grid gap-3 sm:grid-cols-2">
              <Field label="OAuth scopes (comma-separated, optional)">
                <input
                  value={f.scopes}
                  onChange={(e) => patch({ scopes: e.target.value })}
                  placeholder="read:docs"
                  className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs"
                />
              </Field>
              <Field label="Outbound bearer from env var (optional)">
                <input
                  value={f.bearerFromEnv}
                  onChange={(e) => patch({ bearerFromEnv: e.target.value })}
                  placeholder="COPILOT_GITHUB_TOKEN"
                  className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs"
                />
              </Field>
            </div>
            <p className="mt-1.5 text-[11px] text-foreground-muted">
              Reuses a pre-existing sandbox env var as an <span className="font-mono">Authorization</span> bearer
              header on every outbound call to this server — no new credential mount. Unset/empty is skipped, non-fatal.
            </p>
          </fieldset>
        </div>
      )}

      <p className="text-[11px] text-foreground-muted">
        Applied with <span className="font-mono">kubectl apply</span> semantics (field manager <span className="font-mono">kars-bridge</span>). The cluster validates it — invalid specs are rejected with the API server&apos;s own message.
      </p>
      <label className="flex items-center gap-2 text-[11px] text-foreground-muted">
        <input type="checkbox" name="force" /> Force — take ownership of fields another manager owns (only on a conflict)
      </label>
      <div className="flex items-center gap-3">
        <button type="submit" disabled={pending || productionBlocked} className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg disabled:opacity-50">
          {pending ? "Applying…" : editing ? "Save changes" : "Create MCP server"}
        </button>
        {productionBlocked && <p className="text-xs text-warning">Production mode needs an OAuth issuer.</p>}
        {state.error && <p className="text-xs text-danger">{state.error}</p>}
        {state.ok && <p className="text-xs text-ok">{state.ok}</p>}
      </div>
    </form>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="block text-xs text-foreground-muted">
      {label}
      <div className="mt-1">{children}</div>
    </label>
  );
}
