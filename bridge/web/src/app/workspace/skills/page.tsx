// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge Workspace — Skills. The USER side of the skill trust gate: upload
// a skill package, watch it move through operator review, and see which skills
// are approved + usable to assign to a task or team. Uploading proposes
// capability; a different operator reviews the validated content before use.

import { PageHeader, Section } from "@/components/ui";
import { HonestState } from "@/components/honest-state";
import { listUserSkills, getOptions } from "@/lib/bff";
import type { SkillSummary, Options } from "@/lib/types";
import { SkillUpload } from "./skill-upload";
import { LiveRefresh } from "@/components/live-refresh";

export const dynamic = "force-dynamic";

function reviewBadge(s: SkillSummary) {
  if (s.usable) return { label: "Approved · usable", cls: "border-ok/30 bg-ok/10 text-ok" };
  if (s.review === "approved")
    return { label: "Approval no longer current", cls: "border-warning/30 bg-warning/10 text-warning" };
  return { label: "Pending operator review", cls: "border-border bg-surface-muted text-foreground-muted" };
}

export default async function SkillsPage() {
  let skills: SkillSummary[] = [];
  let options: Options | null = null;
  let error = false;
  try {
    [skills, options] = await Promise.all([listUserSkills(), getOptions().catch(() => null)]);
  } catch {
    error = true;
  }

  const usable = skills.filter((s) => s.usable);
  const pending = skills.filter((s) => !s.usable);

  return (
    <div className="space-y-6">
      <LiveRefresh
        active={!error && skills.length > 0}
        intervalMs={skills.some((s) => !s.validated && s.phase !== "Degraded") ? 3500 : 10000}
      />
      <PageHeader
        eyebrow="Workspace"
        title="Skills"
        lead="Reusable instructions and assets for your missions and teams. Upload a skill for content validation and independent operator review. Approval binds its current identity and content; it does not grant additional tools or certify a malware scan."
      />

      {error ? (
        <HonestState
          variant="not_wired"
          title="Skills are unavailable"
          detail="The run environment isn't reachable right now. Try again shortly."
        />
      ) : (
        <>
          {options
            ? <SkillUpload toolPolicies={options.tool_policies} />
            : <HonestState variant="not_wired" compact title="Skill authoring is unavailable"
                detail="Tool policies could not be loaded. Existing skills remain visible; retry after checking connectivity." />}

          <Section title="Approved skills" subtitle={`${usable.length} usable — independently reviewed and content-bound.`}>
            {usable.length === 0 ? (
              <HonestState
                variant="empty"
                compact
                title="No approved skills yet"
                detail="Upload a skill above; after a different operator approves its current content, it appears here ready to assign."
              />
            ) : (
              <ul className="divide-y divide-border">
                {usable.map((s) => (
                  <SkillRow key={s.name} s={s} />
                ))}
              </ul>
            )}
          </Section>

          <Section title="In review" subtitle={`${pending.length} awaiting the operator trust gate.`}>
            {pending.length === 0 ? (
              <HonestState variant="empty" compact title="Nothing in review" detail="Uploaded skills waiting on an operator show here." />
            ) : (
              <ul className="divide-y divide-border">
                {pending.map((s) => (
                  <SkillRow key={s.name} s={s} />
                ))}
              </ul>
            )}
          </Section>
        </>
      )}
    </div>
  );
}

function SkillRow({ s }: { s: SkillSummary }) {
  const badge = reviewBadge(s);
  const displayName = (s.spec?.displayName as string | undefined) ?? s.name;
  return (
    <li className="flex items-start justify-between gap-3 py-3">
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <p className="truncate text-sm font-medium">{displayName}</p>
          {s.version && <span className="rounded bg-surface-muted px-1.5 py-0.5 font-mono text-[10px] text-foreground-muted">v{s.version}</span>}
        </div>
        {s.summary && <p className="mt-0.5 truncate text-xs text-foreground-muted">{s.summary}</p>}
        <div className="mt-1 flex flex-wrap gap-1.5 text-[10px] text-foreground-muted">
          {s.bounding_policy && <span className="rounded border border-border bg-surface-muted px-1.5 py-0.5">bounded by {s.bounding_policy}</span>}
          {s.version_digest ? (
            <span className="rounded border border-border bg-surface-muted px-1.5 py-0.5 font-mono">validated {s.version_digest.slice(0, 19)}…</span>
          ) : (
            <span className="rounded border border-border bg-surface-muted px-1.5 py-0.5">not validated yet</span>
          )}
          {s.approved_by && <span className="rounded border border-border bg-surface-muted px-1.5 py-0.5">reviewed by {s.approved_by}</span>}
        </div>
        {!s.validated && s.validation_detail && <p className="mt-2 text-xs text-warning">{s.validation_detail}</p>}
      </div>
      <span className={`mt-0.5 shrink-0 whitespace-nowrap rounded-full border px-2.5 py-0.5 text-xs font-medium ${badge.cls}`}>{badge.label}</span>
    </li>
  );
}
