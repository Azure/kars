// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

// The legacy git_write_repos field carries the selected repository set. Typed
// Missions and Teams derive authority from reviewed github-pr capabilities;
// the server independently derives and authorizes the binding.

import { useEffect, useState } from "react";
import { defaultNamespace } from "@/lib/config";
import type { ExecutionPlan } from "@/lib/types";

export function missionGithubWriteEnabled(plan?: Partial<ExecutionPlan> | null): boolean {
  // The review editor also supplies incomplete JSON while a plan is being edited.
  const canWrite = (capabilities: unknown) => Array.isArray(capabilities) && capabilities.includes("github-pr");
  return !plan || canWrite(plan.synthesis?.capabilities)
    || (Array.isArray(plan.roles) && plan.roles.some((role) => Array.isArray(role?.phases)
      && role.phases.some((phase) => canWrite(phase?.capabilities))));
}

type Connection = { connected: boolean; account: string | null; repos: string[] };

export function RepoAccess({
  ns = defaultNamespace(),
  initialSelected = [],
  selected: controlledSelection,
  onSelectionChange,
  plan,
  readOnly = !missionGithubWriteEnabled(plan),
}: {
  ns?: string;
  initialSelected?: string[];
  selected?: string[];
  onSelectionChange?: (repos: string[]) => void;
  readOnly?: boolean;
  plan?: Partial<ExecutionPlan> | null;
}) {
  const [conn, setConn] = useState<Connection | null>(null);
  const [error, setError] = useState(false);
  const [localSelection, setLocalSelection] = useState<string[]>(initialSelected);
  const selected = new Set(controlledSelection ?? localSelection);

  useEffect(() => {
    let live = true;
    fetch(`/api/namespaces/${ns}/github/connection`)
      .then((r) => r.json())
      .then((c: Connection) => {
        if (live) setConn(c);
      })
      .catch(() => {
        if (live) setError(true);
      });
    return () => {
      live = false;
    };
  }, [ns]);

  const selectedHas = (repos: Set<string>, repo: string) =>
    Array.from(repos).some((entry) => entry.toLowerCase() === repo.toLowerCase());

  const toggle = (repo: string) => {
    const next = new Set(selected);
    const selectedEntry = Array.from(next).find(
      (entry) => entry.toLowerCase() === repo.toLowerCase(),
    );
    if (selectedEntry) next.delete(selectedEntry);
    else next.add(repo);
    const repos = Array.from(next);
    if (controlledSelection === undefined) setLocalSelection(repos);
    onSelectionChange?.(repos);
  };

  const value = Array.from(selected).join(",");
  const availableRepos = conn?.repos ?? [];
  const repoRows = [...availableRepos, ...Array.from(selected)].filter(
    (repo, index, rows) =>
      rows.findIndex((entry) => entry.toLowerCase() === repo.toLowerCase()) === index,
  );
  const isAvailable = (repo: string) =>
    availableRepos.some((entry) => entry.toLowerCase() === repo.toLowerCase());

  return (
    <div className="rounded-xl border border-border bg-surface-muted/40 px-4 py-3">
      <input type="hidden" name="git_write_repos" value={value} />
      <div className="flex items-center justify-between gap-2">
        <div>
          <p className="text-sm font-medium">{readOnly ? "Read-only repository access" : "Pull request access"}</p>
          <p className="mt-0.5 text-xs text-foreground-muted">
            {readOnly
              ? "This reviewed plan has no GitHub PR capability. Selected repositories receive read-only access through the router; GitHub writes are denied."
              : "Let this work open pull requests on connected repos."}
            {" "}Agents never hold a credential — a scoped token is injected at run time.
          </p>
        </div>
      </div>

      {error ? (
        <p className="mt-2 text-xs text-warning">Couldn’t load the GitHub connection.</p>
      ) : conn == null ? (
        <p className="mt-2 text-xs text-foreground-muted">Loading connected repos…</p>
      ) : repoRows.length === 0 ? (
        <p className="mt-2 text-xs text-foreground-muted">
          No repository is connected for your user yet. Connect GitHub on the{" "}
          <span className="font-medium text-foreground">Configuration</span> console to grant
          {readOnly ? " read-only repository access." : " pull-request access."}
          {" "}Leaving this empty grants no connected GitHub access.
        </p>
      ) : (
        <div className="mt-2 space-y-1.5">
          {conn.connected && conn.account && (
            <p className="text-[11px] text-foreground-muted">
              Connected as <span className="font-medium text-foreground">{conn.account}</span>
            </p>
          )}
          {!conn.connected && selected.size > 0 && (
            <p className="text-[11px] text-warning">
              GitHub is disconnected. Existing grants remain listed so you can revoke them.
            </p>
          )}
          {repoRows.map((repo) => (
            <label key={repo} className="flex cursor-pointer items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={selectedHas(selected, repo)}
                onChange={() => toggle(repo)}
                className="h-3.5 w-3.5 rounded border-border accent-signal"
              />
              <span className="font-mono text-xs">{repo}</span>
              {!isAvailable(repo) && (
                <span className="text-[11px] text-warning">
                  no longer connected — uncheck to revoke
                </span>
              )}
            </label>
          ))}
          {selected.size === 0 && (
            <p className="text-[11px] text-foreground-muted">
              {readOnly
                ? "None selected — no connected GitHub access. Tick a repo to allow read-only verification."
                : "None selected — no git write. Tick a repo to allow opening PRs."}
            </p>
          )}
        </div>
      )}
    </div>
  );
}
