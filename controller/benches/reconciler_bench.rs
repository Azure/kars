// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Reconciler perf baseline (Phase 2 S16).
//!
//! Measures the synthetic reconcile-decision hot path at three workload
//! scales — empty cluster, 100 sandboxes, 1000 sandboxes. The bench does
//! NOT reach into the real `kube::Client` (would couple this benchmark
//! to live K8s) — instead it exercises the deterministic in-memory
//! decision path that dominates p50 reconcile latency in production
//! profiling: hash-map lookup of existing children + status diff.
//!
//! Baseline values are committed to `controller/benches/baselines.json`.
//! CI's `Bench Regression` job fails the PR if median latency exceeds
//! baseline + 25%.

use criterion::{Criterion, criterion_group, criterion_main};
use std::collections::HashMap;
use std::hint::black_box;

/// Simulates the decision step: given a desired-state map and an
/// observed-state map, compute the set of names that need create / update
/// / delete. This is the inner loop of the controller's reconciler.
fn reconcile_decision(desired: &HashMap<String, u64>, observed: &HashMap<String, u64>) -> usize {
    let mut work = 0usize;
    for (k, v) in desired {
        match observed.get(k) {
            Some(ov) if ov == v => {}
            _ => work += 1,
        }
    }
    for k in observed.keys() {
        if !desired.contains_key(k) {
            work += 1;
        }
    }
    work
}

fn build_state(n: usize) -> (HashMap<String, u64>, HashMap<String, u64>) {
    let mut desired = HashMap::with_capacity(n);
    let mut observed = HashMap::with_capacity(n);
    for i in 0..n {
        let name = format!("sandbox-{i:06}");
        desired.insert(name.clone(), i as u64);
        // 95% of observed entries match desired; 5% drift to force work.
        let observed_val = if i % 20 == 0 {
            (i as u64) ^ 0xff
        } else {
            i as u64
        };
        observed.insert(name, observed_val);
    }
    (desired, observed)
}

fn bench_reconcile(c: &mut Criterion) {
    let mut group = c.benchmark_group("reconcile_decision");
    for &n in &[0usize, 100, 1_000] {
        let (desired, observed) = build_state(n);
        group.bench_function(format!("n={n}"), |b| {
            b.iter(|| reconcile_decision(black_box(&desired), black_box(&observed)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_reconcile);
criterion_main!(benches);
