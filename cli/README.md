<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# kars CLI — `@kars-runtime/cli`

The command-line interface for **[kars](https://github.com/Azure/kars)** — a
secure AI agent runtime on Azure. Run OpenClaw (and other) agents in isolated
sandboxes with end-to-end encrypted inter-agent messaging, egress control, and
policy governance — locally on Docker/kind or on AKS.

## Install

```bash
npm i -g @kars-runtime/cli
```

This installs the `kars` command. (Node.js 22+ required.)

> No-compile alternative — the public installer pulls the latest signed release:
> ```bash
> curl -fsSL https://raw.githubusercontent.com/Azure/kars/main/install.sh | bash
> ```

## Quick start

```bash
# Run a sandboxed agent from the published, cosign-signed images (no compile)
kars dev --release

# …or on a local Kubernetes (kind) cluster — real K8s posture
kars dev --release --target local-k8s
```

On first launch you pick an inference provider — **GitHub Copilot** is easiest
(one device-code login, no Azure account). Then talk to your agent:

```bash
kars connect dev-agent
```

When you're ready for a managed cluster:

```bash
kars up --name prod-agent --region swedencentral --release   # provisions AKS + Foundry + the full stack from signed public images (no build)
```

## Browser-guided workspace setup

```bash
kars up --setup-browser
```

This standalone option opens a private, temporary loopback concierge instead of
running Azure provisioning. Choose the destination, components, inference and first
sign-in account, run preflight, then explicitly approve the exact target and host
trust changes. Do not combine it with other `up` flags or share its private link.
The link is single-use; the authenticated browser can reconnect while the
installer remains open. Links from another application can open the landing page
directly; embedded pages and cross-site API requests remain blocked. Credentials
are never stored in browser storage.

Current implemented installation paths target **fresh Kind on Podman**, with
Core and optional Bridge. Both use packaged release assets, the maintained Helm
installation and real local Dex sign-in—not a separate demo stack. Core-only
retains Headlamp with a read-only dashboard account; it creates no Bridge release,
Bridge client or administrator. Foundry and Azure OpenAI retain the existing
operator credential bootstrap. Other providers use governed enrollment: Core-only
creates a dedicated, UID-bound installer ServiceAccount instead of a Bridge writer,
and publishes credentials with a short-lived, provider-Secret-bound API token.
The temporary private kubeconfig is removed after publication or failure.
These paths still require fresh-install and first-login acceptance; local tests
and HTTP readiness are not that evidence.
Prerequisites include a running local Podman connection, Kind 0.20+, kubectl,
Helm 4.1.3, OpenSSL and supported host trust tooling. Local ports default to
8443 (sign-in HTTPS), 4466 (Headlamp), and 3000 (optional Bridge). Expand **Local
ports** to choose unused, distinct ports from 1024–65535; all bindings stay
loopback-only. Privileged port 443 cannot be published by macOS Podman.
Cluster names are generated for each browser setup and remain editable.
When host access needs sudo authorization, setup prompts in its launching terminal
and shows **Action required in Terminal** in the browser. A sticky alert stays
visible even when scrolled to cleanup, the browser tab title identifies the
required action, and instructions are revealed once when a new prompt arrives.
They explain where to enter the computer login password, that typing is invisible,
and that Enter submits it. Polling does not repeatedly move focus or scroll.
Installation/check spinners pause while awaiting authorization; cleanup and check
buttons explicitly say they are waiting for you. Approval clears the alert and resumes
the same check automatically without a retry or new provider sign-in. The browser
never accepts an OS password. Preflight does not install missing tools or change
host trust; installation still requires the reviewed consent.

The first screen has an interactive **Check prerequisites & ports** action, before
provider sign-in or account entry. It attempts to bind each selected active port on
`127.0.0.1`, then releases it, showing available or blocked per service. Core-only
skips the Bridge port. An unavailable port blocks Continue and opens Local ports
for correction; changing ports or components requires a new check. No process is
stopped and no port is reserved. The shared installer checks availability again
before creating resources. This does not prove Podman forwarding or HTTPS readiness.
The same action shows live tool/version checks, selected local Podman service
availability, packaged assets and host support, with terminal recovery instructions
and retry. It does not start Podman or install host tools.
It verifies that the digest-pinned Core schema image's registry index includes
the selected Podman machine's Linux architecture before authorization or
installation. The Kind installer rechecks the actual node architecture and
selects it in the schema-hook overlay; a chart's default amd64 selector is not
proof that a multi-platform image lacks arm64 support. Image digests, node labels,
isolation and production defaults remain unchanged. Unavailable platform metadata
or an unsupported image blocks installation rather than assuming compatibility.
The concierge keeps a destination/component summary alongside the setup journey,
with keyboard-accessible provider choices and a persistent next-step action area.
Switching providers clears the prior key/model and invalidates the reviewed setup.
The concierge uses directional step transitions and a live installation timeline.
Check rows keep their identity between polls, so working indicators do not restart
and completed stages do not flicker. The progress bar and stage counts describe completed checks, not
estimated time or a simulated percentage. **Pause motion** disables animation;
the OS reduced-motion preference is honored automatically. Hidden tabs pause
motion, and a lost installer connection stops the working indicators.
During Core and subsequent Helm waits, **Live workloads & Helm hooks** refreshes
bounded, read-only Kubernetes observations every three seconds: hook deadlines,
job results, pod readiness/restarts, deployment rollout counts and known scheduling
or image-pull blockers. Core substeps distinguish Helm from SRE rollout and
registration waits. No logs, credentials, pod specs or raw event messages are
shown. Unavailable refreshes are explicit; failures retain the last snapshot with
its timestamp. Resource readiness is not customer workflow acceptance.
Core hooks publish fixed diagnostic codes through Kubernetes termination status,
so known enrollment checks and API refusals appear beside the failed pod without
raw exception text. The installer saves these codes in its private ownership
receipt before enabling cleanup; they survive node deletion. Unknown failures
still require inspection of the retained node before cleanup.
After local Dex routing restarts the API server, setup checks both the host API
and the TLS-verified in-cluster API Service before advancing to Core. If either
path does not recover within the bounded readiness wait, setup retains the
cluster and stops before host-trust changes or schema-hook execution.
Named-provider qualification also requires a controller image containing the
named-provider endpoint guard; the published beta.6 controller predates it.
The existing Image Cache Publish workflow has an opt-in `controller_arm64_only`
dispatch for an attested AL3 candidate and image receipt without updating tags
or publishing a release. Bind its verified receipt with the maintained chart
packaging helper before a fresh installation; do not replay failed hooks.
Fresh Kind beta setup can enroll SRE with an explicitly empty model-contract
catalogue only under its record-only accounting and unbounded token-only funding.
The enrollment hook checks the sealed and live Kind profile and accounting policy;
finite funding and production installs still require contracts. Provider activation
follows enrollment, without fabricated prices or contracts.
Final preflight rechecks prerequisites and target freshness; stopped Kind nodes
count as existing clusters. Podman inventory uses JSON rather than Kind's label
template, which is incompatible with Podman 6.

Local account passwords require 12+ characters, uppercase, lowercase, a number
and a symbol, within bcrypt's 72 UTF-8 byte limit. Matching feedback is immediate;
you can generate unique test credentials and explicitly copy them. The copy
includes the password and may enter a synced system clipboard. Setup does not
persist browser credentials. After all final checks pass, review host-trust scope,
approve the changes and type the exact destination to enable installation.
**Copy destination** copies the displayed name without checking consent boxes or
filling the confirmation for you. When macOS requests certificate trust, setup
shows **Action required in the macOS dialog** and waits for the actual system command
without a deadline. Approving continues automatically; denial or failure stops
the attempt without replay or rollback. Ctrl+C cancels a pending authorization.
OS passwords belong only in the system dialog or terminal, never the browser.
Failure recovery commands pin the Node executable and this installer's CLI path
instead of invoking an older globally installed `kars`. Run the command from the
displayed working directory. `dev down` honors an explicit `--cluster-name`
before or after `down` (the latter takes precedence) and uses the owned receipt
to pin the runtime and immutable node identity for managed installations.
After failure, **Remove this failed installation** offers the same owned teardown
without leaving the browser. Confirm deletion of that exact node and its data,
then click Remove. The installer waits interactively for required authorization,
rechecks custody, and removes only recorded host trust and the immutable node.
Other clusters and the Podman machine remain untouched; private receipts remain.
Partial cleanup can be retried without repeating node deletion. Missing or changed
ownership blocks cleanup; a failed installation identity cannot be reused.
To reopen a failed attempt after updating or closing the installer, run
`kars up --setup-browser <failed-cluster>` from its original working directory,
using the same CLI. Close the original installer first. This only opens validated
retained evidence and a cleanup confirmation; it does not restart installation,
delete anything automatically, scan other attempts or recover provider secrets.
Editing inputs invalidates that review, even if you later restore the old value.
Launch `kars up --setup-browser` from an interactive terminal and keep it open.
Setup checks authorization again before privileged installation commands; if it
expired during image preparation, the same terminal prompts again before the
operation runs. OS authentication policies still apply. Privileged operations
remain noninteractive and are never retried to acquire authorization. No password,
sudo output or authentication input is sent to the browser or stored by Kars.

Provider choices:

- **GitHub Copilot:** GitHub device sign-in; the durable OAuth credential stays
  installer-side and is exchanged for a short-lived inference token. Copy the
  device code, open GitHub, then choose from the account's live enabled chat
  models, including Responses-only models. Preflight refreshes account availability
  and uses the selected model's advertised Chat Completions or Responses API.
  Listing does not replace the real model-response check; unavailable catalogs
  fail visibly and can be retried.
- **OpenAI:** official API endpoint, project key and model ID.
- **Anthropic:** official **OpenAI-compatible Chat Completions** endpoint for the
  shared SRE/Bridge route. This is not full native Messages support; advanced
  native-only capabilities are unavailable. Anthropic recommends its native API
  for production features; this compatibility route is not production qualification.
- **Azure AI Foundry / Azure OpenAI:** public resource endpoint, deployment and key.
- **Self-hosted / OpenAI-compatible:** existing remote HTTPS API base on port 443,
  API key and model ID. Local addresses and private DNS answers are rejected.
  This does **not** provision KAITO/GPU models or configure in-cluster local-model
  discovery and egress. Anonymous endpoints are not supported here.

Preflight makes a billable model-response check only after local prerequisites
pass. Installation writes the chosen Core defaults; Bridge inherits Core
configuration. For named providers, it installs Core (and Bridge only if selected)
before enrolling the empty router-only credential store. Only after the workspace
authority is ready does it populate that exact Secret UID/version. Credentials never enter
Helm values, and concurrent store changes fail without overwriting existing data.
Existing AKS choices remain **inspection-only**. On AKS, discovery verifies the canonical Core
Helm release and controller ownership instead of treating namespace presence as
an installed workspace. It displays only public provider/model defaults; it does
not read provider Secrets, verify inference, adopt authority, or overwrite
configuration. Changing targets or retrying inspection clears previous results.
Browser adoption/provider inheritance for an existing installation is not yet
implemented. The terminal and reviewed Helm paths remain available.

Installation shows the current real stage, completed stages and elapsed time.
Long-running image, CNI and Helm operations stay visible without fabricated
percentages. Failure details distinguish recorded node ownership, host-trust
changes and unavailable receipts. Owned teardown is offered only when a node
identity was recorded; it rechecks ownership before deleting. Run the shown
command from the parent of the private attempt directory, after closing the
installer. A consumed failed attempt cannot be retried; use a fresh cluster name
and review after inspecting or cleaning up the previous attempt.

Keep the launching terminal open. Closing the concierge after installation does
not remove components or close access tunnels; Ctrl+C closes tunnels, not the
cluster. Failed attempts and private receipts are retained—no automatic rollback
or replay. Inspect failures before starting a fresh named attempt. HTTP readiness
is not first-login or customer-workflow acceptance.

## Common commands

| Command | What it does |
|---|---|
| `kars dev --release` | Run a sandbox from published images (Docker), no compile |
| `kars dev --release --target local-k8s` | Same, on a local kind cluster |
| `kars connect <name>` | Open the agent chat TUI |
| `kars up --release` | Provision AKS + ACR + Foundry from signed public images (no build) |
| `kars schemas prepare --release kars --namespace kars-system --chart <path>` | Prepare owned CRDs and published OpenAPI before a direct Helm installation |
| `kars add` | Add a sandbox / runtime to an existing deployment |
| `kars operator` | Live operator dashboard (agents, mesh, security posture) |
| `kars --help` | Full command list |

## Security & provenance

Every published kars artefact — container images **and** this CLI tarball — is
**cosign keyless-signed**, ships an **SPDX SBOM**, and carries a **SLSA build
provenance attestation**. The CLI is published to npm via **OIDC trusted
publishing** (no long-lived tokens). Verify with `npm audit signatures`,
`cosign verify`, and `gh attestation verify`.

## Links

- **Repository & docs:** https://github.com/Azure/kars
- **Getting started:** https://github.com/Azure/kars/blob/main/docs/getting-started.md
- **License:** MIT

> kars is an open-source reference implementation maintained under the Azure
> GitHub organization. Not an officially supported Microsoft product.
