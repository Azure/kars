// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { BffError } from "@/lib/bff";

export function missingTeamResource(error: unknown): null {
  if (error instanceof BffError && error.status === 404) return null;
  throw error;
}

export function TeamReadError({ error }: { error: unknown }) {
  const status = error instanceof BffError ? error.status : null;
  const title = status === 409
    ? "Team records need attention"
    : status === 401 ? "Sign in to view this team"
    : status === 403 ? "Team access denied"
    : "Team data could not be loaded";
  const detail = status === 409
    ? "The service refused inconsistent team or review records. This does not establish that the run or its evidence was deleted. Reload this page; if the conflict persists, ask an administrator to inspect it."
    : status === 401 ? "Your session could not be authenticated. Sign in again before viewing this team's records."
    : status === 403 ? "Your current account is not authorized to read these records."
    : "The service could not return the requested records. Reload this page to retry; missing activity or expired retention has not been established.";
  return (
    <section role="alert" className="rounded-xl border border-warning/40 bg-warning/[0.06] p-6">
      <h1 className="text-lg font-semibold">{title}</h1>
      <p className="mt-2 text-sm text-foreground-muted">{detail}</p>
      {status === 409 && error instanceof BffError && (
        <p className="mt-3 text-sm text-foreground">{error.message}</p>
      )}
    </section>
  );
}
