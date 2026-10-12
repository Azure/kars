// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import { DeliverableBody } from "@/components/deliverable-view";
import type { RetainedTeamDocument, TeamRunArchive } from "@/lib/types";

function downloadRetainedDocument(file: RetainedTeamDocument) {
  const url = URL.createObjectURL(new Blob([file.content], { type: "text/plain;charset=utf-8" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = file.name;
  document.body.appendChild(link);
  link.click();
  link.remove();
  URL.revokeObjectURL(url);
}

export function RetainedTeamDocuments({ archive }: { archive: TeamRunArchive }) {
  return (
    <div className="space-y-4">
      {[
        { title: "Retained documents", files: archive.documents },
        { title: "Peer handoffs", files: archive.handoffs },
      ].map(({ title, files }) => (
        <section key={title}>
          <h3 className="text-sm font-semibold">{title}</h3>
          <ul className="mt-2 space-y-2">
            {files.map((file) => (
              <li key={file.name}>
                <details className="rounded-lg border border-border bg-surface">
                  <summary className="cursor-pointer p-3 text-sm">
                    {file.source.artifactName}
                    <span className="ml-2 text-xs text-foreground-muted">{file.source.taskName}</span>
                  </summary>
                  <div className="space-y-3 border-t border-border p-3">
                    <div className="flex flex-wrap items-center justify-between gap-2">
                      <code className="break-all text-xs text-foreground-muted">{file.sha256}</code>
                      <button type="button" onClick={() => downloadRetainedDocument(file)}
                        className="rounded-lg border border-border px-3 py-1.5 text-xs font-medium hover:bg-surface-muted">
                        Download {file.name}
                      </button>
                    </div>
                    <DeliverableBody output={file.content} />
                    <p className="break-all text-xs text-foreground-muted">
                      Task UID: {file.source.taskUid} · Run: {file.source.runNonce}
                      {" · "}Assignment: {file.source.assignmentId} · Agent: {file.source.agentDid}
                    </p>
                  </div>
                </details>
              </li>
            ))}
          </ul>
        </section>
      ))}
    </div>
  );
}
