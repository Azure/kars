// kars Bridge Workspace — Artifacts. The cross-mission deliverable index.
//
// Lists the REAL deliverables missions have produced — the files captured by
// the controller from the agent loop over the mesh, persisted as durable
// cluster records (kars-mission-output / kars-mission-artifacts ConfigMaps).
// Each mission links to its page, where the full artifact set is reviewed in
// place alongside its Governance Receipt. Honestly empty until a mission
// produces a deliverable — never fabricated.

import Link from "next/link";
import { HonestState } from "@/components/honest-state";
import { Icon, type IconName } from "@/components/icon";
import { BffError, getArtifacts } from "@/lib/bff";
import type { MissionArtifacts } from "@/lib/types";

export const dynamic = "force-dynamic";

function fmtSize(n: number | null): string {
  if (n == null) return "";
  if (n < 1024) return `${n} B`;
  return `${(n / 1024).toFixed(1)} KB`;
}

/** Collapse duplicate artifact entries by name (the BFF can surface the same
 *  file from both the output manifest and the artifacts ConfigMap). Keeps the
 *  first (richest) occurrence, so React keys stay unique and the count is true. */
function dedupeByName<T extends { name: string }>(files: T[]): T[] {
  const seen = new Set<string>();
  const out: T[] = [];
  for (const f of files) {
    if (seen.has(f.name)) continue;
    seen.add(f.name);
    out.push(f);
  }

  return out;
}

function isInternalArtifact(name: string): boolean {
  const normalized = name.toLowerCase();
  return (
    normalized.includes("collaboration.jsonl") ||
    normalized.endsWith("role-plan.json") ||
    normalized.endsWith("research-evidence.jsonl") ||
    normalized.endsWith("activity.jsonl")
  );
}

/** Map a filename to a glyph + human kind, so a deliverable index reads as a
 *  gallery of recognisable things rather than a wall of monospace. */
function fileMeta(name: string): { glyph: IconName; kind: string } {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  if (["md", "mdx", "txt", "rst", "adoc"].includes(ext)) return { glyph: "file", kind: "Document" };
  if (["ts", "tsx", "js", "jsx", "rs", "py", "go", "java", "rb", "c", "cpp", "h", "sh"].includes(ext))
    return { glyph: "puzzle", kind: "Code" };
  if (["json", "yaml", "yml", "toml", "xml"].includes(ext)) return { glyph: "gear", kind: "Config" };
  if (["csv", "tsv", "parquet", "xlsx"].includes(ext)) return { glyph: "chart", kind: "Data" };
  if (["png", "jpg", "jpeg", "gif", "svg", "webp"].includes(ext)) return { glyph: "layers", kind: "Image" };
  if (["pdf"].includes(ext)) return { glyph: "note", kind: "PDF" };
  if (["html", "htm"].includes(ext)) return { glyph: "globe", kind: "Web" };
  return { glyph: "box", kind: "File" };
}

export default async function ArtifactsPage({
  searchParams,
}: {
  searchParams: Promise<{ q?: string; view?: string; limit?: string }>;
}) {
  const { q = "", view = "deliverables", limit = "recent" } = await searchParams;
  let index;
  let clusterDown = false;
  try {
    index = await getArtifacts();
  } catch (err) {
    if (err instanceof BffError && err.code === "cluster_unavailable") {
      clusterDown = true;
    } else {
      throw err;
    }
  }

  const allMissions = index?.missions ?? [];
  const query = q.trim().toLowerCase();
  const filteredMissions = allMissions.filter((mission) => {
    if (view !== "all" && mission.status === "error") return false;
    if (!query) return true;
    return [
      mission.display_name,
      mission.objective,
      mission.excerpt,
      mission.team,
      ...mission.pull_requests.map((pr) => `${pr.repo} #${pr.number}`),
    ].some((value) => value?.toLowerCase().includes(query));
  });
  const missions = limit === "all" ? filteredMissions : filteredMissions.slice(0, 10);

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Deliverables</h1>
        <p className="mt-1 text-sm text-foreground-muted">
          Customer-facing outcomes: pull requests, reports, documents, and other reviewable work.
          Internal collaboration files and failed-run diagnostics remain available in All records.
        </p>
      </div>

      <form className="flex flex-wrap items-center gap-2 rounded-xl border border-border bg-surface p-3">
        <input
          type="search"
          name="q"
          defaultValue={q}
          placeholder="Search deliverables, teams, repositories, or PRs"
          className="min-w-64 flex-1 rounded-lg border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-signal"
        />
        <input type="hidden" name="view" value={view} />
        <input type="hidden" name="limit" value={limit} />
        <button className="rounded-lg bg-signal px-3 py-2 text-xs font-semibold text-signal-fg">
          Search
        </button>
        <Link
          href={`/workspace/artifacts${q ? `?q=${encodeURIComponent(q)}&` : "?"}view=${view === "all" ? "deliverables" : "all"}`}
          className="rounded-lg border border-border px-3 py-2 text-xs font-medium"
        >
          {view === "all" ? "Show deliverables only" : "Show all records"}
        </Link>
        {filteredMissions.length > 10 && (
          <Link
            href={`/workspace/artifacts?view=${encodeURIComponent(view)}&limit=${limit === "all" ? "recent" : "all"}${q ? `&q=${encodeURIComponent(q)}` : ""}`}
            className="rounded-lg border border-border px-3 py-2 text-xs font-medium"
          >
            {limit === "all" ? "Show latest 10" : `Show all ${filteredMissions.length}`}
          </Link>
        )}
      </form>

      {clusterDown ? (
        <HonestState
          variant="not_wired"
          title="The run environment isn't connected"
          detail="Artifacts are read from live cluster records, but the cluster isn't reachable right now — so nothing can be listed. This is an environment state, not a faked screen."
        />
      ) : missions.length === 0 ? (
        <HonestState
          variant={query ? "empty" : "needs_run"}
          title={query ? "No matching deliverables" : "No deliverables captured yet"}
          detail={
            query
              ? "Try a different team, repository, PR number, or outcome phrase."
              : view === "all"
                ? "No retained run records are available."
                : "No customer-facing outcome is available yet. Failed and internal records remain under All records."
          }
        />
      ) : (
        <>
          <div className="kb-rise">
            <p className="mb-2 text-[11px] uppercase tracking-wide text-foreground-muted">
              Latest deliverable
            </p>
            <ul className="space-y-4">
              <MissionArtifactsCard key={missions[0].task} m={missions[0]} hero showInternal={view === "all"} />
            </ul>
          </div>
          {missions.length > 1 && (
            <ul className="space-y-4">
              {missions.slice(1).map((m) => (
                <MissionArtifactsCard key={m.evidence_key ?? m.task} m={m} showInternal={view === "all"} />
              ))}
            </ul>
          )}
        </>
      )}
    </div>
  );
}

function MissionArtifactsCard({
  m,
  hero = false,
  showInternal = false,
}: {
  m: MissionArtifacts;
  hero?: boolean;
  showInternal?: boolean;
}) {
  const ok = m.status !== "error";
  const historicalNonTeam = Boolean(
    !m.team && m.evidence_key && m.evidence_key !== m.task,
  );
  const detailHref = m.team
    ? m.evidence_key && m.evidence_key !== m.task
      ? `/workspace/teams/${encodeURIComponent(m.team)}?tab=runs`
      : `/workspace/teams/${encodeURIComponent(m.team)}/runs/${encodeURIComponent(m.task)}`
    : `/workspace/missions/${encodeURIComponent(m.task)}`;
  const allFiles = dedupeByName(m.files);
  const internalCount = allFiles.filter((file) => isInternalArtifact(file.name)).length;
  const files = showInternal ? allFiles : allFiles.filter((file) => !isInternalArtifact(file.name));
  const fileCount = files.length;
  // Deliverable kind (audit f14): the dominant file type, or a text deliverable
  // when the run produced prose only — so the index reads as a typed gallery.
  const kind = fileCount > 0 ? fileMeta(files[0].name).kind : "Text deliverable";
  return (
    <li className={`rounded-xl border bg-surface p-5 ${hero ? "border-signal/40 bg-signal/5 kb-card-hover" : "border-border"}`}>
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="rounded-full border border-accent/30 bg-accent/10 px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-accent">{kind}</span>
            {hero && <span className="text-[10px] font-semibold uppercase tracking-wide text-signal">Latest</span>}
          </div>
          {historicalNonTeam ? (
            <p className="mt-1 text-sm font-semibold">{m.display_name ?? m.task}</p>
          ) : (
            <Link
              href={detailHref}
              className="mt-1 block text-sm font-semibold text-signal hover:underline"
            >
              {m.display_name ?? m.task}
            </Link>
          )}
          {m.team && (
            <p className="mt-0.5 text-[11px] text-foreground-muted">
              Team: {m.team}{m.archived ? " · archived delivery" : ""}
            </p>
          )}
          {m.excerpt ? (
            <p className="mt-0.5 line-clamp-2 text-xs text-foreground-muted">{m.excerpt}</p>
          ) : m.objective ? (
            <p className="mt-0.5 line-clamp-2 text-xs text-foreground-muted">{m.objective}</p>
          ) : null}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {!ok && (
            <span className="rounded-full bg-rose-500/10 px-2 py-0.5 text-xs font-medium text-rose-500">
              error
            </span>
          )}
          {m.archived && (
            <span className="rounded-full border border-border bg-surface-muted px-2.5 py-0.5 text-xs font-medium text-foreground-muted">
              Archived
            </span>
          )}
          {m.review_status === "approved" && (
            <span className="rounded-full border border-emerald-500/30 bg-emerald-500/10 px-2.5 py-0.5 text-xs font-medium text-emerald-600">
              Approved
            </span>
          )}
          {m.review_status === "changes_requested" && (
            <span className="rounded-full border border-amber-500/30 bg-amber-500/10 px-2.5 py-0.5 text-xs font-medium text-amber-600">
              Changes requested{m.review_revision > 0 ? ` · rev ${m.review_revision}` : ""}
            </span>
          )}
          <span className="rounded-full bg-surface-muted px-2.5 py-1 text-xs font-medium">
            {fileCount > 0 ? `${fileCount} file${fileCount === 1 ? "" : "s"}` : "Text only"}
          </span>
          {!showInternal && internalCount > 0 && (
            <span className="text-[10px] text-foreground-muted">
              {internalCount} internal hidden
            </span>
          )}
        </div>
      </div>

      {fileCount > 0 && (
        <ul className="mt-3 flex flex-wrap gap-2">
          {files.map((f, idx) => {
            const meta = fileMeta(f.name);
            return (
              <li
                key={`${f.name}-${idx}`}
                className="inline-flex items-center gap-2 rounded-lg border border-border bg-surface-muted/40 px-2.5 py-1.5"
                title={meta.kind}
              >
                <span aria-hidden className="text-sm leading-none"><Icon name={meta.glyph} size={14} /></span>
                <span className="font-mono text-xs">{f.name}</span>
                {f.size_bytes != null && (
                  <span className="text-[11px] text-foreground-muted">· {fmtSize(f.size_bytes)}</span>
                )}
              </li>
            );
          })}
        </ul>
      )}

      {m.pull_requests.length > 0 && (
        <ul className="mt-3 flex flex-wrap gap-2">
          {m.pull_requests.map((pr) => (
            <li key={pr.url}>
              <a
                href={pr.url}
                target="_blank"
                rel="noreferrer"
                className="inline-flex items-center gap-2 rounded-lg border border-signal/30 bg-signal/5 px-2.5 py-1.5 hover:bg-signal/10"
                title={`Pull request on ${pr.repo}`}
              >
                <span aria-hidden className="text-sm leading-none"><Icon name="branch" size={14} /></span>
                <span className="text-xs font-medium text-signal">PR #{pr.number}</span>
                <span className="font-mono text-[11px] text-foreground-muted">{pr.repo}</span>
                <span aria-hidden className="text-[11px] text-foreground-muted">↗</span>
              </a>
            </li>
          ))}
        </ul>
      )}

      <dl className="mt-3 flex flex-wrap gap-x-6 gap-y-1 text-xs text-foreground-muted">
        {m.model && (
          <div className="flex gap-1.5">
            <dt>Model</dt>
            <dd className="font-medium text-foreground">{m.model}</dd>
          </div>
        )}
        {m.finished_at && (
          <div className="flex gap-1.5">
            <dt>Produced</dt>
            <dd className="font-medium text-foreground">
              {new Date(m.finished_at).toLocaleString()}
            </dd>
          </div>
        )}
        {m.deliverable_did && (
          <div className="flex min-w-0 gap-1.5">
            <dt>Identity</dt>
            <dd className="truncate font-mono text-[11px]" title={m.deliverable_did}>{m.deliverable_did}</dd>
          </div>
        )}
        <div className="flex gap-1.5">
          <dt>Review</dt>
          <dd>
            <Link
              href={detailHref}
              className="text-signal hover:underline"
            >
              {historicalNonTeam ? "Open current mission →" : "Open in place →"}
            </Link>
          </dd>
        </div>
      </dl>
    </li>
  );
}
