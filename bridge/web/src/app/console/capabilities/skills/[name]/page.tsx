// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import Link from "next/link";
import { BffError, getSkillPackage } from "@/lib/bff";
import { PageHeader, Section } from "@/components/ui";
import { HonestState } from "@/components/honest-state";
import { SkillApproval } from "../../../skill-approval";
import { LiveRefresh } from "@/components/live-refresh";

export const dynamic = "force-dynamic";

export default async function SkillReviewPage({ params }: { params: Promise<{ name: string }> }) {
  const { name } = await params;
  let review;
  try {
    review = await getSkillPackage(name);
  } catch (error) {
    return <HonestState variant="not_wired" title="Skill package is unavailable"
      detail={error instanceof BffError ? error.message : "The package could not be read. Try again after checking connectivity."} />;
  }
  const { skill, files } = review;
  return <div className="space-y-6">
    <LiveRefresh active={!skill.validated && skill.phase !== "Degraded"} />
    <Link href="/console/capabilities" className="text-sm text-signal underline">Back to capabilities</Link>
    <PageHeader eyebrow="Operator review" title={skill.name}
      lead="Inspect the instructions and every file before approving. A different operator must review a submitter's changes. Manual approval is not a malware scan or cryptographic attestation." />
    <Section title="Current package" subtitle={`Version ${skill.version ?? "unavailable"} · ${skill.phase ?? "awaiting validation"}`}>
      <p className="text-sm">{skill.summary}</p>
      <p className="mt-2 break-all font-mono text-xs text-foreground-muted">UID {skill.uid} · generation {skill.generation} · {skill.version_digest ?? "No validated digest yet"}</p>
      <p className="mt-2 text-xs text-foreground-muted">Bounded by {skill.bounding_policy}. {skill.validation_detail}</p>
      {typeof skill.spec.recipe === "string" && <pre className="mt-3 whitespace-pre-wrap break-words rounded-lg bg-surface-muted p-3 text-xs">{skill.spec.recipe}</pre>}
    </Section>
    <Section title="Package files" subtitle="Exact content-bound files. Relative subdirectories are preserved. Files do not grant additional tool authority.">
      {Object.keys(files).length === 0 ? <p className="text-sm text-foreground-muted">Recipe-only skill; no package files.</p>
        : Object.entries(files).map(([path, content]) => <details key={path} open={path === "SKILL.md"} className="mb-3 rounded-lg border border-border p-3">
          <summary className="cursor-pointer font-mono text-sm">{path} · {Buffer.byteLength(content, "utf8")} bytes</summary>
          <pre className="mt-3 max-h-96 overflow-auto whitespace-pre-wrap break-words rounded-lg bg-surface-muted p-3 text-xs">{content}</pre>
        </details>)}
    </Section>
    <SkillApproval skill={skill} inspected />
  </div>;
}
