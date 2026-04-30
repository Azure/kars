# k6 perf smoke (Phase 2 S16)

The k6 smoke test exercises the inference router at modest concurrency
(50 VUs / 30s) against `/healthz`. It is the wall-clock counterpart to
the in-process criterion bench in `inference-router/benches/proxy_bench.rs`.

## Run locally

```bash
# 1. Start the router (or any HTTP target) on 127.0.0.1:8443.
cargo run --bin azureclaw-inference-router &

# 2. Run k6.
k6 run tests/k6/router_smoke.js

# Or against a different target:
ROUTER_URL=https://router.example.com k6 run tests/k6/router_smoke.js
```

## Thresholds

| Metric | Limit |
|---|---|
| `http_req_duration` p95 | < 100 ms |
| `http_req_failed` rate | < 0.1 % |

If either fails, the script exits non-zero. The nightly perf workflow
(`.github/workflows/perf-nightly.yml`) treats that as a build failure.

## Why nightly only

k6 + GitHub-hosted runner network behaviour is too noisy for a required
PR check. The criterion benches (in-process, deterministic) cover the
PR-blocking regression gate; k6 catches the wall-clock view nightly.

## Adding new k6 scenarios

Add a new `tests/k6/<name>.js` file and append a step to the nightly
workflow. Keep each scenario focused on one surface (router health,
spawn latency, handoff latency) so failure attribution is easy.
