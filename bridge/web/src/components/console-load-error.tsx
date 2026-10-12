// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { BffError } from "@/lib/bff";
import { PageHeader } from "@/components/ui";

export function ConsoleLoadError({ title, error, detail, inline = false }: {
  title: string;
  error: unknown;
  detail?: string;
  inline?: boolean;
}) {
  const code = error instanceof BffError ? error.code : "connection_failed";
  return (
    <div className="space-y-6">
      {!inline && <PageHeader eyebrow="Operator Console" title={title} />}
      <div role="alert" className="rounded-xl border border-danger/40 bg-danger/10 p-5 text-sm">
        <p className="font-semibold">Could not load {title.toLowerCase()} ({code}).</p>
        <p className="mt-2 text-foreground-muted">
          {detail ?? "The current configuration could not be verified. This does not mean nothing is configured. Changes are unavailable until the read succeeds."}
        </p>
        {code === "credential_authority_unavailable" && (
          <p className="mt-2 text-foreground-muted">
            An operator must enroll or repair the integration store using the reviewed credential grant.
            Existing credentials and access restrictions have not been changed.
          </p>
        )}
        <p className="mt-2 text-foreground-muted">
          Refresh this page to retry, or inspect{" "}
          <a href="/console/troubleshooting" className="text-signal underline">Troubleshooting</a>.
        </p>
      </div>
    </div>
  );
}
