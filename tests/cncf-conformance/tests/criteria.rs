//! Per-criterion conformance tests. Each `#[test]` covers one row of
//! [`azureclaw_cncf_conformance::run_all_checks`] and asserts pass.
//!
//! These tests run in PR CI (`cargo test --all`) and are the gate
//! that catches regressions (e.g., a new CRD merged without a
//! conditions array, or a Deployment edited to drop probes).

use azureclaw_cncf_conformance as conf;

fn manifests() -> std::collections::BTreeMap<std::path::PathBuf, Vec<serde_yaml::Value>> {
    conf::load_repo_manifests()
}

#[test]
fn c1_crd_versions_served_storage() {
    let m = manifests();
    let crds = conf::collect_crds(&m);
    assert!(
        !crds.is_empty(),
        "no CRDs discovered under deploy/helm/azureclaw/templates/"
    );
    assert_eq!(conf::check_crd_versions_served(&crds), conf::Outcome::Pass);
}

#[test]
fn c2_printer_columns_present() {
    let m = manifests();
    let crds = conf::collect_crds(&m);
    assert_eq!(conf::check_printer_columns(&crds), conf::Outcome::Pass);
}

#[test]
fn c3_conditions_array_present() {
    let m = manifests();
    let crds = conf::collect_crds(&m);
    assert_eq!(conf::check_conditions_array(&crds), conf::Outcome::Pass);
}

#[test]
fn c4_structural_schema() {
    let m = manifests();
    let crds = conf::collect_crds(&m);
    assert_eq!(conf::check_structural_schema(&crds), conf::Outcome::Pass);
}

#[test]
fn c5_cel_validations_present() {
    let m = manifests();
    let crds = conf::collect_crds(&m);
    assert_eq!(conf::check_cel_validations(&crds), conf::Outcome::Pass);
}

#[test]
fn c6_status_subresource() {
    let m = manifests();
    let crds = conf::collect_crds(&m);
    assert_eq!(conf::check_status_subresource(&crds), conf::Outcome::Pass);
}

#[test]
fn c7_deployment_probes() {
    let m = manifests();
    assert_eq!(conf::check_deployment_probes(&m), conf::Outcome::Pass);
}

#[test]
fn c8_default_deny_netpol() {
    let m = manifests();
    assert_eq!(conf::check_default_deny_netpol(&m), conf::Outcome::Pass);
}

#[test]
fn c9_no_floating_latest_in_deployments() {
    let m = manifests();
    assert_eq!(conf::check_image_pinning(&m), conf::Outcome::Pass);
}

#[test]
fn c10_recommended_labels() {
    let m = manifests();
    let crds = conf::collect_crds(&m);
    assert_eq!(conf::check_recommended_labels(&crds), conf::Outcome::Pass);
}

#[test]
fn c11_crd_scope() {
    let m = manifests();
    let crds = conf::collect_crds(&m);
    assert_eq!(conf::check_scope(&crds), conf::Outcome::Pass);
}

#[test]
fn c12_state_printer_column() {
    let m = manifests();
    let crds = conf::collect_crds(&m);
    assert_eq!(conf::check_state_printer_column(&crds), conf::Outcome::Pass);
}

#[test]
fn c13_pod_security_baseline() {
    let m = manifests();
    assert_eq!(conf::check_pod_security_baseline(&m), conf::Outcome::Pass);
}

#[test]
fn c14_supply_chain_rows() {
    assert_eq!(conf::check_supply_chain_rows(), conf::Outcome::Pass);
}

#[test]
fn c15_deny_config() {
    assert_eq!(conf::check_deny_config(), conf::Outcome::Pass);
}

#[test]
fn full_report_is_all_pass() {
    let r = conf::run_all_checks();
    assert!(r.all_passed(), "conformance failures:\n{}", r.to_markdown());
}

#[test]
fn report_markdown_is_stable() {
    // Two back-to-back runs must produce byte-identical reports;
    // catches accidental non-determinism (e.g., HashMap iteration
    // leaking into the output).
    let a = conf::run_all_checks().to_markdown();
    let b = conf::run_all_checks().to_markdown();
    assert_eq!(a, b);
}
