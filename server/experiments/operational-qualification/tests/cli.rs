use std::fs;
use std::io::Write;
use std::process::{Command, Output, Stdio};

use serde_json::{json, Value};

const BINARY: &str = env!("CARGO_BIN_EXE_waddle-operational-qualification");

fn local_isolation(input: &[u8]) -> Output {
    let mut child = Command::new(BINARY)
        .arg("local-isolation")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run local-isolation");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input)
        .expect("write declaration");
    child.wait_with_output().expect("finish local-isolation")
}

#[test]
fn local_isolation_executable_reads_stdin_and_emits_only_a_stable_unverified_proposal() {
    let input = include_bytes!("../examples/local-isolation.json");
    let first = local_isolation(input);
    let second = local_isolation(input);
    assert!(first.status.success());
    assert!(first.stderr.is_empty());
    assert_eq!(first.stdout, second.stdout);
    let value: Value = serde_json::from_slice(&first.stdout).expect("proposal JSON");
    assert_eq!(
        value["assurance"]["execution_authorization"],
        "not_authorized"
    );
    assert_eq!(value["assurance"]["artifact_closure"], "unverified");
    assert_eq!(value["assurance"]["host_confinement"], "unverified");
    assert_eq!(value["assurance"]["isolation"], "unverified");
    let output = Command::new(BINARY)
        .args(["local-isolation", "examples/local-isolation.json"])
        .output()
        .expect("reject path operand");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn local_isolation_executable_bounds_input_and_never_echoes_unsafe_input() {
    for input in [
        br#"{"credentials":"synthetic-secret-marker"}"#.as_slice(),
        b"not JSON: synthetic-secret-marker".as_slice(),
    ] {
        let output = local_isolation(input);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "input is not valid contract JSON\n"
        );
    }
    let valid = include_str!("../examples/local-isolation.json");
    for (field, duplicate) in [
        ("\"run_id\": 42,", "\"run_id\": 42, \"run_id\": 42,"),
        (
            "\"type\": \"file_bytes\",",
            "\"type\": \"file_bytes\", \"type\": \"file_bytes\",",
        ),
        ("\"vcpus\": 16,", "\"vcpus\": 16, \"vcpus\": 16,"),
        ("\"id\": 1,", "\"id\": 1, \"id\": 1,"),
        (
            "\"purpose\": \"qualification\",",
            "\"purpose\": \"qualification\", \"purpose\": \"qualification\",",
        ),
        (
            "\"role\": \"host_qemu_binary\",",
            "\"role\": \"host_qemu_binary\", \"role\": \"host_qemu_binary\",",
        ),
    ] {
        assert!(valid.contains(field));
        let output = local_isolation(valid.replacen(field, duplicate, 1).as_bytes());
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "input is not valid contract JSON\n"
        );
    }
    let fields: Value = serde_json::from_str(valid).expect("fixture");
    let digest = fields["artifacts"][0]["content"]["digest"]
        .as_str()
        .expect("digest");
    let digest_field = format!("\"digest\": \"{digest}\"");
    let repeated = format!("{digest_field}, {digest_field}");
    let output = local_isolation(valid.replacen(&digest_field, &repeated, 1).as_bytes());
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "input is not valid contract JSON\n"
    );
    // Whitespace fills the remaining budget without changing the valid declaration.
    let mut exact_limit = include_bytes!("../examples/local-isolation.json").to_vec();
    exact_limit.resize(64 * 1024, b' ');
    assert!(local_isolation(&exact_limit).status.success());
    exact_limit.push(b' ');
    let output = local_isolation(&exact_limit);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "input exceeds 64 KiB\n"
    );
}

#[test]
fn local_isolation_executable_enforces_inventory_and_resource_validation_after_parsing() {
    let valid: Value =
        serde_json::from_str(include_str!("../examples/local-isolation.json")).expect("fixture");
    let mut maximum = valid.clone();
    let template = maximum["additional_artifacts"][0].clone();
    maximum["additional_artifacts"] = Value::Array(
        (1..=64)
            .map(|id| {
                let mut artifact = template.clone();
                artifact["id"] = json!(id);
                artifact
            })
            .collect(),
    );
    let output = local_isolation(&serde_json::to_vec(&maximum).expect("maximum inventory JSON"));
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("proposal JSON");
    assert_eq!(value["assurance"]["artifact_closure"], "unverified");
    let mut invalid = Vec::new();
    let mut sixty_fifth = template.clone();
    sixty_fifth["id"] = json!(65);
    maximum["additional_artifacts"]
        .as_array_mut()
        .expect("inventory")
        .push(sixty_fifth);
    invalid.push(maximum);
    for (field, data) in [("vcpus", 17), ("memory_mib", 49153), ("disk_gib", 129)] {
        let mut value = valid.clone();
        value["resources"][field] = json!(data);
        invalid.push(value);
    }
    let mut duplicate = valid.clone();
    duplicate["additional_artifacts"]
        .as_array_mut()
        .expect("inventory")
        .push(template);
    invalid.push(duplicate);
    let mut zero = valid.clone();
    zero["additional_artifacts"][0]["id"] = json!(0);
    invalid.push(zero);
    for index in [0, 2] {
        let mut missing = valid.clone();
        missing["artifacts"]
            .as_array_mut()
            .expect("inventory")
            .remove(index);
        invalid.push(missing);
    }
    let mut wrong_type = valid.clone();
    wrong_type["artifacts"][0]["content"]["type"] = json!("oci_manifest");
    invalid.push(wrong_type);
    for value in invalid {
        let output = local_isolation(&serde_json::to_vec(&value).expect("invalid fixture JSON"));
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr)
            .starts_with("invalid local isolation declaration:"));
    }
}

#[test]
fn local_isolation_executable_handles_probe_and_qualification_without_placeholder_services() {
    let probe = local_isolation(include_bytes!("../examples/isolation-probe.json"));
    assert!(probe.status.success());
    let value: Value = serde_json::from_slice(&probe.stdout).expect("probe JSON");
    assert_eq!(value["manifest"]["purpose"], "isolation_probe");
    assert_eq!(
        value["manifest"]["artifacts"]
            .as_array()
            .expect("inventory")
            .len(),
        3
    );
    assert_eq!(value["manifest"]["resources"]["disk_gib"], 0);
    assert_eq!(value["requirements"]["guest_disk"], "forbidden");
    assert_eq!(
        value["assurance"]["execution_authorization"],
        "not_authorized"
    );
    let qualification = local_isolation(include_bytes!("../examples/local-isolation.json"));
    assert!(qualification.status.success());
    let value: Value = serde_json::from_slice(&qualification.stdout).expect("qualification JSON");
    assert_eq!(value["manifest"]["resources"]["vcpus"], 16);
    assert_eq!(value["manifest"]["resources"]["memory_mib"], 49152);
    let generator = value["manifest"]["artifacts"]
        .as_array()
        .expect("inventory")
        .iter()
        .find(|artifact| artifact["role"] == "generator")
        .expect("generator");
    assert_eq!(generator["content"]["type"], "file_bytes");
    assert_eq!(value["assurance"]["artifact_closure"], "unverified");
}

#[test]
fn local_isolation_executable_rejects_positional_records_zero_digests_and_additional_escapes() {
    let valid: Value =
        serde_json::from_str(include_str!("../examples/local-isolation.json")).expect("fixture");
    let mut invalid = Vec::new();
    invalid.push(json!([
        valid["schema_version"],
        valid["run_id"],
        valid["purpose"],
        valid["architecture"],
        valid["resources"],
        valid["artifacts"],
        valid["additional_artifacts"]
    ]));
    for target in [
        "resources",
        "artifact",
        "content",
        "additional",
        "additional_content",
    ] {
        let mut value = valid.clone();
        match target {
            "resources" => value["resources"] = json!([16, 49152, 32]),
            "artifact" => {
                let artifact = &value["artifacts"][0];
                value["artifacts"][0] = json!([
                    artifact["role"],
                    artifact["architecture"],
                    artifact["content"]
                ]);
            }
            "content" => {
                value["artifacts"][0]["content"] =
                    json!(["file_bytes", value["artifacts"][0]["content"]["digest"]])
            }
            "additional" => {
                let artifact = &value["additional_artifacts"][0];
                value["additional_artifacts"][0] = json!([
                    artifact["id"],
                    artifact["role"],
                    artifact["architecture"],
                    artifact["content"]
                ]);
            }
            "additional_content" => {
                value["additional_artifacts"][0]["content"] = json!([
                    "file_bytes",
                    value["additional_artifacts"][0]["content"]["digest"]
                ])
            }
            _ => unreachable!("fixed test targets"),
        }
        invalid.push(value);
    }
    for inventory in ["artifacts", "additional_artifacts"] {
        let mut value = valid.clone();
        value[inventory][0]["content"]["digest"] = json!(format!("sha256:{}", "0".repeat(64)));
        invalid.push(value);
    }
    for (field, data) in [
        ("id", json!("../../production")),
        ("id", json!(u64::MAX)),
        ("role", json!("production")),
        ("host_path", json!("synthetic-secret-marker")),
        ("qemu_args", json!(["synthetic-secret-marker"])),
    ] {
        let mut value = valid.clone();
        value["additional_artifacts"][0][field] = data;
        invalid.push(value);
    }
    for (index, value) in invalid.into_iter().enumerate() {
        let output = local_isolation(&serde_json::to_vec(&value).expect("invalid fixture JSON"));
        assert!(!output.status.success(), "invalid case {index}");
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "input is not valid contract JSON\n"
        );
    }
}

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
