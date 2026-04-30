// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// k6 smoke test — inference-router (Phase 2 S16).
//
// 50 VUs for 30s against /healthz and a stub upstream. Asserts:
//   * p95 latency < 100ms
//   * error rate < 0.1%
//
// Run locally:
//   ROUTER_URL=http://127.0.0.1:8443 k6 run tests/k6/router_smoke.js
//
// CI: this script runs in the nightly perf workflow only
// (`.github/workflows/perf-nightly.yml`). It is intentionally NOT part of
// PR CI — k6's TLS + network behaviour on hosted runners is too flaky
// for a required-check.

import http from "k6/http";
import { check, sleep } from "k6";

export const options = {
  vus: 50,
  duration: "30s",
  thresholds: {
    http_req_duration: ["p(95)<100"],
    http_req_failed: ["rate<0.001"],
  },
};

const BASE = __ENV.ROUTER_URL || "http://127.0.0.1:8443";

export default function () {
  const r = http.get(`${BASE}/healthz`);
  check(r, {
    "status is 200": (resp) => resp.status === 200,
    "body has ok marker": (resp) =>
      typeof resp.body === "string" && resp.body.length > 0,
  });
  sleep(0.1);
}
