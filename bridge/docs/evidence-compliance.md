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

## Compliance mappings

Bridge may map evidence to NIST AI RMF, EU AI Act, or other control language.
These are implementation-evidence aids, not legal conclusions or an attestation
by Microsoft.

## Retention and export

Operators must define:

- mission/team retention TTLs;
- receipt and log retention;
- export to a durable evidence store or SIEM;
- key custody and rotation;
- incident preservation and legal hold;
- deletion and tenant-offboarding behavior.
