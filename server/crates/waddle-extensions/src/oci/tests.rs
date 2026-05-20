use super::*;

use bytes::Bytes;
use oci_client::manifest::OciDescriptor;
use oci_client::Reference;

#[test]
fn validates_reference_format() {
    let cache_dir = std::env::temp_dir().join("waddle-test");
    let puller = OciExtensionPuller::new(cache_dir);
    let module = ExtensionModuleConfig {
        name: "example-extension".to_string(),
        registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
        digest: Some(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ),
        tag: None,
        namespace: "urn:example:extension:1".to_string(),
        config: serde_json::Value::Object(Default::default()),
        capability_grants: Vec::new(),
        allowed_http_origins: Vec::new(),
        provider_room_grants: Vec::new(),
        config_secret_files: Default::default(),
        local_path: None,
    };

    let reference = puller
        .reference_for(&module)
        .expect("reference should parse");
    let expected: Reference =
            "ghcr.io/waddle-social/waddle/extensions/example-extension@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .parse()
                .expect("reference should parse");
    assert_eq!(reference.registry(), expected.registry());
    assert_eq!(reference.repository(), expected.repository());
    assert_eq!(reference.digest(), expected.digest());
}

#[test]
fn rejects_reference_with_mutable_registry_tag() {
    let cache_dir = std::env::temp_dir().join("waddle-test");
    let puller = OciExtensionPuller::new(cache_dir);
    let module = ExtensionModuleConfig {
        name: "example-extension".to_string(),
        registry: "ghcr.io/waddle-social/waddle/extensions/example-extension:latest".to_string(),
        digest: Some(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ),
        tag: None,
        namespace: "urn:example:extension:1".to_string(),
        config: serde_json::Value::Object(Default::default()),
        capability_grants: Vec::new(),
        allowed_http_origins: Vec::new(),
        provider_room_grants: Vec::new(),
        config_secret_files: Default::default(),
        local_path: None,
    };

    let error = puller
        .reference_for(&module)
        .expect_err("mutable registry tags should be rejected");
    assert!(error.to_string().contains("must not include a mutable tag"));
}

#[test]
fn rejects_oci_module_tag_field_even_when_digest_is_set() {
    let cache_dir = std::env::temp_dir().join("waddle-test");
    let puller = OciExtensionPuller::new(cache_dir);
    let module = ExtensionModuleConfig {
        name: "example-extension".to_string(),
        registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
        digest: Some(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ),
        tag: Some("latest".to_string()),
        namespace: "urn:example:extension:1".to_string(),
        config: serde_json::Value::Object(Default::default()),
        capability_grants: Vec::new(),
        allowed_http_origins: Vec::new(),
        provider_room_grants: Vec::new(),
        config_secret_files: Default::default(),
        local_path: None,
    };

    let error = puller
        .reference_for(&module)
        .expect_err("tag field should be rejected for OCI modules");
    assert!(error
        .to_string()
        .contains("must use an immutable digest instead of tag"));
}

#[test]
fn selects_single_wasm_layer() {
    let layers = vec![
        ImageLayer {
            data: Bytes::from_static(&[0u8; 8]),
            media_type: "application/octet-stream".to_string(),
            annotations: None,
        },
        ImageLayer {
            data: Bytes::from_static(&[0, 97, 115, 109, 1, 0, 0, 0]),
            media_type: "application/wasm".to_string(),
            annotations: None,
        },
    ];
    let descriptors = vec![
        OciDescriptor {
            media_type: "application/octet-stream".to_string(),
            digest: String::new(),
            size: 8,
            urls: None,
            annotations: None,
            artifact_type: None,
        },
        OciDescriptor {
            media_type: "application/wasm".to_string(),
            digest: String::new(),
            size: 8,
            urls: None,
            annotations: None,
            artifact_type: None,
        },
    ];

    let layer =
        select_wasm_layer("example-extension", &layers, &descriptors).expect("wasm selected");
    assert_eq!(layer.data[..4], [0, 97, 115, 109]);
}

#[test]
fn rejects_invalid_wasm_payload() {
    let layer = ImageLayer {
        data: Bytes::from_static(&[1, 2, 3, 4, 0, 0, 0, 0]),
        media_type: "application/wasm".to_string(),
        annotations: None,
    };
    let error =
        validate_wasm_layer("example-extension", &layer).expect_err("invalid payload should fail");
    assert!(error.to_string().contains("payload is not a wasm binary"));
}

#[test]
fn accepts_wasm_component_payload() {
    let layer = ImageLayer {
        data: Bytes::from_static(&[0, 97, 115, 109, 0x0d, 0, 1, 0]),
        media_type: "application/wasm".to_string(),
        annotations: None,
    };

    validate_wasm_layer("example-extension", &layer).expect("component payload should validate");
}

#[test]
fn rejects_unknown_wasm_binary_version() {
    let layer = ImageLayer {
        data: Bytes::from_static(&[0, 97, 115, 109, 2, 0, 0, 0]),
        media_type: "application/wasm".to_string(),
        annotations: None,
    };

    let error = validate_wasm_layer("example-extension", &layer)
        .expect_err("unknown binary version should fail");
    assert!(error
        .to_string()
        .contains("uses unsupported wasm binary version"));
}

#[test]
fn rejects_invalid_cache_component() {
    let puller = OciExtensionPuller::new(std::env::temp_dir().join("waddle-test"));
    let module = ExtensionModuleConfig {
        name: "../bad".to_string(),
        registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
        digest: Some(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ),
        tag: None,
        namespace: "urn:example:extension:1".to_string(),
        config: serde_json::Value::Object(Default::default()),
        capability_grants: Vec::new(),
        allowed_http_origins: Vec::new(),
        provider_room_grants: Vec::new(),
        config_secret_files: Default::default(),
        local_path: None,
    };

    let error = puller
        .cached_wasm_path(&module)
        .expect_err("cache path should reject path traversal");
    assert!(error
        .to_string()
        .contains("must not include path separators"));
}

#[test]
fn cache_path_uses_sanitized_digest() {
    let puller = OciExtensionPuller::new(std::env::temp_dir().join("waddle-test"));
    let module = ExtensionModuleConfig {
        name: "example-extension".to_string(),
        registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
        digest: Some(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ),
        tag: None,
        namespace: "urn:example:extension:1".to_string(),
        config: serde_json::Value::Object(Default::default()),
        capability_grants: Vec::new(),
        allowed_http_origins: Vec::new(),
        provider_room_grants: Vec::new(),
        config_secret_files: Default::default(),
        local_path: None,
    };

    let path = puller
        .cached_wasm_path(&module)
        .expect("cache path should be valid");
    assert!(path.ends_with(
            "example-extension/sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.wasm"
        ));
}

#[test]
fn cached_wasm_requires_matching_sidecar_digest() {
    let root = std::env::temp_dir().join(format!(
        "waddle-extension-cache-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("cache test dir");
    let wasm_path = root.join("example-extension.wasm");
    let wasm = [0, 97, 115, 109, 0x0d, 0, 1, 0];
    std::fs::write(&wasm_path, wasm).expect("wasm fixture");

    let missing = validate_cached_wasm_file("example-extension", &wasm_path)
        .expect_err("cache without digest sidecar should be rejected");
    assert!(missing
        .to_string()
        .contains("failed to read cached extension digest"));

    std::fs::write(
        cached_wasm_digest_path(&wasm_path),
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
    )
    .expect("digest sidecar");
    let mismatch = validate_cached_wasm_file("example-extension", &wasm_path)
        .expect_err("cache with wrong digest sidecar should be rejected");
    assert!(mismatch.to_string().contains("wasm digest mismatch"));

    write_cached_wasm_digest(&wasm_path, &wasm).expect("valid digest sidecar");
    validate_cached_wasm_file("example-extension", &wasm_path)
        .expect("cache with matching digest sidecar should validate");

    let _ = std::fs::remove_dir_all(root);
}
