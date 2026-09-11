// kars Bridge — New team. Server shell that loads the real cluster building
// blocks (models, harnesses) so the org-chart composer can offer per-role
// harness + model choices. When ?profile=<name> is present, the team is
// instantiated from that vetted KarsProfile (charter + roster + envelope
// prefilled). The composition itself happens client-side.

import { getOptions, listProfiles } from "@/lib/bff";
import { defaultNamespace } from "@/lib/config";
import type { Options, ProfileSummary } from "@/lib/types";
import { HonestState } from "@/components/honest-state";
import { TeamComposer } from "./team-composer";

export const dynamic = "force-dynamic";

export default async function NewTeamPage({
  searchParams,
}: {
  searchParams: Promise<{ profile?: string; intent?: string }>;
}) {
  const { profile: profileName, intent } = await searchParams;
  let options: Options | null = null;
  let optionsError = false;
  try {
    options = await getOptions();
    const namespace = defaultNamespace();
    options = {
      ...options,
      mcp_servers: options.mcp_servers.filter((server) => server.namespace === namespace),
      tool_policies: options.tool_policies.filter((policy) => policy.namespace === namespace),
      memories: options.memories.filter((memory) => memory.namespace === namespace),
      skills: options.skills.filter((skill) => skill.namespace === namespace),
    };
  } catch {
    optionsError = true;
  }
  let profile: ProfileSummary | null = null;
  if (profileName) {
    try {
      const all = await listProfiles();
      profile = all.find((p) => p.name === profileName) ?? null;
    } catch {
      profile = null;
    }
  }
  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Set up a team</h1>
        <p className="mt-1 text-sm text-foreground-muted">
          {profile
            ? `Instantiating from the “${profile.display_name ?? profile.name}” profile — its charter, roles, and access are prefilled. Edit anything before you create.`
            : "A standing org that works continuously under a charter. Describe the mandate; kars proposes an editable org chart — each role can run its own harness and model."}
        </p>
      </div>
      {optionsError || !options ? (
        <HonestState
          variant="not_wired"
          title="Can’t set up a team right now"
          detail="The run environment isn’t reachable, so the available models and harnesses couldn’t be loaded. Try again shortly."
        />
      ) : (
        <TeamComposer options={options} profile={profile} initialCharter={intent} />
      )}
    </div>
  );
}
