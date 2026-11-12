These PEM keys and certificates are test-only fixtures for the signed OCI policy
verification tests in `controller/src/policy_fetcher.rs`. They form an isolated
Fulcio-like root and leaf plus a test Rekor signing key; none are production
credentials or trusted outside this test.

The fixture exercises Cosign's legacy `.sig` OCI manifest format and a valid
Rekor Signed Entry Timestamp. It does not represent a public Sigstore trust root.
