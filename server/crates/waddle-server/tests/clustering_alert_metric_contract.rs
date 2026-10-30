//! The Mimir alert rules read clustering metrics only through names pinned
//! in `src/clustering/alert_metric_contract.json`.
//!
//! The chain that makes a rename break a test:
//! - `clustering::metrics` unit tests bind each fixture `(name, unit)` to the
//!   instrument exported through the in-memory metric reader;
//! - `infrastructure/waddle.cloud/rules/tests/push_otlp_contract.py` (cloud PR
//!   pipeline) pushes each `(name, unit)` through Prometheus's real OTLP
//!   translator and requires exactly `prometheus_name`;
//! - this test requires every `waddle_clustering_*` series a rule reads to be
//!   a fixture `prometheus_name` (or an explicitly listed spelling the
//!   exporter never produces), and every contracted alert to read it.

use std::path::Path;

#[derive(serde::Deserialize)]
struct AlertMetricContract {
    name: String,
    prometheus_name: String,
    alerts: Vec<String>,
    unexported_rule_spellings: Vec<String>,
}

fn alert_metric_contracts() -> Vec<AlertMetricContract> {
    serde_json::from_str(include_str!("../src/clustering/alert_metric_contract.json"))
        .expect("alert_metric_contract.json is a list of contracts")
}

fn mimir_reliability_rules() -> String {
    let rules_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../infrastructure/waddle.cloud/rules/mimir/waddle-reliability.yaml");
    // The nix test derivations copy this file into the build tree (flake.nix
    // testArgs postUnpack), so a missing file is a broken copy path: fail
    // loudly rather than skipping the guard (#1436).
    std::fs::read_to_string(&rules_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", rules_path.display()))
}

fn promql_identifiers(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
        .filter(|word| !word.is_empty())
}

/// The `expr:` line of the named alert rule.
fn alert_expression<'a>(rules: &'a str, alert: &str) -> &'a str {
    let header = format!("- alert: {alert}");
    rules
        .lines()
        .skip_while(|line| line.trim() != header)
        .skip(1)
        .take_while(|line| {
            let line = line.trim_start();
            !line.starts_with("- alert:") && !line.starts_with("- record:")
        })
        .find_map(|line| line.trim_start().strip_prefix("expr:"))
        .unwrap_or_else(|| panic!("alert {alert} has no expr in the Mimir rules"))
}

#[test]
fn every_clustering_series_in_the_mimir_rules_has_an_exported_name_contract() {
    let rules = mimir_reliability_rules();
    let contracts = alert_metric_contracts();
    assert!(!contracts.is_empty());

    for word in promql_identifiers(&rules) {
        if !word.starts_with("waddle_clustering_") {
            continue;
        }
        assert!(
            contracts.iter().any(|contract| {
                contract.prometheus_name == word
                    || contract
                        .unexported_rule_spellings
                        .iter()
                        .any(|spelling| spelling == word)
            }),
            "{word} is read by the Mimir rules but has no entry in \
             src/clustering/alert_metric_contract.json"
        );
    }
}

#[test]
fn every_contracted_alert_reads_the_exported_series() {
    let rules = mimir_reliability_rules();
    for contract in alert_metric_contracts() {
        assert!(
            !contract.alerts.is_empty(),
            "{} names no alert",
            contract.name
        );
        for alert in &contract.alerts {
            assert!(
                promql_identifiers(alert_expression(&rules, alert))
                    .any(|word| word == contract.prometheus_name),
                "alert {alert} no longer reads the exported series {}",
                contract.prometheus_name
            );
        }
    }
}

/// Listed unexported spellings must still be in the rules, so the list is
/// pruned together with the rules (G3 of #2046 drops the bare
/// `waddle_clustering_node_heartbeat_age` spelling).
#[test]
fn listed_unexported_spellings_are_still_read_by_the_rules() {
    let rules = mimir_reliability_rules();
    let rule_words: Vec<&str> = promql_identifiers(&rules).collect();
    for contract in alert_metric_contracts() {
        for spelling in &contract.unexported_rule_spellings {
            assert!(
                rule_words.contains(&spelling.as_str()),
                "{spelling} is no longer in the Mimir rules; drop it from \
                 alert_metric_contract.json"
            );
        }
    }
}
