"use client";

// kars Bridge — Repository access (keyless git write) for a mission or team.
//
// Each principal connects repos through the shared GitHub App. Here the user
// grants a subset of only their own connected repositories
// to THIS mission/team so its agents can open pull requests — without ever holding
// a credential (the router mints + injects a scoped token at run time). The
// selection is written to a hidden `git_write_repos` field the create action reads;
// the controller clamps it to declared ∩ connection-granted.

import { useEffect, useState } from "react";
import { defaultNamespace } from "@/lib/config";

type Connection = { connected: boolean; account: string | null; repos: string[] };

export function RepoAccess({
  ns = defaultNamespace(),
  initialSelected = [],
  onSelectionChange,
}: {
  ns?: string;
  initialSelected?: string[];
  onSelectionChange?: (repos: string[]) => void;
}) {
  const [conn, setConn] = useState<Connection | null>(null);
  const [error, setError] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(
    () => new Set(initialSelected),
  );

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

  const toggle = (repo: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      const selectedEntry = Array.from(next).find(
        (entry) => entry.toLowerCase() === repo.toLowerCase(),
      );
      if (selectedEntry) next.delete(selectedEntry);
      else next.add(repo);
      onSelectionChange?.(Array.from(next));
      return next;
    });

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
          <p className="text-sm font-medium">Pull request access</p>
          <p className="mt-0.5 text-xs text-foreground-muted">
            Let this {`work`} open pull requests on connected repos. Agents never hold a
            credential — a scoped token is injected at run time.
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
          pull-request access. Leaving this empty means no git write — the mission can still
          read/clone public repos.
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
              None selected — no git write. Tick a repo to allow opening PRs.
            </p>
          )}
        </div>
      )}
    </div>
  );
}
