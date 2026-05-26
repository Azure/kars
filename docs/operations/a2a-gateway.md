# A2A gateway operations

> Companion to `docs/architecture/a2a-gateway.md`. Read that first.

## Enable

The gateway is opt-in. To turn it on:

```bash
helm upgrade azureclaw deploy/helm/azureclaw \
  --set a2aGateway.enabled=true \
  --set inferenceRouter.a2aMtls.enabled=true   # required pair
```

Both halves must agree:

- `a2aGateway.enabled=true` renders the gateway Deployment, Service,
  and ServiceAccount.
- The router's mTLS listener is gated by the env var
  `A2A_MTLS_ENABLED=1` (set today on the router Deployment when the
  pair is opted in).

When `a2aGateway.enabled=false` (default), the inference router is
reachable only on the cluster-internal mesh — current behaviour
byte-for-byte unchanged.

## Cert rotation

Two independent rotations:

### Public TLS leaf (gateway → external callers)

1. cert-manager / AGC controller writes the new leaf to the Secret
   named in `a2aGateway.tls.secretName`.
2. The Kubelet refreshes the Secret-projected volume (≤60s).
3. The gateway's `notify::Watcher` (`a2a-gateway/src/tls.rs`) sees
   the file-change event and atomically swaps `Arc<ServerConfig>`.
4. Existing TLS sessions continue under the old key; new sessions
   pick up the new leaf. **No pod restart required.**

### Gateway → router mTLS pair

1. Rotate the gateway client cert (`a2aGateway.mtls.secretName`)
   *and* the corresponding CA bundle the router trusts
   (`A2A_MTLS_CA_PATH` on the router Deployment) **in lockstep**.
2. Use a transitional CA bundle (old + new) for at least one Kubelet
   refresh window before retiring the old cert. The Cilium
   ClusterwideNetworkPolicy `azureclaw-a2a-gateway-to-router`
   already restricts the path to the gateway ServiceAccount, so a
   stolen cert from outside that SA cannot reach the router even if
   chain-of-trust check passed.

## Rate-limit tuning

The defaults (60 burst / 5 rps refill) are a starting point.

To tune:

```yaml
a2aGateway:
  rateLimits:
    perSubjectBurst: 120
    perSubjectRefillPerSec: 10
    maxSubjects: 100000
```

Indicators that you need to raise the limits:

- Prometheus: `a2a_gateway_rejections_total{reason="rate_limited"}`
  rising faster than `a2a_gateway_requests_total`.
- Reports of 429 responses from legitimate peers in their post-mortems.

Indicators you can lower them:

- Subject map nearing `maxSubjects` from a small set of repeat
  callers (i.e., the cap was sized for breadth that does not exist).

The Helm value `sharedRedisUrl` is reserved for cross-replica sync
but **not yet implemented**. Setting it today panics the
gateway with an explicit message.

## Observability setup

| Endpoint | Port | Use |
|---|---|---|
| `/healthz` | 9090 | Liveness — process up. |
| `/readyz` | 9090 | Readiness — admin server bound. |
| `/metrics` | 9090 | Prometheus exposition. |

### Prometheus scrape

```yaml
- job_name: azureclaw-a2a-gateway
  kubernetes_sd_configs:
    - role: pod
      selectors:
        - role: pod
          label: "app.kubernetes.io/name=azureclaw-a2a-gateway"
  relabel_configs:
    - source_labels: [__meta_kubernetes_pod_container_port_name]
      regex: admin
      action: keep
```

### Suggested SLO alerts

| Alert | Expression | Severity |
|---|---|---|
| Gateway down | `up{job="azureclaw-a2a-gateway"} == 0` for 5m | page |
| JWS rejection burst | `rate(a2a_gateway_rejections_total{reason="jws_invalid"}[5m]) > 1` | ticket |
| Replay attack signal | `rate(a2a_gateway_rejections_total{reason="replay"}[1m]) > 0` | page |
| Subject map saturating | `a2a_gateway_subject_count / 50000 > 0.8` | ticket |

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| 503 on `/readyz` | Admin server not bound | Check pod `kubectl logs` for bind errors. |
| All requests rejected as `jws_invalid` | Trust store empty | Check `A2aAgent` CR projection / re-apply trust anchors. |
| Router refuses gateway connections | CA bundle skew | Verify `A2A_MTLS_CA_PATH` on the router contains the *current* gateway-CA. |
| Cert reload didn't fire | macOS dev cluster (kqueue quirks) | Force a pod restart; in-cluster Linux uses inotify and is reliable. |
