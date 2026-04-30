# Phase 3 Manual Verification Checklist (Pre-Public-Release)

> Tracking the human-only OSPO compliance items that cannot be auto-graded.
> Source: `docs/internal/2026-04-28-Azure-azureclaw.md` § Manual Verification Required.

## Status

- [ ] Owner: TBD (assign individual once team membership verified)
- [ ] Target completion: prior to dev → main close-out PR for Phase 3 (S29).

---

## Items (11 total)

### Branch Protection (5 items)

| Item ID | What to check | Where | Reference |
|---|---|---|---|
| **BP-DEFAULT-PROTECTED** | Verify `main` branch is protected (no direct pushes allowed) | GitHub Settings → Branches → Branch protection rules | `docs/releasing/general/branch-protection.md` |
| **BP-PR-REQUIRED** | PR + ≥1 reviewer required before merge | same | OSPO § Branch Protection §. 82 |
| **BP-CODEOWNERS-REVIEW** | "Require review from Code Owners" is enabled | same | `CODEOWNERS` file; verify `@AzureClawTeam` is configured |
| **BP-CHECKS-REQUIRED** | Required status checks enforced: `ci`, `ci-gates`, `codeql`, `image-sign-sbom` | same → "Require status checks to pass before merging" | OSPO § Branch Protection §. 85 |
| **BP-LINEAR-HISTORY** | Enforce linear history (no force-push, no merge commits) | same | OSPO § Branch Protection §. 86 |

---

### Repository Security (2 items)

| Item ID | What to check | Where |
|---|---|---|
| **SEC-SECRET-SCAN** | Secret Scanning enabled | Settings → Code security & analysis → Secret scanning → Enable |
| **SEC-PUSH-PROTECTION** | Push Protection enabled (prevents commit of secrets) | same section → Push Protection → Enable |

---

### Contributions & CLA (2 items)

| Item ID | What to check | Where |
|---|---|---|
| **CONTRIB-CLA-BOT** | Microsoft CLA bot installed and configured on repo | Settings → Integrations / Installed GitHub Apps → search "CLA" |
| **CONTRIB-COMAINTAINER** | `@AzureClawTeam` roster has ≥2 active Microsoft employees | GitHub org Members page; verify team membership |

---

### Release & Naming (3 items)

| Item ID | What to check | Where |
|---|---|---|
| **REL-REGISTERED** | Repository registered in OSPO Release Portal, status = "approved" | https://aka.ms/opensource/portal (Microsoft internal) |
| **REL-NAMING** | Repo name "azureclaw" approved by Branding / CELA | OSPO ticket history / ticket reference in release request |
| **REL-PUBLIC-PROCESS** | CELA / Trademark sign-off recorded against release request | same OSPO ticket |

---

### Component Governance (1 item)

| Item ID | What to check | Where |
|---|---|---|
| **SEC-SUPPLY-CHAIN** | Component Governance / Artemis enrollment is configured | 1ES tooling dashboard or repo Settings → Advanced → Artemis |

---

## How to Record Completion

Each item above must be manually verified by a human with the required access:

- **Branch protection** checks (BP-*): GitHub.com repo Settings access
- **Security settings** (SEC-SECRET-SCAN, SEC-PUSH-PROTECTION): GitHub.com repo Settings access
- **CLA & team** (CONTRIB-CLA-BOT, CONTRIB-COMAINTAINER): GitHub.com repo/org access
- **Release & naming** (REL-*): OSPO Portal access (Microsoft internal) + ticket history
- **Component Governance** (SEC-SUPPLY-CHAIN): 1ES tooling access

### Workflow for closing items

1. **Verify** the item in the appropriate system (see "Where" column above).
2. **Record evidence** (screenshot, ticket number, or link) in a comment on the PR that closes this checklist.
3. **Check the box** in this document for that item (replace `[ ]` with `[x]`).
4. **Commit** the update (one commit per batch of related items, or one final commit closing all).
5. **Create a PR** with the updated checklist.

### Phase 3 close-out (S29)

Once all 11 items are verified and checked, this file becomes the **evidence trail** attached to the S29 dev → main PR. The PR description will reference this checklist and the evidence comments.

---

## Reference: OSPO Findings

Below are the exact manual items extracted from the OSPO scorecard:

> From `docs/internal/2026-04-28-Azure-azureclaw.md` § Manual Verification Required:
> 
> - Branch protection on `main` (BP-* — all 5 items)
> - Secret scanning + push protection enabled (SEC-SECRET-SCAN, SEC-PUSH-PROTECTION)
> - CLA bot installed on the repo (CONTRIB-CLA-BOT)
> - ≥2 active Microsoft co-maintainers behind `@AzureClawTeam` (CONTRIB-COMAINTAINER)
> - OSPO Release Portal entry exists and is approved (REL-REGISTERED)
> - Repo name approved by Branding / CELA (REL-NAMING)
> - Public-process / CELA Trademark sign-off recorded (REL-PUBLIC-PROCESS)
> - Component Governance / Artemis enrollment (SEC-SUPPLY-CHAIN)
> - ESRP signing for any future binary releases (SEC-CODE-SIGNING — *note: currently only OCI images ship, which use Notation+OIDC: this passes*)
> - PoliCheck clean run on final tree (CODE-POLICHECK — *automation tbd*)
> - Internal-only references / non-public URLs scrubbed (CODE-NO-INTERNAL — *automation tbd*)
>
> **Total: 11 items requiring human verification** (PoliCheck and internal-reference scrub are marked automation-pending and out of scope for this checklist).

---

## Notes

- **Branch protection (BP-*)**: All 5 items are binary go/no-go checks in a single UI pane. Can batch-verify in one pass.
- **Security scanning (SEC-SECRET-SCAN, SEC-PUSH-PROTECTION)**: Also in the same UI pane, can batch-verify together.
- **CLA + team (CONTRIB-CLA-BOT, CONTRIB-COMAINTAINER)**: CLA bot is in Integrations; team membership is in the org team roster.
- **Release naming (REL-REGISTERED, REL-NAMING, REL-PUBLIC-PROCESS)**: All three require OSPO Portal access; consolidate evidence in one ticket reference.
- **Component Governance (SEC-SUPPLY-CHAIN)**: May require escalation to the 1ES team if not self-service in repo settings.

---

*Checklist generated for Phase 3 S28 (Manual Verification). Companion to `docs/internal/2026-04-28-Azure-azureclaw.md`.*
