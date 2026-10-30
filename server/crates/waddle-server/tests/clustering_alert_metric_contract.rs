//! The Mimir alert rules read clustering metrics only through names pinned
//! in `src/clustering/alert_metric_contract.json`.
//!
//! The chain that makes a rename break a test:
//! - `clustering::metrics` unit tests bind each fixture `(name, unit)` to the
//!   instrument exported through the in-memory metric reader;
//! - `infrastructure/waddle.cloud/rules/tests/push_otlp_contract.py` (server
//!   `checkAlertRules`, which gates the image publish, and cloud `rulesTest`,
//!   which gates the rule sync) pushes each `(name, unit)` through
//!   Prometheus's real OTLP translator and requires exactly `prometheus_name`;
//! - this test requires every `waddle_clustering_*` series that any Mimir
//!   rule's `expr` reads to be a fixture `prometheus_name` (or an explicitly
//!   listed spelling the exporter never produces), and every contracted alert
//!   to read it.

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

/// Every Mimir rule file, as `(file name, contents)`. Like
/// `rules/tests/run.sh`, an empty set is a failure, not a pass.
fn mimir_rule_files() -> Vec<(String, String)> {
    let rules_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../infrastructure/waddle.cloud/rules/mimir");
    // The nix test derivations copy the rules into the build tree (flake.nix
    // testArgs postUnpack), so a missing directory is a broken copy path:
    // fail loudly rather than skipping the guard (#1436).
    let entries = std::fs::read_dir(&rules_dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", rules_dir.display()));
    let mut files: Vec<(String, String)> = entries
        .map(|entry| entry.expect("rules directory entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "yaml")
        })
        .map(|path| {
            let contents = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            (path.display().to_string(), contents)
        })
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no Mimir rule files under {}",
        rules_dir.display()
    );
    files
}

/// One rule's PromQL: the `alert:` or `record:` name and its `expr` value.
struct RuleExpression {
    name: String,
    expr: String,
}

/// The PromQL of every rule in a rule file. Only `expr` values are
/// returned, never annotations or runbook prose, so a metric named in prose
/// neither satisfies nor trips the checks below. Handles single-line
/// values and `|`/`>` block scalars.
fn rule_expressions(contents: &str) -> Vec<RuleExpression> {
    let lines: Vec<&str> = contents.lines().collect();
    let mut rules = Vec::new();
    let mut name = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rule) = trimmed
            .strip_prefix("- alert:")
            .or_else(|| trimmed.strip_prefix("- record:"))
        {
            name = Some(rule.trim().to_owned());
            continue;
        }
        let Some(value) = trimmed.strip_prefix("expr:") else {
            continue;
        };
        let rule = name
            .take()
            .unwrap_or_else(|| panic!("expr without an alert or record name: {line}"));
        let value = value.trim();
        let expr = if value.starts_with('|') || value.starts_with('>') {
            let indent = line.len() - trimmed.len();
            lines[index + 1..]
                .iter()
                .take_while(|next| {
                    next.trim().is_empty() || next.len() - next.trim_start().len() > indent
                })
                .map(|next| next.trim())
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            value.to_owned()
        };
        rules.push(RuleExpression { name: rule, expr });
    }
    rules
}

fn all_rule_expressions() -> Vec<RuleExpression> {
    mimir_rule_files()
        .iter()
        .flat_map(|(_, contents)| rule_expressions(contents))
        .collect()
}

fn promql_identifiers(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
        .filter(|word| !word.is_empty())
}

#[test]
fn every_clustering_series_in_the_mimir_rules_has_an_exported_name_contract() {
    let rules = all_rule_expressions();
    let contracts = alert_metric_contracts();
    assert!(!contracts.is_empty());

    for word in rules
        .iter()
        .flat_map(|rule| promql_identifiers(&rule.expr))
        .filter(|word| word.starts_with("waddle_clustering_"))
    {
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
    let rules = all_rule_expressions();
    for contract in alert_metric_contracts() {
        assert!(
            !contract.alerts.is_empty(),
            "{} names no alert",
            contract.name
        );
        for alert in &contract.alerts {
            let expression = rules
                .iter()
                .find(|rule| &rule.name == alert)
                .unwrap_or_else(|| panic!("alert {alert} is not in any Mimir rule file"));
            assert!(
                promql_identifiers(&expression.expr).any(|word| word == contract.prometheus_name),
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
    let rules = all_rule_expressions();
    let rule_words: Vec<&str> = rules
        .iter()
        .flat_map(|rule| promql_identifiers(&rule.expr))
        .collect();
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
