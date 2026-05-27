# BYO Runtime Quickstart

A minimal **Bring-Your-Own** runtime for Kars. Demonstrates the
full round-trip:

1. Author a small FastAPI app that calls the Kars inference
   router at `http://127.0.0.1:8443/openai/v1`.
2. Package it into an OCI image that meets the
   [BYO contract](../../docs/byo-runtime-contract.md):
   - Runs as UID 1000.
   - Read-only root filesystem, writable `/sandbox` and `/tmp`.
   - Emits the `org.kars.runtime.contract` label.
   - All network I/O goes via the inference router on `127.0.0.1:8443`.
3. Roll it out as a `KarsSandbox` of `runtime.kind=BYO`.
4. Verify that Kars's policy / safety / token-budget pipeline
   applies to the agent's LLM calls.

> **Strict-mode admission** — when the controller is deployed with
> `controller.byoStrict=true`, the BYO contract is enforced at
> reconciliation time. A typo in `byo.contractVersion` or a missing
> image tag will surface as `Degraded=True / Reason=BYOContractInvalid`
> on the CR instead of a half-rendered Deployment. See the
> [BYO strict-mode notes](../../docs/operations/byo-strict.md) for details.

## Prerequisites

- An Kars cluster (helm upgrade from `deploy/helm/kars`).
- An OCI registry the cluster can pull from (ACR, GHCR, …).
- `docker` to build the image.

## 1. Inspect the app

```bash
cat app/main.py
```

The app exposes a single endpoint `POST /chat` that forwards the
caller's prompt to the inference router using the **standard `openai`
SDK** — no Azure-specific auth in user code. The router handles AAD,
content-safety, policy checks and token budgets transparently.

## 2. Build & push

```bash
docker build -t ghcr.io/<your-org>/byo-quickstart:v1 .
docker push   ghcr.io/<your-org>/byo-quickstart:v1
```

## 3. Apply the KarsSandbox

```bash
sed "s|REPLACE_ME|ghcr.io/<your-org>/byo-quickstart:v1|" \
  k8s/karssandbox.yaml | kubectl apply -f -
kubectl wait karssandbox/byo-quickstart \
  --for=condition=Ready --timeout=120s
```

If `byoStrict` is enabled and you forgot the tag, the controller
will refuse:

```bash
kubectl get karssandbox byo-quickstart -o jsonpath='{.status.conditions}'
# [{"type":"Degraded","status":"True","reason":"BYOContractInvalid", ...}]
```

## 4. Drive the agent

```bash
kubectl port-forward svc/byo-quickstart 8080:8080 -n kars-byo-quickstart
curl localhost:8080/chat -d '{"prompt":"What does Kars do?"}' \
  -H content-type:application/json
```

Tear down with:

```bash
kubectl delete karssandbox byo-quickstart
```

## 5. Verify strict-mode admission (recommended for production)

The example above runs cleanly under either `byoStrict=false` (default
— advisory warnings only) or `byoStrict=true` (rejection at admission
time). For production we recommend the latter. To see strict mode
actually reject a malformed CR, use the intentionally-invalid demo
manifest:

```bash
# Roll the controller with strict mode on:
helm upgrade kars deploy/helm/kars \
  --reuse-values --set controller.byoStrict=true
kubectl rollout status deploy/kars-controller -n kars-system

# Apply a CR with a bogus contractVersion:
kubectl apply -f k8s/karssandbox-strict-demo.yaml

# Confirm the controller refused:
kubectl get karssandbox byo-strict-demo -n kars-system \
  -o jsonpath='{.status.conditions}' | jq
# Expect: type=Degraded, status=True, reason=BYOContractInvalid

# No Deployment / Service / NetworkPolicy should exist:
kubectl get all -n kars-byo-strict-demo 2>/dev/null || echo "namespace empty (as expected)"

# Tear down:
kubectl delete -f k8s/karssandbox-strict-demo.yaml
```

See [`docs/operations/byo-strict.md`](../../docs/operations/byo-strict.md)
for the full list of CR-level checks and the roadmap for
registry-side label introspection.
