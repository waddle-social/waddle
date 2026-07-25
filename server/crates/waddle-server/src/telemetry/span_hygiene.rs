//! Span-attribute hygiene at the export seam (#1439).
//!
//! Some attributes on exported spans are not ours to fix at the
//! instrumentation site:
//!
//! - `actor.id` (`#21@<libp2p peer id>`) is recorded by the `kameo`
//!   crate itself on every `actor.handle_message` span. It is unique per
//!   actor instance, so it is pure per-span cardinality with no query
//!   value — the actor *name* next to it is the useful label.
//! - `code.file.path` comes from the `tracing-opentelemetry` layer's
//!   location capture, which uses `Metadata::file()`. For our own crates
//!   that is already a repo-relative path, but for vendored dependencies
//!   it is the absolute build-sandbox path
//!   (`/nix/store/<hash>-vendor-cargo-deps/<hash>/kameo-0.20.0/src/…`),
//!   ~130 bytes of build-machine trivia on every dependency-framed span.
//! - Empty-string attribute values carry no information at all; they are
//!   dropped wholesale rather than shipped as `message_id=""`.
//!
//! Scrubbing happens on the exporter (after sampling, after the batch
//! processor) so it cannot influence any sampling decision made by
//! [`super::span_noise::SpanNoiseFilter`]: that sampler only ever sees
//! span names, parents, and creation-time attributes, none of which this
//! module touches.

use std::time::Duration;

use opentelemetry::{KeyValue, Value};
use opentelemetry_sdk::{
    error::OTelSdkResult,
    trace::{SpanData, SpanExporter},
    Resource,
};

/// Attribute keys removed from every exported span.
const DROPPED_ATTRIBUTE_KEYS: &[&str] = &["actor.id"];

/// The attribute whose absolute dependency paths get trimmed.
const CODE_FILE_PATH_KEY: &str = "code.file.path";

/// Trim an absolute dependency source path down to
/// `<crate>-<version>/src/…`, which is all a reader needs to jump to the
/// frame. Returns `None` when the path is already fine (our own crates
/// are recorded repo-relative) or when no crate root can be identified.
fn trimmed_code_file_path(path: &str) -> Option<String> {
    if !path.starts_with('/') {
        return None;
    }
    let crate_root_end = path.rfind("/src/")?;
    let crate_root_start = path[..crate_root_end].rfind('/')? + 1;
    Some(path[crate_root_start..].to_string())
}

/// Apply every hygiene rule to one span's attribute list, in place.
pub(crate) fn scrub_attributes(attributes: &mut Vec<KeyValue>) {
    attributes.retain(|attribute| {
        !DROPPED_ATTRIBUTE_KEYS.contains(&attribute.key.as_str())
            && !matches!(&attribute.value, Value::String(value) if value.as_str().is_empty())
    });
    for attribute in attributes.iter_mut() {
        if attribute.key.as_str() != CODE_FILE_PATH_KEY {
            continue;
        }
        let Value::String(path) = &attribute.value else {
            continue;
        };
        if let Some(trimmed) = trimmed_code_file_path(path.as_str()) {
            attribute.value = Value::String(trimmed.into());
        }
    }
}

/// Wraps the real span exporter and scrubs each batch on the way out.
#[derive(Debug)]
pub(crate) struct HygienicSpanExporter<E> {
    inner: E,
}

impl<E: SpanExporter> HygienicSpanExporter<E> {
    pub(crate) fn new(inner: E) -> Self {
        Self { inner }
    }
}

impl<E: SpanExporter> SpanExporter for HygienicSpanExporter<E> {
    async fn export(&self, mut batch: Vec<SpanData>) -> OTelSdkResult {
        for span in batch.iter_mut() {
            scrub_attributes(&mut span.attributes);
        }
        self.inner.export(batch).await
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.inner.set_resource(resource);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use tracing_subscriber::prelude::*;

    use super::*;

    #[derive(Clone, Debug)]
    struct CaptureSpanExporter(Arc<Mutex<Vec<SpanData>>>);

    impl SpanExporter for CaptureSpanExporter {
        async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
            self.0.lock().expect("capture lock").extend(batch);
            Ok(())
        }
    }

    fn scrubbed(attributes: Vec<KeyValue>) -> Vec<(String, String)> {
        let mut attributes = attributes;
        scrub_attributes(&mut attributes);
        attributes
            .into_iter()
            .map(|attribute| (attribute.key.to_string(), attribute.value.to_string()))
            .collect()
    }

    #[test]
    fn kameo_actor_id_is_dropped_but_actor_name_survives() {
        assert_eq!(
            scrubbed(vec![
                KeyValue::new("actor.id", "#21@12D3KooWQvA3f2Vh4Zk8Rk1sN9Y"),
                KeyValue::new("actor.name", "UserActor"),
            ]),
            vec![("actor.name".to_string(), "UserActor".to_string())],
        );
    }

    #[test]
    fn empty_string_attributes_are_omitted() {
        assert_eq!(
            scrubbed(vec![
                KeyValue::new("message_id", ""),
                KeyValue::new("message_id_present", "m-1"),
                KeyValue::new("recipients", 3i64),
            ]),
            vec![
                ("message_id_present".to_string(), "m-1".to_string()),
                ("recipients".to_string(), "3".to_string()),
            ],
        );
    }

    #[test]
    fn vendored_dependency_paths_are_trimmed_to_the_crate_root() {
        assert_eq!(
            scrubbed(vec![KeyValue::new(
                "code.file.path",
                "/nix/store/9k3l-vendor-cargo-deps/2f1c/kameo-0.20.0/src/actor/kind.rs",
            )]),
            vec![(
                "code.file.path".to_string(),
                "kameo-0.20.0/src/actor/kind.rs".to_string(),
            )],
        );
        assert_eq!(
            trimmed_code_file_path(
                "/Users/x/.cargo/registry/src/index.crates.io-1949/kameo-0.20.0/src/actor/kind.rs"
            )
            .as_deref(),
            Some("kameo-0.20.0/src/actor/kind.rs"),
        );
    }

    #[test]
    fn first_party_relative_paths_are_left_alone() {
        let path = "crates/waddle-server/src/telemetry/span_hygiene.rs";
        assert_eq!(trimmed_code_file_path(path), None);
        assert_eq!(
            scrubbed(vec![KeyValue::new("code.file.path", path)]),
            vec![("code.file.path".to_string(), path.to_string())],
        );
    }

    /// End-to-end through the real `tracing` → tracing-opentelemetry →
    /// SDK pipeline: what reaches the wire exporter is already scrubbed.
    #[test]
    fn exported_spans_carry_no_actor_id_empty_or_nix_store_attributes() {
        let exported = Arc::new(Mutex::new(Vec::new()));
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(HygienicSpanExporter::new(CaptureSpanExporter(Arc::clone(
                &exported,
            ))))
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("span-hygiene-test")));

        tracing::subscriber::with_default(subscriber, || {
            tracing::info_span!(
                "actor.handle_message",
                actor.name = "UserActor",
                actor.id = "#21@12D3KooWQvA3f2Vh4Zk8Rk1sN9Y",
                message_id = "",
                code.file.path =
                    "/nix/store/9k3l-vendor-cargo-deps/2f1c/kameo-0.20.0/src/actor/kind.rs",
            )
            .in_scope(|| {});
        });
        provider.force_flush().expect("flush");

        let spans = exported.lock().expect("capture lock").clone();
        let span = spans.first().expect("one span must export");
        let attributes: Vec<(String, String)> = span
            .attributes
            .iter()
            .map(|attribute| (attribute.key.to_string(), attribute.value.to_string()))
            .collect();
        assert!(
            attributes
                .iter()
                .any(|(key, value)| key == "actor.name" && value == "UserActor"),
            "the useful actor label must survive: {attributes:?}",
        );
        assert!(
            !attributes.iter().any(|(key, _)| key == "actor.id"),
            "kameo's per-instance actor.id must not export: {attributes:?}",
        );
        assert!(
            !attributes.iter().any(|(_, value)| value.is_empty()),
            "empty attribute values must not export: {attributes:?}",
        );
        assert!(
            !attributes
                .iter()
                .any(|(_, value)| value.contains("/nix/store/")),
            "build-sandbox paths must not export: {attributes:?}",
        );
        assert!(
            attributes
                .iter()
                .any(|(key, value)| key == "code.file.path"
                    && value == "kameo-0.20.0/src/actor/kind.rs"),
            "the dependency frame must stay resolvable: {attributes:?}",
        );
    }
}
