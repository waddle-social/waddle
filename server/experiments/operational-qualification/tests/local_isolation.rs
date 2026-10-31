use serde_json::{json, Value};
use waddle_operational_qualification::{
    AdditionalArtifactId, AdditionalArtifactRole, ArtifactContent, ArtifactRole, GuestArchitecture,
    LocalIsolationManifest, LocalIsolationPlan, LocalPurpose, LocalResources, LocalRunId,
    Sha256Digest,
};

fn declaration() -> LocalIsolationManifest {
    serde_json::from_str(include_str!("../examples/local-isolation.json"))
        .expect("synthetic declaration")
}

fn valid_json() -> Value {
    serde_json::to_value(declaration()).expect("declaration JSON")
}

fn assert_invalid(value: Value) {
    if let Ok(manifest) = serde_json::from_value::<LocalIsolationManifest>(value) {
        assert!(LocalIsolationPlan::build(manifest).is_err());
    }
}

#[test]
fn proposal_canonicalizes_inventory_and_keeps_every_assurance_unverified() {
    let manifest = declaration();
    let plan = LocalIsolationPlan::build(manifest.clone()).expect("valid declaration");
    let encoded = serde_json::to_vec(&plan).expect("proposal JSON");
    let mut reversed = manifest;
    reversed.artifacts.reverse();
    reversed.additional_artifacts.reverse();
    assert_eq!(
        encoded,
        serde_json::to_vec(&LocalIsolationPlan::build(reversed).expect("reordered declaration"))
            .expect("proposal JSON")
    );
    let value: Value = serde_json::from_slice(&encoded).expect("proposal JSON");
    assert_eq!(
        value["assurance"],
        json!({
            "execution_authorization": "not_authorized",
            "artifact_digest_verification": "unverified",
            "artifact_closure": "unverified",
            "architecture_compatibility": "unverified",
            "host_confinement": "unverified",
            "isolation": "unverified",
            "inventory_scope": "declaration_only",
            "qualification": "not_assessed"
        })
    );
    assert_eq!(
        value["requirements"],
        json!({
            "guest_disk": "required",
            "kvm_acceleration": "required",
            "software_emulation": "forbidden",
            "unprivileged_qemu": "required",
            "implicit_devices": "forbidden",
            "user_configuration": "forbidden",
            "network_interfaces": "forbidden",
            "host_shares": "forbidden",
            "host_runtime_sockets": "forbidden",
            "device_passthrough": "forbidden",
            "vsock": "forbidden",
            "independent_host_confinement": "required",
            "management": {
                "private_run_owned_local_socket": "required",
                "run_directory_and_socket_ownership_validation": "required",
                "separate_guest_serial": "required",
                "serial_is_untrusted": "required",
                "serial_capture_limit_bytes": 1048576,
                "tcp_listener": "forbidden",
                "allowed_operations": ["inspect_status", "request_shutdown", "force_stop"]
            }
        })
    );
    assert_eq!(plan.manifest().artifacts.len(), ArtifactRole::ALL.len());
}

#[test]
fn resource_and_identity_bounds_are_inclusive_and_do_not_claim_capacity() {
    for resources in [
        LocalResources {
            vcpus: 1,
            memory_mib: 1_024,
            disk_gib: 8,
        },
        LocalResources {
            vcpus: 16,
            memory_mib: 49_152,
            disk_gib: 128,
        },
    ] {
        for run_id in [1, u32::MAX] {
            let mut manifest = declaration();
            manifest.resources = resources.clone();
            manifest.run_id = LocalRunId(run_id);
            assert!(LocalIsolationPlan::build(manifest).is_ok());
        }
    }
    for (field, invalid) in [
        ("vcpus", 0),
        ("vcpus", 17),
        ("vcpus", u64::from(u16::MAX)),
        ("memory_mib", 1_023),
        ("memory_mib", 49_153),
        ("memory_mib", u64::from(u32::MAX)),
        ("disk_gib", 7),
        ("disk_gib", 129),
        ("disk_gib", u64::from(u32::MAX)),
    ] {
        let mut value = valid_json();
        value["resources"][field] = json!(invalid);
        assert_invalid(value);
    }
    for field in ["vcpus", "memory_mib", "disk_gib"] {
        for extreme in [json!(u64::MAX), json!(-1), json!(1.5), json!("8")] {
            let mut value = valid_json();
            value["resources"][field] = extreme;
            assert_invalid(value);
        }
    }
    for run_id in [
        json!(0),
        json!(u64::MAX),
        json!(-1),
        json!("../../production"),
        json!("https://production.invalid"),
    ] {
        let mut value = valid_json();
        value["run_id"] = run_id;
        assert_invalid(value);
    }
    for schema in [0, 2, u16::MAX] {
        let mut manifest = declaration();
        manifest.schema_version = schema;
        assert!(LocalIsolationPlan::build(manifest).is_err());
    }
}

#[test]
fn minimum_inventory_rejects_missing_duplicate_and_wrong_content_declarations() {
    for index in 0..ArtifactRole::ALL.len() {
        let mut missing = declaration();
        missing.artifacts.remove(index);
        assert!(LocalIsolationPlan::build(missing).is_err());

        let mut duplicate = declaration();
        duplicate.artifacts.push(duplicate.artifacts[index].clone());
        assert!(LocalIsolationPlan::build(duplicate).is_err());

        let mut replacement = declaration();
        replacement.artifacts[index] =
            replacement.artifacts[(index + 1) % ArtifactRole::ALL.len()].clone();
        assert!(LocalIsolationPlan::build(replacement).is_err());

        let mut wrong_type = declaration();
        let digest: Sha256Digest = format!("sha256:{}", "a".repeat(64))
            .parse()
            .expect("digest syntax");
        wrong_type.artifacts[index].content = match wrong_type.artifacts[index].content {
            ArtifactContent::FileBytes { .. } => ArtifactContent::OciManifest { digest },
            ArtifactContent::OciManifest { .. } => ArtifactContent::FileBytes { digest },
        };
        let both_allowed = !matches!(
            wrong_type.artifacts[index].role,
            ArtifactRole::HostQemuBinary
                | ArtifactRole::GuestKernel
                | ArtifactRole::GuestInitrd
                | ArtifactRole::GuestDisk
                | ArtifactRole::KindBinary
                | ArtifactRole::KindNode
                | ArtifactRole::CnpgOperator
        );
        assert_eq!(LocalIsolationPlan::build(wrong_type).is_ok(), both_allowed);
    }
    let mut empty = declaration();
    empty.artifacts.clear();
    assert!(LocalIsolationPlan::build(empty).is_err());
    let mut unknown_role = valid_json();
    unknown_role["artifacts"][0]["role"] = json!("production_guest");
    assert_invalid(unknown_role);
}

#[test]
fn architectures_are_explicit_and_every_artifact_must_match() {
    let mut arm = declaration();
    arm.architecture = GuestArchitecture::Aarch64;
    for artifact in &mut arm.artifacts {
        artifact.architecture = GuestArchitecture::Aarch64;
    }
    for artifact in &mut arm.additional_artifacts {
        artifact.architecture = GuestArchitecture::Aarch64;
    }
    assert!(LocalIsolationPlan::build(arm).is_ok());
    for index in 0..ArtifactRole::ALL.len() {
        let mut wrong = declaration();
        wrong.artifacts[index].architecture = GuestArchitecture::Aarch64;
        assert!(LocalIsolationPlan::build(wrong).is_err());
    }
    for architecture in [json!("native"), json!("amd64"), json!("production")] {
        let mut value = valid_json();
        value["architecture"] = architecture;
        assert_invalid(value);
    }
}

#[test]
fn digests_only_accept_immutable_lowercase_sha256_syntax() {
    for hex in [
        format!("{}1", "0".repeat(63)),
        "a".repeat(64),
        "f".repeat(64),
    ] {
        assert!(format!("sha256:{hex}").parse::<Sha256Digest>().is_ok());
    }
    for invalid in [
        String::new(),
        "sha256:".to_owned(),
        format!("sha256:{}", "0".repeat(64)),
        format!("sha256:{}", "0".repeat(63)),
        format!("sha256:{}", "0".repeat(65)),
        format!("sha256:{}", "A".repeat(64)),
        format!("sha256:{}", "g".repeat(64)),
        format!("sha512:{}", "0".repeat(64)),
        format!("sha256:{}\n", "0".repeat(64)),
        "https://production.invalid/image:latest".to_owned(),
        "/var/lib/production/disk".to_owned(),
        "$(touch /tmp/injection)".to_owned(),
    ] {
        let mut value = valid_json();
        value["artifacts"][0]["content"]["digest"] = json!(invalid);
        assert_invalid(value);
    }
    for invalid in [json!(null), json!(42), json!({"file":"/tmp/disk"})] {
        let mut value = valid_json();
        value["artifacts"][0]["content"]["digest"] = invalid;
        assert_invalid(value);
    }
}

#[test]
fn unknown_fields_and_all_location_or_command_escape_hatches_are_rejected() {
    for field in [
        "host_path",
        "url",
        "credentials",
        "qemu_args",
        "network_devices",
        "qmp_commands",
        "production",
        "execution_authorization",
    ] {
        for target in [
            "root",
            "resources",
            "artifact",
            "content",
            "additional",
            "additional_content",
        ] {
            let mut value = valid_json();
            let object = match target {
                "root" => &mut value,
                "resources" => &mut value["resources"],
                "artifact" => &mut value["artifacts"][0],
                "content" => &mut value["artifacts"][0]["content"],
                "additional" => &mut value["additional_artifacts"][0],
                "additional_content" => &mut value["additional_artifacts"][0]["content"],
                _ => unreachable!("fixed test targets"),
            };
            object[field] = json!("synthetic-secret-marker");
            assert_invalid(value);
        }
    }
    for kind in ["file", "oci_index", "qemu_arguments", "production"] {
        let mut value = valid_json();
        value["artifacts"][0]["content"]["type"] = json!(kind);
        assert_invalid(value);
    }
    for field in [
        "run_id",
        "purpose",
        "architecture",
        "resources",
        "artifacts",
        "additional_artifacts",
    ] {
        let mut value = valid_json();
        value.as_object_mut().expect("object").remove(field);
        assert_invalid(value);
    }
}

#[test]
fn probe_requires_only_diskless_kernel_initrd_and_host_qemu() {
    let probe: LocalIsolationManifest =
        serde_json::from_str(include_str!("../examples/isolation-probe.json"))
            .expect("synthetic probe");
    assert_eq!(probe.purpose, LocalPurpose::IsolationProbe);
    assert_eq!(probe.artifacts.len(), 3);
    assert!(probe.additional_artifacts.is_empty());
    let plan = LocalIsolationPlan::build(probe.clone()).expect("minimal probe");
    let value = serde_json::to_value(plan).expect("proposal JSON");
    assert_eq!(value["requirements"]["guest_disk"], "forbidden");
    for index in 0..3 {
        let mut missing = probe.clone();
        missing.artifacts.remove(index);
        assert!(LocalIsolationPlan::build(missing).is_err());
    }
    for resources in [
        LocalResources {
            vcpus: 1,
            memory_mib: 128,
            disk_gib: 0,
        },
        LocalResources {
            vcpus: 2,
            memory_mib: 1_024,
            disk_gib: 0,
        },
    ] {
        let mut bounded = probe.clone();
        bounded.resources = resources;
        assert!(LocalIsolationPlan::build(bounded).is_ok());
    }
    for resources in [
        LocalResources {
            vcpus: 0,
            memory_mib: 128,
            disk_gib: 0,
        },
        LocalResources {
            vcpus: 3,
            memory_mib: 128,
            disk_gib: 0,
        },
        LocalResources {
            vcpus: 1,
            memory_mib: 127,
            disk_gib: 0,
        },
        LocalResources {
            vcpus: 1,
            memory_mib: 1_025,
            disk_gib: 0,
        },
        LocalResources {
            vcpus: 1,
            memory_mib: 128,
            disk_gib: 1,
        },
    ] {
        let mut invalid = probe.clone();
        invalid.resources = resources;
        assert!(LocalIsolationPlan::build(invalid).is_err());
    }
    let mut wrong_purpose = probe.clone();
    wrong_purpose.purpose = LocalPurpose::Qualification;
    wrong_purpose.resources = declaration().resources;
    assert!(LocalIsolationPlan::build(wrong_purpose).is_err());
    let mut extra_service = probe.clone();
    extra_service
        .artifacts
        .push(declaration().artifacts[16].clone());
    assert!(LocalIsolationPlan::build(extra_service).is_err());
    let mut runtime = probe.clone();
    runtime
        .additional_artifacts
        .push(declaration().additional_artifacts[0].clone());
    assert!(LocalIsolationPlan::build(runtime).is_ok());
    let mut operator = probe;
    operator
        .additional_artifacts
        .push(declaration().additional_artifacts[1].clone());
    assert!(LocalIsolationPlan::build(operator).is_err());
}

#[test]
fn additional_inventory_is_bounded_typed_and_uses_unique_numeric_ids() {
    let mut maximum = declaration();
    let template = maximum.additional_artifacts[0].clone();
    maximum.additional_artifacts = (1..=64)
        .map(|id| {
            let mut artifact = template.clone();
            artifact.id = AdditionalArtifactId(id);
            artifact
        })
        .collect();
    assert!(LocalIsolationPlan::build(maximum.clone()).is_ok());
    maximum.additional_artifacts.push(template.clone());
    maximum.additional_artifacts[64].id = AdditionalArtifactId(65);
    assert!(LocalIsolationPlan::build(maximum).is_err());
    let mut empty = declaration();
    empty.additional_artifacts.clear();
    assert!(LocalIsolationPlan::build(empty).is_ok());
    let mut duplicate = declaration();
    duplicate.additional_artifacts.push(template);
    assert!(LocalIsolationPlan::build(duplicate).is_err());
    let mut zero = declaration();
    zero.additional_artifacts[0].id = AdditionalArtifactId(0);
    assert!(LocalIsolationPlan::build(zero).is_err());
    let mut maximum_id = declaration();
    maximum_id.additional_artifacts[0].id = AdditionalArtifactId(u16::MAX);
    assert!(LocalIsolationPlan::build(maximum_id).is_ok());
    let mut wrong_architecture = declaration();
    wrong_architecture.additional_artifacts[0].architecture = GuestArchitecture::Aarch64;
    assert!(LocalIsolationPlan::build(wrong_architecture).is_err());
    for role in [
        AdditionalArtifactRole::HostRuntimeFile,
        AdditionalArtifactRole::GuestRuntimeFile,
        AdditionalArtifactRole::OperatorImage,
        AdditionalArtifactRole::ServiceImage,
        AdditionalArtifactRole::OciBlob,
        AdditionalArtifactRole::OfflineConfiguration,
    ] {
        for content in [
            declaration().additional_artifacts[0].content.clone(),
            declaration().additional_artifacts[1].content.clone(),
        ] {
            let mut value = declaration();
            value.additional_artifacts[0].role = role;
            value.additional_artifacts[0].content = content.clone();
            let needs_image = matches!(
                role,
                AdditionalArtifactRole::OperatorImage | AdditionalArtifactRole::ServiceImage
            );
            assert_eq!(
                LocalIsolationPlan::build(value).is_ok(),
                needs_image == matches!(content, ArtifactContent::OciManifest { .. })
            );
        }
    }
}

#[test]
fn object_shapes_reject_positional_arrays_at_every_record_boundary() {
    let value = valid_json();
    assert_invalid(json!([
        value["schema_version"],
        value["run_id"],
        value["purpose"],
        value["architecture"],
        value["resources"],
        value["artifacts"],
        value["additional_artifacts"]
    ]));
    for target in [
        "resources",
        "artifact",
        "content",
        "additional",
        "additional_content",
    ] {
        let mut value = valid_json();
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
        assert_invalid(value);
    }
}
