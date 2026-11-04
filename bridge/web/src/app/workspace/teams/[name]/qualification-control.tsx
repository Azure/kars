// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import type { TeamDetail } from "@/lib/types";
import { teamQualificationAction } from "./qualification-actions";

export function QualificationControl({ team, canQualify }: { team: TeamDetail; canQualify: boolean }) {
  const router = useRouter();
  const [pending, startTransition] = useTransition();
  const [error, setError] = useState<string | null>(null);
  const [confirmedReview, setConfirmedReview] = useState<string | null>(null);
  const qualification = team.qualification;
  if (!qualification) return null;
  const expected = team.tasks.some((task) => task.status === "active" || task.status === "awaiting_review") ? undefined : team.tasks.find((task) => task.status === "pending" && task.depends_on.every((id) => team.tasks.some((dependency) => dependency.id === id && ["done", "retired"].includes(dependency.status))));
  const reviewKey = JSON.stringify({ team, expected });
  const confirmed = confirmedReview === reviewKey;
  const setConfirmed = (value: boolean) => setConfirmedReview(value ? reviewKey : null);
  const submit = (action: "attempt" | "pause" | "review") => startTransition(async () => {
    setError(null);
    const result = await teamQualificationAction(team.name, team.uid, action, expected);
    setError(result.error);
    setConfirmed(false);
    router.refresh();
  });
  return <section aria-label="Team qualification" className="rounded-xl border border-border bg-surface p-5 space-y-3">
    <h2 className="text-sm font-semibold">Unqualified Team — {qualification.state.replaceAll("_", " ")}</h2>
    <p className="text-xs text-foreground-muted">{qualification.note}</p>
    {qualification.expires_at && <p className="text-xs text-foreground-muted">Admission approval expires: {qualification.expires_at}</p>}
    {qualification.state === "pending" && expected && <div className="text-sm space-y-2">
      <p className="font-medium">Reviewed next item: {expected.id} — {expected.title}</p>
      <p className="whitespace-pre-wrap">{expected.description}</p>
      <ul className="list-disc pl-5">{expected.acceptance_criteria.map((criterion, index) => <li key={index}>{criterion}</li>)}</ul>
      {canQualify && <label className="block text-xs"><input type="checkbox" checked={confirmed} onChange={(event) => setConfirmed(event.target.checked)} className="mr-2 accent-signal" />I reviewed this exact item and the Team’s routes, resources and finite budget below. Authorize one attempt only.</label>}
      {canQualify && <button type="button" disabled={!confirmed || pending} onClick={() => submit("attempt")} className="rounded-lg bg-signal px-3 py-2 text-xs font-medium text-signal-fg disabled:opacity-50">Approve one attempt, then pause</button>}
    </div>}
    {qualification.state === "blocked" && <p className="text-xs">Approval is expired or incomplete. Create a freshly reviewed Team; ordinary Run cannot bypass this gate.</p>}
    {canQualify && !team.paused && <button type="button" disabled={pending} onClick={() => submit("pause")} className="rounded-lg border border-border px-3 py-2 text-xs">Pause qualification attempt</button>}
    {canQualify && qualification.state === "awaiting_evidence" && <div className="space-y-2 text-xs">
      <p>Completion alone is not qualification. After recording independently reviewed route/resource evidence, explicitly release this package to ordinary admission. This does not resume the Team.</p>
      <label className="block"><input type="checkbox" checked={confirmed} onChange={(event) => setConfirmed(event.target.checked)} className="mr-2 accent-signal" />I reviewed the evidence for this exact Team package.</label>
      <button type="button" disabled={!confirmed || pending} onClick={() => submit("review")} className="rounded-lg border border-border px-3 py-2 disabled:opacity-50">Check qualified evidence and release — remain paused</button>
    </div>}
    {error && <p role="alert" className="text-xs text-danger">{error}</p>}
  </section>;
}
