use std::fs;
use std::process::Command;

use serde_json::Value;

const BINARY: &str = env!("CARGO_BIN_EXE_waddle-operational-qualification");

#[test]
fn executable_examples_produce_stable_plans_and_explicit_accounting() {
    let plan = || {
        Command::new(BINARY)
            .args(["plan", "examples/small-run.json"])
            .output()
            .expect("run plan")
    };
    let first = plan();
    let second = plan();
    assert!(first.status.success());
    assert_eq!(first.stdout, second.stdout);
    let value: Value = serde_json::from_slice(&first.stdout).expect("plan JSON");
    assert_eq!(value["totals"]["expected_pairs"], 8);
    assert_eq!(value["totals"]["duration_ms"], 600);
    let output = Command::new(BINARY)
        .args([
            "account",
            "examples/small-run.json",
            "examples/synthetic-observations.json",
        ])
        .output()
        .expect("run account");
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("report JSON");
    assert_eq!(value["qualification"], "not_assessed");
    assert_eq!(value["accounting_status"], "complete");
    assert_eq!(value["measurement"]["expected_pairs"], 6);
    assert_eq!(value["measurement"]["on_time_pairs"], 3);
    assert_eq!(value["measurement"]["late_pairs"], 1);
    assert_eq!(value["measurement"]["timed_out_pairs"], 2);
    assert_eq!(value["measurement"]["duplicate_receipts"], 1);
}

#[test]
fn executable_rejects_oversized_and_invalid_inputs_without_echoing_contents() {
    let root = std::env::temp_dir().join(format!("waddle-offline-cli-{}", std::process::id()));
    fs::create_dir(&root).expect("create private test directory");
    let path = root.join("input.json");
    let marker = "synthetic-secret-marker-must-not-appear";
    fs::write(&path, format!(r#"{{"password":"{marker}"}}"#)).expect("write invalid input");
    let output = Command::new(BINARY)
        .arg("plan")
        .arg(&path)
        .output()
        .expect("run invalid input");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains(marker));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "input is not valid contract JSON\n"
    );
    fs::write(&path, vec![b' '; 8 * 1024 * 1024 + 1]).expect("write oversized input");
    let output = Command::new(BINARY)
        .arg("plan")
        .arg(&path)
        .output()
        .expect("run oversized input");
    fs::remove_file(path).expect("remove input");
    fs::remove_dir(root).expect("remove private test directory");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "input exceeds 8 MiB\n"
    );
}
