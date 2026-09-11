// kars Bridge Workspace — Teams list. Standing orgs that run continuously
// under a charter — distinct from Missions (finite task forces). Each Team's
// charter loop mints task-force work on a cadence (autonomous monitoring).

import Link from "next/link";
import { HonestState } from "@/components/honest-state";
import { listTeams } from "@/lib/bff";
import { defaultNamespace } from "@/lib/config";
import { type TeamSummary } from "@/lib/types";
import { TeamsList } from "./teams-list";

export const dynamic = "force-dynamic";

export default async function TeamsPage() {
  const ns = defaultNamespace();
  let teams: TeamSummary[] = [];
  let error = false;
  try {
    teams = await listTeams(ns);
  } catch {
    error = true;
  }

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

      {error ? (
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
