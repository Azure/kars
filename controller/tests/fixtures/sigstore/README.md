These PEM keys and certificates are test-only fixtures for the signed OCI
policy verification tests in `controller/src/policy_fetcher.rs`. They form an
isolated Fulcio-like root set and leaf certificates plus a test Rekor signing
key; none are production credentials or trusted outside this test.

The fixture exercises Cosign's legacy `.sig` OCI manifest format and a valid
Rekor Signed Entry Timestamp. It does not represent a public Sigstore trust root.
The SHA-384 and SHA-512 hashedrekord cases use P-384 and P-521 leaf keys,
respectively, so the signed payload's signature and advertised Rekor digest use
the same hash algorithm.
