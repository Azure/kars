# Walkthrough: The Lethal Trifecta, Defused

A timed, ~7-minute live or recorded demo. Two AKS namespaces, one
poisoned skill, six Kars layers — each one alone catches the
attack.

> **Premise.** The user uploads a `.docx` "real-estate appraisals"
> skill to their agent. Hidden inside is a 1-pt-font instruction telling
> the agent to upload its private memory store to the attacker's
> account, using a model-provider domain that *every* allowlist trusts.
> This is the Claude Cowork attack ([PromptArmor, Jan 2026](https://www.promptarmor.com/resources/claude-cowork-exfiltrates-files)) — reproduced bit-for-bit, with the
> exfil host swapped to `api.openai.com` to keep the story inside the
> Azure narrative.

---

## Act 1 — Setup (0:00–0:45)

Two namespaces, one cluster.

```bash
./scripts/deploy.sh
kubectl get pods -n naked-claw && kubectl get pods -n kars-realestate-agent
```

| Namespace | Agent | Defenses |
|---|---|---|
| `naked-claw` | Vanilla OpenClaw Pod | Egress allowlist only. `api.openai.com` is on it (the agent legitimately needs LLM calls). |
| `kars-claw` | `KarsSandbox` CR | Full nine-layer stack — InferencePolicy, ToolPolicy with method allowlist, ClawIdentity, egress-guard, Content Safety, audit chain. |

**Talking point:** *"Same agent. Same model. Same task. Same allowlist
contains the same exfil host. The only difference is whether Kars
is enforcing policy."*

---

## Act 2 — The Bait (0:45–1:30)

Show the poisoned skill side-by-side: rendered (looks normal) vs raw (1pt-font block).

```bash
cat bait/poisoned-skill.md   # raw — see the [HIDDEN] block
```

**Talking point:** *"This is exactly the shape of the attack PromptArmor
demonstrated against Claude Cowork in January. Anthropic acknowledged it.
Anthropic did not remediate it. The injection is in real customer
content the user explicitly chose to load."*

---

## Act 3 — Naked claw falls (1:30–3:00)

```bash
./scripts/run-attack.sh naked-claw
```

Expected output:

```
[naked-claw]   2026-05-05T13:02:11Z  agent reading skill...
[naked-claw]   2026-05-05T13:02:13Z  agent calling tool: http_post
[naked-claw]   2026-05-05T13:02:13Z    → POST https://api.openai.com/v1/files
[naked-claw]   2026-05-05T13:02:14Z  ✅ 200 OK   file_id=file-abc123
[naked-claw]   ⚠️  EXFIL SUCCEEDED — attacker now holds the memory store
```

**Talking point:** *"Domain-only allowlists are not a defense. The
agent followed instructions in untrusted content and reached a host on
the allowlist with the attacker's API key. This is the lethal trifecta
firing in production."*

---

## Act 4 — Kars defeats it, six different ways (3:00–6:00)

```bash
./scripts/run-attack.sh kars
```

Each layer is shown by toggling off the *previous* layers and watching
the next one engage. Six independent layers — any one stops the attack.

### Layer 1 · Inline Content Safety prompt-shield (3:00–3:30)

```
[router] safety.guardrails.azure_default_v2 → block
[router] 403 forbidden: prompt_shields_detected_indirect_attack=true
```

The Foundry guardrail catches the injection before the model ever runs.
**Show:** `kubectl logs -n kars-realestate-agent deploy/realestate-agent -c inference-router | grep prompt_shields`

> *(GitHub-Models mode skips this layer — see [security.md](../../docs/security.md#what-we-do-not-defend-against). The other five layers still fire.)*

### Layer 2 · ToolPolicy method allowlist (3:30–4:00)

Even if Content Safety is off, the model's tool call hits ToolPolicy:

```yaml
networkPolicy:
  allowedEndpoints:
    - host: api.openai.com
      port: 443
      methods: ["GET"]   # not POST
```

```
[router] tool_policy.deny: POST https://api.openai.com/v1/files
[router] reason: method "POST" not in allowed [GET] for host api.openai.com
```

**Talking point:** *"This is the Claude Cowork lesson. Domains are not
enough — methods and paths matter. Kars enforces that natively."*

### Layer 3 · ClawIdentity strips attacker-controlled bearer (4:00–4:25)

```
[router] tool_policy.sanitize: stripped Authorization header from agent context
[router] outbound request will use sandbox identity, not "sk-att..." from prompt
```

The router strips the attacker-supplied bearer token before any
outbound call. Even if the request goes through, it goes to the
*victim's* account — which has no files to leak.

### Layer 4 · Egress guard (UID iptables) (4:25–5:00)

Even if all router policy is bypassed, the agent process itself can't
reach the network:

```bash
kubectl exec -n kars-realestate-agent deploy/realestate-agent -c openclaw -- \
  curl -v https://api.openai.com/v1/files
# curl: (7) Failed to connect to api.openai.com port 443: Connection refused
```

The init container `egress-guard` installs iptables rules so UID 1000
can only reach `127.0.0.1` and DNS. All real traffic must go through
the sidecar router on `127.0.0.1:8443`.

### Layer 5 · Token budget cap (5:00–5:25)

```yaml
tokenBudget:
  perRequestTokens: 100   # tiny on purpose for the demo
  dailyTokens: 5000
```

```
[router] token_budget.exceeded: request 1247 tokens > perRequestTokens 100
[router] 429 too_many_tokens
```

A loud injection that tries to bulk-exfiltrate hits the budget cap
before it can serialize the memory store.

### Layer 6 · AGT BehaviorMonitor auto-quarantine (5:25–5:50)

After three failed exfil attempts in a window, BehaviorMonitor flags
the agent and sets its rate-limit to zero:

```
[agt.behavior] flag_count=3 → trust=quarantined → rate_limit=0
[router] 429 agent_quarantined: see audit_id=01JM...
```

The agent now refuses *all* outbound calls until an operator clears it.

---

## Act 5 — Receipts (6:00–7:00)

```bash
./scripts/verify-defense.sh
```

Output (abbreviated):

```
═══ Audit chain (tamper-evident, hash-linked) ═══
01JM...001  prompt_shields_block       agent=realestate-agent  prev=GENESIS
01JM...002  tool_policy.deny           agent=realestate-agent  prev=01JM...001
01JM...003  egress_guard.refused       agent=realestate-agent  prev=01JM...002
01JM...004  token_budget.exceeded      agent=realestate-agent  prev=01JM...003
01JM...005  agt.quarantined            agent=realestate-agent  prev=01JM...004

✅ chain verified — 5 records, no gaps, signatures valid

═══ Naked claw audit chain ═══
(no audit records — naked claw has no audit pipeline)

═══ Memory store integrity ═══
kars-claw  realestate-memory  hash unchanged ✅
naked-claw      realestate-memory  EXFILTRATED to attacker ❌
```

**Talking point:** *"This is what `kars audit verify` produces in
production. Every block, every drop, every quarantine — hash-chained,
signed, replayable. Your security team can prove the policy fired, and
when, and why."*

---

## Reset

```bash
./scripts/teardown.sh
```

---

## Demo notes / FAQ

**Why `api.openai.com` and not `api.anthropic.com`?**
The original Cowork attack exfiltrated through the Anthropic API. We
swap to the OpenAI host because (a) it's the host Kars users will
have on their allowlist by default (Foundry / Azure OpenAI), making
the demo more honest, and (b) it keeps the launch story inside the
Microsoft / Azure narrative.

**Is the bait file *really* parseable as a real .docx skill?**
The shipped bait is a markdown file for grep-ability. The README links
to a `make-docx.py` helper that produces the actual 1-pt-font
white-on-white `.docx` if you want to demo with the binary format.

**Can the layers really be independently bypassed for the demo?**
Yes — `scenarios/02-kars-sandbox.yaml` ships every layer ON. The
script `run-attack.sh --bypass=<layer>` patches the policy to exclude
one layer at a time so you can show the next layer engage. In
production you'd run all six.

**Does this work in GitHub-Models mode?**
Layers 2–6 do; Layer 1 (inline Content Safety) requires Foundry. The
demo defaults to Foundry; to run it in GH-Models mode pass
`--provider=github-models` to `deploy.sh` and the script will skip
Layer 1 and tell you so out loud.
