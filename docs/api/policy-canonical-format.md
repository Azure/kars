# Policy canonical format — per-kind byte rules

> Byte-exact canonicalization rules for kars signed Policy artifacts.
> The [`kars policy sign`](../cli-reference.md#kars-policy) CLI applies
> these rules before computing the OCI layer digest; the controller's
> `policy_fetcher` re-validates them after pulling. Both producer and consumer
> are byte-identical or the artifact is rejected.
>
> Every signed Policy artifact is YAML — *block style only, fixed key
> order, LF-only, trailing newline*. The kind-specific rules below
> layer on top of that.

## §0. Universal rules (apply to every kind)

These rules hold for **every** `application/vnd.kars.*.v1+yaml` artifact.
Producer (CLI) and consumer (controller) both enforce them; mismatch =
`CanonicalFormError`, fail-closed.

1. **Block style only.** No flow style (`{a: b, c: d}`), no anchors/aliases,
   no comments, no document-end marker (`...`). Document-start `---` is
   optional and ignored by the consumer.
2. **Fixed key order per kind** — see kind sections below. Map keys are
   emitted in the order specified by the kind's schema, never alphabetical
   by default.
3. **UTF-8, LF-only line endings, single trailing newline** at end of file.
4. **No tabs anywhere.** Indentation is exactly two spaces per level.
5. **Integer/scalar normalization:** integers as plain digits (`42`, never
   `0x2a` or `+42`); booleans as `true` / `false` (never `yes` / `True`);
   strings always quoted when they contain `:` or start with `-`.
6. **List ordering:** lexicographic by the kind's natural sort key (see
   per-kind sections). Producer sorts; consumer re-verifies sort order.
7. **Pinned `apiVersion` + `kind`** at the top of every artifact:

   ```yaml
   apiVersion: kars.dev/v1alpha1
   kind: {EgressAllowlist|ToolPolicy|InferencePolicy|MemoryPolicy|McpToolsPolicy|EvalCorpus}
   ```

   These two fields are the **first two keys** in every artifact and are
   verified by the consumer before any further parsing.
8. **Generation field is monotonic.** Every artifact carries `generation: N`
   under metadata; `N` MUST be greater than the previous artifact's
   generation when the CR rotates digests. Consumer surfaces `Drift=True`
   if generation regresses without an explicit rollback annotation.

## §1. `egress` — EgressAllowlist v1

**Status:** authoritative implementation in
`cli/src/commands/egress/sign.ts` (producer) + `controller/src/policy_fetcher.rs`
(consumer). Slice 1c-real **extracts** the shared scaffolding but does NOT
change the on-wire byte format — every existing signed bundle remains valid.

**Layer mediaType:** `application/vnd.kars.egress-allowlist.v1+yaml`

**Key order:**

```yaml
apiVersion: kars.dev/v1alpha1
kind: EgressAllowlist
metadata:
  name: <slug>
  generation: <int>
spec:
  endpoints:
    - host: <canonical-host>
      port: <int>
      protocol: <string>   # optional; omit if absent
    - ...
```

**Host normalization (rule #7):**

- Lowercase ASCII or Punycode (IDNA-2008 UTS-46 transitional=false).
- No leading/trailing dot, no `..`, no whitespace, no wildcards (`*`).
- No control bytes (`< 0x20` or `0x7f`).
- Allowed alphabet after normalization: `[a-z0-9.-]`.

**Endpoint sort:** lexicographic by `(host, port, protocol?)` tuple,
treating absent `protocol` as the empty string for sort purposes.

**Reference:** `cli/src/commands/egress/sign.ts` is the authoritative
implementation. Any change to these rules requires a `v2` artifactType and
a controller-side compatibility shim.

## §2. `tools` — ToolPolicy v1 *(Slice 1c-real)*

**Layer mediaType:** `application/vnd.kars.tool-policy.v1+yaml`

**Key order:**

```yaml
apiVersion: kars.dev/v1alpha1
kind: ToolPolicy
metadata:
  name: <slug>
  generation: <int>
spec:
  appliesTo:
    selector:
      matchLabels:
        <k>: <v>    # sorted by key, ASCII lex
  tools:
    <tool-id>:
      mode: allow | warn | deny
      conditions:
        - <canonical-condition-string>    # see §2.1
      rateLimit:                          # optional; omit entire block if absent
        perMinute: <int>
        perHour: <int>
  agtProfile:                             # optional; omit if absent
    inline: |
      <verbatim AGT YAML body>            # not re-canonicalized; passed through
```

**Tool sort:** map keys (`tool-id`) sorted ASCII lex. Within each tool,
`conditions[]` sorted ASCII lex on the canonical-condition-string.

### §2.1 Canonical condition string

A condition is a JSON object reduced to a canonical single-line string:

- Keys sorted ASCII lex.
- No whitespace except inside string values.
- Booleans/numbers/strings encoded as JSON.

Example: `{"argMatches":"^github.com/.*","method":"GET"}` (alphabetic key
order, no spaces).

### §2.2 AGT profile inline body

The `agtProfile.inline` value is treated as **opaque bytes** by the canonical
format — neither re-indented nor re-canonicalized. This is intentional: AGT
maintains its own schema and version cadence; we don't want to fork-validate
its YAML.

The producer concatenates `agtProfile.inline` verbatim into the output stream
between a literal `agtProfile:\n  inline: |\n` header and a single trailing
newline. The consumer extracts the same byte range for hashing into the
AGT mount.

## §3. `inference` — InferencePolicy v1 *(Slice 1c-real)*

**Layer mediaType:** `application/vnd.kars.inference-policy.v1+yaml`

**Key order:**

```yaml
apiVersion: kars.dev/v1alpha1
kind: InferencePolicy
metadata:
  name: <slug>
  generation: <int>
spec:
  appliesTo:
    selector:
      matchLabels:
        <k>: <v>
  routes:
    - path: <string>                # e.g. /v1/chat/completions
      mode: allow | deny
      allowedModels:                # optional; omit if absent
        - <model-id>
      tokenBudget:                  # optional
        perRequest: <int>
        perMinute: <int>
      contentSafety:                # optional
        enabled: <bool>
        categories: [hate, sexual, violence, selfharm]   # fixed enum order
```

**Route sort:** `(path, mode)` tuple, ASCII lex.
**Allowed-model sort:** ASCII lex on model-id strings.
**ContentSafety category order:** the fixed enum order shown above (NOT
alphabetical) — categories present in the source policy are emitted in
this order; absent categories are simply not included.

## §4. `memory` — MemoryPolicy v1 *(Slice 1c-real)*

**Layer mediaType:** `application/vnd.kars.memory-policy.v1+yaml`

**Key order:**

```yaml
apiVersion: kars.dev/v1alpha1
kind: MemoryPolicy
metadata:
  name: <slug>
  generation: <int>
spec:
  appliesTo:
    selector:
      matchLabels:
        <k>: <v>
  binding:
    foundryProject: <resource-id>
    threadSelector: <string>             # e.g. "user-{principal}"
    retention:
      maxTurns: <int>
      maxAgeSeconds: <int>
  redaction:                             # optional; omit if absent
    patterns:
      - name: <slug>
        regex: <ECMAScript-style>
        replacement: <string>
```

**Redaction-pattern sort:** ASCII lex on `name`.

## §5. `mcp-tools` — McpToolsPolicy v1 *(Slice 1c-real)*

**Layer mediaType:** `application/vnd.kars.mcp-tools.v1+yaml`

**Key order:**

```yaml
apiVersion: kars.dev/v1alpha1
kind: McpToolsPolicy
metadata:
  name: <slug>
  generation: <int>
spec:
  appliesTo:
    selector:
      matchLabels:
        <k>: <v>
  servers:
    - id: <slug>                          # matches McpServer.metadata.name
      allowedTools:
        - <tool-name>
      deniedTools:                        # optional; omit if absent
        - <tool-name>
      argFilters:                         # optional
        - tool: <tool-name>
          path: <jsonpath>
          mode: allow | deny
          pattern: <regex>
```

**Server sort:** ASCII lex on `id`.
**Tool list sort:** ASCII lex (both allowed and denied).
**ArgFilter sort:** `(tool, path, mode)` tuple, ASCII lex.

## §6. `eval-corpus` — EvalCorpus v1 *(Slice 6)*

Reserved. Schema lands in Slice 6 when KarsEval is repurposed.

## §7. Forward compatibility

- **v1 consumers MUST refuse v2 artifacts.** A future v2 schema bumps the
  artifactType suffix (`.v2+yaml`); legacy controllers reject by
  artifactType mismatch.
- **v1 producers MUST refuse to sign content that doesn't round-trip the
  canonical rules.** If a YAML field is present that the producer doesn't
  recognize, the producer fails — never silently strips. This prevents
  "looks like it worked" bugs where the consumer ignores a field the
  operator thought was being enforced.
- **Schema additions require a new artifactType.** Even additive
  changes (e.g., a new optional field) require `.v2+yaml` — because the
  byte canonical form is part of the digest, and any new key changes the
  digest deterministically.

## §8. Reference implementation locations

| Concern | Producer (TS) | Consumer (Rust) |
|---------|--------------|----------------|
| Universal rules (§0) | `cli/src/commands/policy/canonical/common.ts` *(Slice 1c-real)* | `controller/src/policy_canonical/common.rs` *(Slice 1c-real)* |
| egress (§1) | `cli/src/commands/egress/sign.ts` (existing) | `controller/src/policy_fetcher.rs` (existing) |
| tools (§2) | `cli/src/commands/policy/canonical/tools.ts` *(Slice 1c-real)* | `controller/src/policy_canonical/tools.rs` *(Slice 1c-real)* |
| inference (§3) | `cli/src/commands/policy/canonical/inference.ts` *(Slice 1c-real)* | `controller/src/policy_canonical/inference.rs` *(Slice 1c-real)* |
| memory (§4) | `cli/src/commands/policy/canonical/memory.ts` *(Slice 1c-real)* | `controller/src/policy_canonical/memory.rs` *(Slice 1c-real)* |
| mcp-tools (§5) | `cli/src/commands/policy/canonical/mcp_tools.ts` *(Slice 1c-real)* | `controller/src/policy_canonical/mcp_tools.rs` *(Slice 1c-real)* |
| eval-corpus (§6) | Slice 6 | Slice 6 |

The egress producer/consumer pair is the **reference implementation**
against which every other kind is reviewed for behavioural consistency.
Any divergence (e.g., different sort algorithm, different host
normalization library) requires explicit justification in the slice doc.
