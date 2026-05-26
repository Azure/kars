# Demo: The Lethal Trifecta, Defused

> *"Any time you grant an LLM-based system access to private data, exposure to
> untrusted content, and the ability to externally communicate, you have a
> nasty security hole."* — Simon Willison, [The Lethal Trifecta](https://simonwillison.net/2025/Jun/16/the-lethal-trifecta/)

This demo reproduces a **real, very recent** agentic-AI attack against two
identical agents on the same AKS cluster — one wrapped by AzureClaw, one
not — and shows exactly which AzureClaw layer catches the attack at every
step.

## The attack we reproduce

The chain is the **Claude Cowork file-exfiltration** pattern (PromptArmor,
[January 2026](https://www.promptarmor.com/resources/claude-cowork-exfiltrates-files),
building on Johann Rehberger's earlier [Claude.ai disclosure](https://embracethered.com/blog/posts/2025/claude-abusing-network-access-and-anthropic-api-for-data-exfiltration/)).
It's the same shape as the [Google Antigravity .env exfiltration](https://www.promptarmor.com/resources/google-antigravity-exfiltrates-data)
(Nov 2025) and [EchoLeak / M365 Copilot](https://nvd.nist.gov/vuln/detail/CVE-2025-32711)
(CVE-2025-32711, Jun 2025). All three exploit the lethal trifecta:

| Trifecta leg | In this demo |
|---|---|
| **Private data** | A "real-estate appraisals" memory store the agent can read |
| **Untrusted content** | A `.docx` "skill" the user uploads, with a 1-pt-font hidden injection |
| **Exfil channel** | `POST /v1/files` to a model provider — domain is on every reasonable allowlist |

The clever bit, the bit that **broke Anthropic's allowlist** in production:
the attacker provides their own API key inside the injection, so the
victim's agent uploads files to the **attacker's** account using a domain
the allowlist trusts. **Domain-only allowlists fail.** That's the lesson —
and it's the lesson this demo delivers.

## What the demo proves

| | Naked OpenClaw | OpenClaw on AzureClaw |
|---|---|---|
| Inline Content Safety prompt-shield | ❌ none | ✅ Foundry DefaultV2 → 403 |
| URL-method allowlist (not just domain) | ❌ domain-only | ✅ ToolPolicy: `GET` only on that host |
| Stripped attacker-controlled bearer | ❌ propagated | ✅ Router strips Authorization from agent context |
| Egress guard (UID-based iptables) | ❌ none | ✅ UID 1000 → `ECONNREFUSED` |
| Token budget cap | ❌ none | ✅ 100-token / request budget |
| AGT BehaviorMonitor auto-quarantine | ❌ none | ✅ 3 flags → rate-limit 0 |
| Tamper-evident audit chain | ❌ none | ✅ `azureclaw audit verify` |

**Six independent layers. Each one alone catches the attack.**

## Layout

```
examples/lethal-trifecta-demo/
├── README.md                          # this file
├── WALKTHROUGH.md                     # step-by-step demo script (timed)
├── bait/
│   └── poisoned-skill.md              # the injection payload (1pt-font equivalent)
├── scenarios/
│   ├── 00-namespaces.yaml             # the two namespaces
│   ├── 01-naked-claw.yaml             # vanilla OpenClaw agent (no AzureClaw)
│   ├── 02-azureclaw-sandbox.yaml      # ClawSandbox + InferencePolicy + ToolPolicy
│   └── 03-bait-server.yaml            # serves the poisoned skill on cluster-internal HTTP
└── scripts/
    ├── deploy.sh                      # apply everything in order
    ├── run-attack.sh                  # send the bait to both agents
    ├── verify-defense.sh              # check audit + Slack channel + namespace state
    └── teardown.sh                    # remove namespaces + watchers
```

## Prereqs

- An AzureClaw deployment via `azureclaw up` ([Getting Started](../../docs/getting-started.md))
- `kubectl` context pointing at that cluster
- One Foundry / Azure OpenAI deployment (the demo uses it for the `gpt-4.1` model;
  GitHub-Models mode also works but Layer 1 — inline Content Safety — won't fire,
  see [security.md](../../docs/security.md#what-we-do-not-defend-against))
- ~10 minutes

## Quick run

```bash
cd examples/lethal-trifecta-demo

./scripts/deploy.sh           # creates two namespaces, two agents, bait server
./scripts/run-attack.sh       # sends the bait prompt to both agents
./scripts/verify-defense.sh   # shows the audit trail of which layer caught it
./scripts/teardown.sh         # removes everything
```

> **What works out of the box** — `deploy.sh` and `teardown.sh` run end-to-end on
> local-k8s and AKS; both Deployments reach `Ready` and the AzureClaw sandbox
> stack (egress-guard, inference-router, NetworkPolicy, InferencePolicy) all
> reconcile.
>
> **What `run-attack.sh` additionally requires** —
> 1. The `naked-claw` container needs a working OpenClaw runtime config (mounted
>    or templated into `01-naked-claw.yaml`); without it the vanilla pod
>    crashloops at startup before it can attempt the attack.
> 2. The `azureclaw-realestate-agent` namespace is governed by the cluster-wide
>    `ValidatingAdmissionPolicy/azureclaw-sandbox-exec-ban` ValidatingAdmissionPolicy,
>    so `kubectl exec` into the `openclaw` container is denied by design. For
>    the demo's exec-driven attack path, label the namespace
>    `azureclaw.azure.com/break-glass=true` first — every bypass is audited.
>
> If either prerequisite isn't met, `deploy.sh` still demonstrates the
> deploy-time defense posture (network policy, egress guard, governance
> mounts), which is the bulk of the story.

The full timed walkthrough is in [`WALKTHROUGH.md`](WALKTHROUGH.md) — what to
say at each second mark for a recorded or live demo.

## Why this is honest

- **The exploit is real and reproducible** — PromptArmor's writeup gives
  step-by-step reproduction; the same shape is in our `bait/poisoned-skill.md`.
- **The naked-claw failure is not theatrical.** A standard OpenClaw deploy
  with a normal egress allowlist *will* fall to this exact attack. We aren't
  setting up a strawman.
- **No layer is hidden.** Every defense in the AzureClaw column is a YAML in
  `scenarios/` you can read, copy, and audit. There's no magic; it's policy.

## Credits

- [Simon Willison](https://simonwillison.net/) for naming the lethal trifecta
- [PromptArmor](https://www.promptarmor.com/) for the Claude Cowork & Antigravity disclosures
- [Johann Rehberger](https://embracethered.com/) for the original Claude.ai exfil disclosure
- [AIM Labs](https://www.aim.security/) for the EchoLeak (CVE-2025-32711) writeup

## Related reading in this repo

- [`docs/architecture.md`](../../docs/architecture.md) — inference data-path with both providers
- [`docs/security.md`](../../docs/security.md) — the nine layers + what we do *not* defend against
- [`docs/blueprints/02-enterprise-self-hosted.md`](../../docs/blueprints/02-enterprise-self-hosted.md) — how this maps to a real prod deploy
