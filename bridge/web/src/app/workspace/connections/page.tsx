// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge Workspace — Connections. Where a USER connects their own GitHub
// repos so their agents can open pull requests (keyless). Operator-level App
// setup lives in the Console; each user's GitHub connection is isolated.

import { PageHeader, Section } from "@/components/ui";
import { ConnectGithub } from "@/components/connect-github";
import { ConnectChannels } from "@/components/connect-channels";
import { ConnectTeams } from "@/components/connect-teams";
import { defaultNamespace } from "@/lib/config";

export const dynamic = "force-dynamic";

export default function ConnectionsPage() {
  const ns = defaultNamespace();
  return (
    <div className="space-y-6">
      <PageHeader
        title="Connections"
        lead="Connect the tools and channels your agents work with. GitHub access is private to your signed-in user; messaging settings are shared across the workspace."
      />
      <Section
        title="GitHub"
        subtitle="Install the kars app on the repositories you want your agents to work on, then Connect. When a mission needs to push, the router mints a short-lived, repo-scoped token and injects it — your agents never see a token, and you can Disconnect to remove your grant."
      >
        <ConnectGithub ns={ns} />
      </Section>
      <Section
        title="Channels"
        subtitle="Set up channels for the workspace. Telegram supports proactive updates when allowed chat IDs are provided. Slack, Discord and WhatsApp support inbound conversation, not proactive reports. Saving settings does not verify delivery."
      >
        <ConnectChannels ns={ns} />
      </Section>
      <Section
        title="Microsoft Teams"
        subtitle="Connect a Microsoft Teams channel for HITL approval cards and proactive updates. Requires an Entra App Registration with Bot enabled and admin consent. Credentials are write-only."
      >
        <ConnectTeams ns={ns} />
      </Section>
    </div>
  );
}
