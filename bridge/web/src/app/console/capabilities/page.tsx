// kars Bridge Operator Console — Agent capabilities. What teams/agents may be
// granted: versioned skills, ready-made team profiles, MCP services (bounded
// by a tool policy), and per-agent runtime credentials. Split out of the
// Configuration page (which is CLUSTER infrastructure — provider, Foundry,
// GitHub App) so the two layers get their own place in the nav instead of one
// long scroll: what this cluster runs on vs. what your teams may use.

import { listMcpServers, listProfiles, listSkills, getOptions } from "@/lib/bff";
import { PageHeader, Section, Badge } from "@/components/ui";
import { HonestState } from "@/components/honest-state";
import { CredentialForm } from "../configuration/credential-form";
import { AuthorResource } from "../author-resource";
import { ProfileEditor } from "../profile-editor";
import { DeleteResource } from "../delete-resource";
import { SkillApproval } from "../skill-approval";
import { SkillComposer } from "@/components/skill-composer";
import { submitSkillConsoleAction } from "../skill-submit-action";
import { McpProfiles } from "../mcp-profiles";
import { McpCatalog } from "../mcp-catalog";
import { McpServerEditor } from "../mcp-server-editor";
import { Icon } from "@/components/icon";
import type { McpServer, Options, ProfileSummary, SkillSummary } from "@/lib/types";

export const dynamic = "force-dynamic";

export default async function CapabilitiesPage() {
  let options: Options | null = null;
  let skills: SkillSummary[] = [];
  let profiles: ProfileSummary[] = [];
  let mcp: McpServer[] = [];
  try {
    [options, skills, profiles, mcp] = await Promise.all([
      getOptions(),
      listSkills().catch(() => []),
      listProfiles().catch(() => []),
      listMcpServers().catch(() => []),
    ]);
  } catch {
    options = null;
  }

  return (
    <div className="space-y-6">
      <PageHeader
        eyebrow="Operator Console"
        title="Agent capabilities"
        lead="Vetted building blocks teams pick from — skills, ready-made team profiles, MCP services, and runtime credentials. The cluster's inference provider and Foundry connection live under Configuration."
      />

      <Section
        title="Skills"
        subtitle="Versioned capability bundles a team can acquire — the same guided upload used in the Workspace; attested + operator-approved before use."
      >
        <div className="mb-3">
          <SkillComposer toolPolicies={options?.tool_policies ?? []} submit={submitSkillConsoleAction} />
        </div>
        {skills.length === 0 ? (
          <HonestState variant="empty" compact title="No skills yet" detail="Register a KarsSkill to offer it to teams." />
        ) : (
          <ul className="grid gap-2 sm:grid-cols-2">
            {skills.map((s) => (
              <li
                key={s.name}
                className="rounded-lg border border-border bg-surface px-3 py-2.5 text-sm"
              >
                <div className="flex items-center justify-between">
                  <span className="flex items-center gap-1.5">
                    <Icon name="bolt" size={13} />
                    <span className="font-medium">{s.name}</span>
                    {s.version && <span className="text-xs text-foreground-muted">v{s.version}</span>}
                  </span>
                  <Badge tone={s.attestation_verified === true ? "ok" : s.attestation_verified === false ? "warn" : "muted"}>
                    {s.attestation_verified === true
                      ? "attested"
                      : s.attestation_verified === false
                        ? "unverified"
                        : s.phase ?? "attestation unknown"}
                  </Badge>
                </div>
                <div className="mt-2 flex flex-wrap items-center gap-2">
                  <SkillApproval skill={s} />
                  <span className="h-4 w-px bg-border" aria-hidden />
                  <AuthorResource kind="KarsSkill" initialName={s.name} initialSpec={JSON.stringify(s.spec, null, 2)} />
                  <DeleteResource kind="KarsSkill" name={s.name} label="skill" />
                </div>
              </li>
            ))}
          </ul>
        )}
      </Section>

      <Section
        title="Team profiles"
        subtitle="Vetted org templates per domain — instantiate a whole standing team in one click."
        action={<ProfileEditor toolPolicies={options?.tool_policies ?? []} skills={options?.skills ?? []} />}
      >
        {profiles.length === 0 ? (
          <HonestState variant="empty" compact title="No profiles yet" detail="Publish a profile to offer a ready-made team, then instantiate it from the Teams page." />
        ) : (
          <ul className="grid gap-2 sm:grid-cols-2">
            {profiles.map((p) => (
              <li
                key={p.name}
                className="rounded-lg border border-border bg-surface px-3 py-2.5 text-sm"
              >
                <div className="flex items-center justify-between gap-2">
                  <span className="min-w-0">
                    <span className="font-medium">{p.display_name ?? p.name}</span>
                    <span className="ml-2 text-xs text-foreground-muted">
                      {p.roles.length} role{p.roles.length === 1 ? "" : "s"}
                      {p.tier != null ? ` · Tier ${p.tier}` : ""}
                    </span>
                  </span>
                  <span className="flex shrink-0 items-center gap-2 text-xs text-foreground-muted">
                    {p.domain && <Badge tone="muted">{p.domain}</Badge>}
                    {p.phase ?? "ready"}
                  </span>
                </div>
                {p.charter_template && (
                  <p className="mt-1 line-clamp-2 text-xs text-foreground-muted">{p.charter_template}</p>
                )}
                <div className="mt-2 flex flex-wrap items-center gap-3">
                  <ProfileEditor initialName={p.name} initialSpec={p.spec} toolPolicies={options?.tool_policies ?? []} skills={options?.skills ?? []} />
                  <DeleteResource kind="KarsProfile" name={p.name} label="team profile" />
                </div>
              </li>
            ))}
          </ul>
        )}
      </Section>

      <Section
        title="Internal-system integrations"
        subtitle="Services agents can be granted — docs stores, repos, Foundry tools — reached over MCP and bounded by a tool policy."
        action={<McpServerEditor />}
      >
        {/* Managed and external modes are intentionally distinct: "installed"
            means the controller owns a real workload; "registered" means the
            operator supplied an already-running endpoint. */}
        <details className="group mb-3 rounded-lg border border-border bg-surface-muted/30 p-3 text-xs">
          <summary className="flex cursor-pointer items-center justify-between font-medium">
            What happens when you add an MCP server?
            <span className="text-foreground-muted transition group-open:rotate-180" aria-hidden>⌄</span>
          </summary>
          <ol className="mt-2 space-y-1.5 text-foreground-muted">
            <li><span className="font-medium text-foreground">1. Installed or registered</span> — a managed preset creates a real Deployment + Service; an external entry records the real endpoint you already operate.</li>
            <li><span className="font-medium text-foreground">2. Probed</span> — managed servers must complete <code className="font-mono">initialize → tools/list</code>; their discovered tool names and schema digest are recorded before Ready.</li>
            <li><span className="font-medium text-foreground">3. Offered</span> — Ready servers appear for missions/teams, scoped by <code className="font-mono">allowedSandboxes</code> and bounded by the team&rsquo;s tool policy.</li>
            <li><span className="font-medium text-foreground">4. Brokered</span> — the sandbox router namespaces and governs every tool call, then forwards it. The agent gets neither the upstream endpoint nor its credentials.</li>
          </ol>
        </details>
        <div className="mb-3"><McpCatalog /></div>
        {mcp.length === 0 ? (
          <HonestState
            variant="empty"
            compact
            title="No services connected"
            detail="Pick one from the catalog above, or register a custom MCP server."
          />
        ) : (
          <ul className="space-y-2">
            {mcp.map((m) => (
              <li
                key={m.name}
                className="rounded-lg border border-border bg-surface px-3 py-2.5 text-sm"
              >
                <div className="flex items-center justify-between gap-3">
                  <span className="flex items-center gap-1.5">
                    <Icon name="plug" size={13} />
                    <span className="font-medium">{m.name}</span>
                  </span>
                  <div className="flex items-center gap-2">
                    <Badge tone={m.phase === "Ready" ? "ok" : m.phase === "Degraded" ? "danger" : "warn"} dot>
                      {m.phase ?? "Pending"}
                    </Badge>
                    <span className="rounded-full border border-border px-1.5 py-0.5 text-[10px] text-foreground-muted">
                      {m.mode ?? (m.spec.managed ? "Managed" : "External")}
                    </span>
                  </div>
                </div>
                <p className="mt-1 truncate font-mono text-xs text-foreground-muted">
                  {m.workload_ref ? `workload ${m.workload_ref}` : (m.endpoint ?? m.url ?? "endpoint pending")}
                </p>
                {m.discovered_tools.length > 0 && (
                  <p className="mt-1 text-[11px] text-foreground-muted">
                    {m.discovered_tools.length} tools verified
                    {m.tool_schema_digest ? ` · ${m.tool_schema_digest.slice(0, 19)}…` : ""}
                  </p>
                )}
                <div className="mt-2 flex items-center gap-3">
                  <McpServerEditor initialName={m.name} initialSpec={m.spec} />
                  <DeleteResource kind="McpServer" name={m.name} label="MCP server" />
                </div>
              </li>
            ))}
          </ul>
        )}
        {/* Operator-curated MCP profiles — vetted bundles users pick as a set. */}
        <div className="mt-5 border-t border-border pt-4">
          <h3 className="text-sm font-semibold">MCP profiles</h3>
          <p className="mt-0.5 text-xs text-foreground-muted">Named, vetted bundles of the servers above. Users add a whole bundle in one click; a profile can only reference registered servers.</p>
          <div className="mt-3">
            <McpProfiles profiles={options?.mcp_profiles ?? []} servers={options?.mcp_servers ?? []} />
          </div>
        </div>
      </Section>

      <Section
        title="Secure credentials"
        subtitle="Give an agent or team a repo or service token without baking it into an image. It is stored as a write-only secret that only that agent can read at runtime — never visible to the model or the UI."
      >
        {/* Make the security model explicit — the operator asked how these work. */}
        <ul className="mb-4 grid gap-2 sm:grid-cols-2">
          <li className="flex items-start gap-2 rounded-lg border border-emerald-500/25 bg-emerald-500/[0.04] p-3 text-xs">
            <Icon name="lock" size={14} />
            <span><span className="font-medium text-foreground">Write-only.</span> You set the value once; the API and this screen never return it again — there is no read path for the plaintext.</span>
          </li>
          <li className="flex items-start gap-2 rounded-lg border border-emerald-500/25 bg-emerald-500/[0.04] p-3 text-xs">
            <Icon name="box" size={14} />
            <span><span className="font-medium text-foreground">Stored as a K8s Secret</span> (<code className="font-mono">&lt;name&gt;-credentials</code>) in that agent&rsquo;s own namespace — not in the image, not in the CRD, not in git.</span>
          </li>
          <li className="flex items-start gap-2 rounded-lg border border-emerald-500/25 bg-emerald-500/[0.04] p-3 text-xs">
            <Icon name="cross" size={14} />
            <span><span className="font-medium text-foreground">Never seen by the model.</span> It&rsquo;s injected into the agent container&rsquo;s env at runtime only; the LLM sees tool results, never the token.</span>
          </li>
          <li className="flex items-start gap-2 rounded-lg border border-emerald-500/25 bg-emerald-500/[0.04] p-3 text-xs">
            <Icon name="target" size={14} />
            <span><span className="font-medium text-foreground">Scoped to one agent/team.</span> Only the sandbox it&rsquo;s bound to can mount it; other namespaces&rsquo; pods cannot read it.</span>
          </li>
        </ul>
        <CredentialForm />
      </Section>
    </div>
  );
}
