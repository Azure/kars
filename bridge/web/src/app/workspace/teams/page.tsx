// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge Workspace — Teams list. Standing orgs that run continuously
// under a charter — distinct from Missions (finite task forces). Each Team's
// charter loop mints task-force work on a cadence (autonomous monitoring).

import Link from "@/components/navigation-link";
import { HonestState } from "@/components/honest-state";
import { listProfiles, listTeams } from "@/lib/bff";
import { defaultNamespace } from "@/lib/config";
import { TeamsList } from "./teams-list";

export const dynamic = "force-dynamic";

export default async function TeamsPage() {
  const ns = defaultNamespace();
  const [teamsResult, profilesResult] = await Promise.allSettled([
    listTeams(ns),
    listProfiles(),
  ]);
  const teams = teamsResult.status === "fulfilled" ? teamsResult.value : [];
  const profiles = profilesResult.status === "fulfilled"
    ? profilesResult.value.filter((profile) => profile.namespace === ns)
    : [];

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">Teams</h1>
          <p className="mt-1 text-sm text-foreground-muted">
            Standing teams that work continuously under a charter — watching a repo, an org, or a
            system, and acting on a schedule. Unlike a mission, a team doesn&apos;t finish.
          </p>
        </div>
        <Link href="/workspace/teams/new" className="shrink-0 rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg">+ New team</Link>
      </div>

      <section aria-labelledby="team-profiles-heading" className="space-y-3">
        <div>
          <h2 id="team-profiles-heading" className="text-lg font-semibold">Start from a team profile</h2>
          <p className="text-sm text-foreground-muted">Copy a validated charter, roster and authority envelope into an editable draft. Nothing launches until you review and approve it.</p>
        </div>
        {profilesResult.status === "rejected" ? (
          <HonestState variant="not_wired" title="Team profiles are unavailable" detail="The profile inventory could not be loaded. You can still create a team from a charter or use an existing team." />
        ) : profiles.length === 0 ? (
          <p className="text-sm text-foreground-muted">No team profiles are available in this workspace. You can still create a team from a charter.</p>
        ) : (
          <ul className="grid gap-3 md:grid-cols-2">
            {profiles.map((profile) => (
              <li key={profile.name} className="space-y-2 rounded-lg border border-border p-4">
                <h3 className="font-semibold">{profile.display_name ?? profile.name}</h3>
                {profile.charter_template && <p className="line-clamp-3 text-sm text-foreground-muted">{profile.charter_template}</p>}
                <p className="text-sm text-foreground-muted">{profile.roles.length} roles · {profile.phase ?? "Status unavailable"}</p>
                {profile.phase === "Ready" ? (
                  <Link href={`/workspace/teams/new?profile=${encodeURIComponent(profile.name)}`} className="inline-block text-sm font-semibold text-signal">
                    Use {profile.display_name ?? profile.name}
                  </Link>
                ) : (
                  <p className="text-sm text-foreground-muted">This profile needs controller validation before it can be used.</p>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>

      {teamsResult.status === "rejected" ? (
        <HonestState
          variant="not_wired"
          title="Teams are unavailable"
          detail="The run environment isn't reachable right now. Try again shortly."
        />
      ) : teams.length === 0 ? (
        <HonestState
          variant="empty"
          title="No standing teams yet"
          detail="A team is a durable org with a charter and a cadence — stand one up to watch a repo or an org continuously."
        />
      ) : (
        <TeamsList teams={teams} />
      )}
    </div>
  );
}
