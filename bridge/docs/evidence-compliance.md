<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Evidence, receipts, and compliance views

Bridge presents Kars evidence; it does not turn evidence mappings into a
certification.

## Evidence model

A delivered task may retain:

- mission output;
- text and binary artifacts;
- router trace and token telemetry;
- envelope digest and runtime/model identity;
- approvals and egress decisions;
- a `KarsReceipt`;
- inclusion-log metadata and verification result.

## Audit surface

The Audit product is read-only and self-contained. Auditors can inspect
receipts and evidence but cannot use Workspace or Console mutation APIs.

Insights, System and Operator Audit use the same typed receipt-log snapshot
as receipt verification. The reader includes the legacy `kars-receipt-log`
head and every numbered overflow segment in `BRIDGE_CORE_NAMESPACE` (default
`kars-system`), with checkpoint, witness and published-key data from that same
API snapshot. It checks object identities, contiguous segment indices,
previous-root links, entry sequence numbers and the complete hash chain.

A genuinely absent log or a valid empty legacy head has count zero. API errors,
malformed data, foreign responses, missing segments and broken chains return
the existing sanitized upstream-error response instead of an empty or healthy
summary. Owner-scoped Insights counts only logged inclusions for the visible
receipts, not the fleet-wide history. Cryptographic verification still requires
the signed payload/subject binding and checkpoint; a missing checkpoint cannot
produce a successful receipt-verification result.

Unlabelled legacy heads and valid opaque legacy payload-digest strings remain
readable. Overflow follows the current canonical names and explicit index/root
metadata; arbitrary renamed segments or missing index metadata are rejected,
not guessed. A paginated/incomplete snapshot is also rejected. No storage,
writer, permissions, trust anchor or external witness is created by these reads.

## Verification

Receipt verification checks the recorded payload, signature scheme/key
metadata, and inclusion evidence supported by the Kars release. Hash chaining
provides tamper detection. It is not equivalent to third-party notarization or
regulatory certification.

The Auditor's statement display and **Download JSON** contain decoded,
formatted JSON for inspection. They preserve the statement's JSON content,
not the original signed byte sequence. **Verify now** checks the retained
receipt's original DSSE payload and signature. Offline signature verification
must use that original payload (`spec.dsse.payload`, base64-decoded) with its
DSSE payload type and framing, rather than the formatted statement download.
A valid signature against the cluster-published key is not an independent
trust anchor or proof that the recorded controls were enforced.

Unavailable compliance mappings are shown separately from receipt verification,
with a retry control. A mapping lookup failure does not hide the signed receipt
or count as a successful compliance assessment.

## Effective-control evidence

Expand a receipt and follow **Inspect effective-control evidence and limitations**
from its completeness claim. **Verify now** loads the existing BFF verification
response's retained `signed_statement`; the panel displays the five fields under
`/predicate/completeness`, with links to their decoded signed source:

- `taskNamespaceFloorVap`: task-namespace CREATE floor;
- `execBanVap`: agent-container exec/attach ban;
- `postureLockVap`: UPDATE downgrade lock;
- `defaultDenyEgress`: sandbox-namespace default-deny egress;
- `floorEnforced`: rollup of the four controls, not a fifth independent control.

`false` means **NOT VERIFIED by the collector**, not proof of absent enforcement.
Missing or malformed fields remain **UNKNOWN**. Historical `true` values are
labelled **RECORDED TRUE**, never independently verified enforcement. A true
rollup without four true supporting flags is explicitly inconsistent. Neither
these values nor the receipt claim verdict are rewritten by the UI.

The maintained controller `gather_completeness()` deliberately returns an
unverified floor. Commit `8953d376` removed the old policy-name-only collector:
a named VAP without an applicable Deny binding, or an Egress policy in
`kars-system`, does not establish protection of the actual sandbox. The BFF
binds and decodes the signed statement; signature and inclusion verification
are separate from effective-control collection. Both Hermes and OpenClaw use
this shared controller receipt path; runtime-specific success is not a floor
observation.

Closing this evidence limitation requires retained, task-bound policy contents,
Deny bindings, actual namespace/pod/container selectors, exemptions and
issuance-time provenance. CREATE, CONNECT and UPDATE applicability must be
checked separately. NetworkPolicy evaluation must include all additive allow
policies in the sandbox namespace; runtime enforcement needs a separate
bound datapath witness. A chart setting, policy name, passing signature or
current live probe cannot retroactively establish a historical receipt's floor.
The panel lists these evidence requirements without enabling policies, changing
permissions, or claiming that live enforcement is absent. The displayed source
is decoded JSON, not original signed bytes; use the original DSSE payload for
offline verification as described above.

## Compliance mappings

Bridge may map evidence to NIST AI RMF, EU AI Act, or other control language.
These are implementation-evidence aids, not legal conclusions or an attestation
by Microsoft. The shared Mission/Auditor compliance panel displays **Claim PASS**,
**Claim PARTIAL**, **Claim OMITTED**, or the other recorded status, including for
advisory mappings. Several framework mappings can reference the same receipt
claim; counts are not independent observations or satisfied legal obligations.

Opening the panel or selecting **Download pack** does not verify the receipt.
Use **Verify now** or the displayed CLI command for the separate cryptographic
checks, with the trust-anchor and enforcement limitations above. Advisory does
not mean satisfied; missing or omitted evidence remains a limitation.

The JSON download preserves the received pack and adds an `interpretation`
notice. Existing API fields remain compatible: `satisfied` counts mapped PASS
claims; other legacy aggregates, including `partial`, are preserved unchanged.
Inspect `controls` for each recorded status. No aggregate is a compliance verdict.
The panel derives
its non-advisory/advisory counts from the actual mappings and does not change
signed claims, the verification result, or retained evidence.

## Retention and export

Operators must define:

- mission/team retention TTLs;
- receipt and log retention;
- export to a durable evidence store or SIEM;
- key custody and rotation;
- incident preservation and legal hold;
- deletion and tenant-offboarding behavior.
