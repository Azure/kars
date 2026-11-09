<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Bounded `serde_json::Value` CodeQL controls

This is a sink-local correction to the existing complete Rust
security-and-quality suite, not a query exclusion or a blanket JSON exemption.
`ci/codeql-rust-controls.sh` installs it into the isolated upstream checkout
before source-pack resolution and mandatory controls. The existing query-ID
coverage guard, library-path check, scanner IDs and analysis search-path binding
remain in force. The Actions path is unchanged.

## Scope

The recognizer requires an immutable primitive `&str` or `&[u8]`, a standard
`len()` rejection with a finite positive maximum no greater than 4 MiB, and the
exact false-branch SSA barrier at the parse argument. Only canonical standard
`serde_json::de::from_str` / `from_slice` calls whose unique concrete inferred
success type is the actual `serde_json::value::Value` qualify. Pseudo propagation
types are ignored when checking uniqueness, matching shared upstream inference;
multiple concrete types still fail the proof.

The only sink-model change excludes that proven parse from `ModelsAsDataSink`
using `FlowSummaryNode.getSummaryNode().getSourceSinkReportingElement()`.
Other unsafe-deserialization sinks and all existing barriers remain unchanged.
There is no global barrier, whole-function exemption, arbitrary `Deref`, generic
or custom deserializer exemption, or unbounded-input exemption. In particular,
`Bytes` / `response.bytes()` cases are not covered by this primitive-slice proof.

## Frozen provenance and fail-closed installation

- CodeQL CLI: **2.27.4**, already pinned by the existing workflow.
- Upstream commit: `fdf5c8481ffef04e04c7376167fb5ce25aae8453`.
- ch2b source manifest SHA-256:
  `e856e935770c7738ea867f0c63b8e48641f5c6b798eceb20573cba768fb94e7d`.
- Original upstream extension SHA-256:
  `5ed51f5a338722805f9810ac1306b8246c86e4eaf743c86b0eef22cf765b611a`.
- Patched upstream extension SHA-256:
  `c0da0548958ba44252eab00c1a70f99b0afd5ab8a269d85d6ff0dea2f38111eb`.

`controls/` is copied byte-for-byte from the successful frozen ch2b input,
including both expectations. The helper's historical “Experimental” header is
preserved for that byte identity; this runner now enables exactly that helper.
`manifest.json` records each payload hash. `fixtures/UnsafeDeserializationExtensions.qll`
is the unchanged upstream file from [the pinned source](https://github.com/github/codeql/blob/fdf5c8481ffef04e04c7376167fb5ce25aae8453/rust/ql/lib/codeql/rust/security/UnsafeDeserializationExtensions.qll),
retained solely for offline installer/pipeline regression tests.

`apply.py` verifies the revision, entire original extension, complete control file
set and every payload digest before writing. It applies exactly the two source
edits validated by ch2b and verifies the resulting digest. Existing helper or
control destinations and redirected source paths are rejected. The helper used
by the recognizer control and the installed analysis library is identical.
`bounded-value-extension.json` records provenance, digests and installed paths
in the existing evidence directory; installation alone is not a controls pass.
A mismatch or control failure stops the runner before publishing analysis inputs.

The runner retains all four previous Rust control directories and appends:

1. `query-tests/security/CWE-502-bounded-value-controls`: 17-row recognizer and
   nine-row integrated source-to-sink flow controls, including the mixed-sink
   case that retains the second, unbounded parse in the same function.
2. Unchanged upstream `query-tests/security/CWE-502`.
3. Unchanged upstream `query-tests/unusedentities`.

Never regenerate expectations. If the upstream pin changes, requalify the exact
extension and all controls rather than relaxing a checksum. Retire the local
extension only when an immutable upstream replacement passes the same positive,
negative, integrated-flow and complete-suite coverage controls.

## Validation boundaries

The persisted **ch2b** result is six actual CodeQL tests passing: two custom
controls and four unchanged upstream CWE-502 / unusedentities tests. It qualifies
the frozen recognizer and exact source edit, not this newly wired CI path or a
fresh hosted Kars analysis.

Local integration validation for this change:

- `PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest ci.tests.codeql_rust_controls_test`:
  **46 passed**. These execute the real installer, shell runner, coverage guard
  and binding with offline Git/CodeQL/rustc doubles; they do **not** execute the
  CodeQL compiler, extractor or behavioral query tests. They cover unchanged
  expectations, source/payload drift, mandatory control selection, failure before
  analysis exports, installed-library binding and the unchanged Actions path.
- `bash -n ci/codeql-rust-controls.sh`: **passed**.
- `shellcheck ci/codeql-rust-controls.sh`: **passed**.

No new hosted analysis or remote submission was performed. The remaining hosted
findings, including cases outside the primitive bounded-slice scope, are not
claimed cleared. Security sign-off remains separate.
