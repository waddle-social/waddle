//! Real [`DurableJobRunner`]: looks up the currently loaded/granted
//! extension for a claimed row's job kind and invokes it through the
//! ordinary `framework.handle-event` export, exactly like any other
//! extension event.

use std::sync::Arc;

use async_trait::async_trait;
use waddle_extensions::{
    DisplayText, DurableJob as WitDurableJob, DurableJobId, DurableJobOutcome, ExtensionEffect,
    ExtensionEvent, ExtensionManager, StanzaId as WitStanzaId,
};

use super::drain::{DurableJobRunOutcome, DurableJobRunner};
use super::store::ClaimedJob;

pub struct ExtensionManagerDurableJobRunner {
    manager: Arc<ExtensionManager>,
}

impl ExtensionManagerDurableJobRunner {
    pub fn new(manager: Arc<ExtensionManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl DurableJobRunner for ExtensionManagerDurableJobRunner {
    async fn run(&self, job: &ClaimedJob) -> DurableJobRunOutcome {
        let Some(actor) = self.manager.durable_job_handler(&job.job_kind) else {
            return DurableJobRunOutcome::NoHandler;
        };
        let Ok(target_stanza_id) = WitStanzaId::new(job.target_stanza_id.id.clone()) else {
            // The claimed row's own stanza-id was already validated at
            // enqueue time (see `store::enqueue_pending_in_tx`); a failure
            // here would mean stored data has been corrupted underneath
            // us, not a transient condition — but is still not this row's
            // fault to permanently punish, so back off and retry.
            return DurableJobRunOutcome::Failure {
                message: "stored target stanza-id is unexpectedly empty".to_string(),
                retryable: true,
            };
        };
        let Ok(body) = DisplayText::new(job.body_snapshot.clone()) else {
            return DurableJobRunOutcome::Failure {
                message: "stored body snapshot is unexpectedly empty".to_string(),
                retryable: true,
            };
        };
        let event = ExtensionEvent::DurableJob(WitDurableJob {
            kind: job.job_kind.clone(),
            job_id: DurableJobId::new(job.id.as_str()).unwrap_or_else(|_| {
                DurableJobId::new("unknown-job-id").expect("static id is non-empty")
            }),
            waddle_id: job.waddle_id.clone(),
            room: job.room.clone(),
            target_stanza_id,
            body,
            attempt: job.attempt_count.clamp(0, u32::MAX as i64) as u32,
        });
        let effects = actor
            .handle_event_for_waddle_with_requester(event, job.waddle_id.clone(), None)
            .await;
        for effect in effects {
            if let ExtensionEffect::DurableJobResult(outcome) = effect {
                return match outcome {
                    DurableJobOutcome::Success(result) => DurableJobRunOutcome::Success(result),
                    DurableJobOutcome::Failure(failure) => DurableJobRunOutcome::Failure {
                        message: failure.message.into_string(),
                        retryable: failure.retryable,
                    },
                };
            }
        }
        // The extension returned successfully but produced no
        // `DurableJobResult` effect at all (e.g. a `HostWarning` from a
        // failed host-tool call inside its own handling, or simply a bug).
        // Retryable: there is no reason to believe a retry would behave
        // identically forever, and dead-lettering on the first ambiguous
        // response would be too eager.
        DurableJobRunOutcome::Failure {
            message: "extension did not return a durable-job result for its durable-job event"
                .to_string(),
            retryable: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_extensions::{ExtensionCapability, ExtensionConfig, ExtensionModuleConfig, RoomJid};
    use waddle_xmpp_core::xep0359::StanzaId;

    fn message_hook_fixture_path() -> String {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../waddle-extensions/tests/fixtures/message_hook.wasm")
            .to_string_lossy()
            .into_owned()
    }

    /// Loads the shared `message_hook.wasm` test fixture (see
    /// `waddle-extensions/tests/fixtures/`), which answers any `DurableJob`
    /// event with a canned successful `judgment-result` — enough to prove
    /// the real host<->guest event conversion round trip
    /// (`waddle_extensions::DurableJob` -> WIT -> the guest -> WIT
    /// `durable-job-result` -> `waddle_extensions::DurableJobOutcome`)
    /// through a real wasmtime component, without needing network access
    /// or the real `community-safety-judge` extension (whose own Jev
    /// request/response logic is covered by its own crate's unit tests).
    async fn manager_with_granted_judge() -> Arc<ExtensionManager> {
        let manager = ExtensionManager::from_config(ExtensionConfig {
            enabled: true,
            modules: vec![ExtensionModuleConfig {
                name: "message-hook-fixture".into(),
                namespace: "urn:test:message-hook".into(),
                registry: Default::default(),
                digest: None,
                tag: None,
                config: serde_json::json!({"capabilities": [15], "job_kinds": ["message-judge"]}),
                capability_grants: vec![ExtensionCapability::DurableJob],
                allowed_http_origins: vec![],
                provider_room_grants: vec![],
                config_secret_files: Default::default(),
                local_path: Some(message_hook_fixture_path()),
            }],
            ..ExtensionConfig::default()
        })
        .await
        .expect("granted judge fixture must load");
        Arc::new(manager)
    }

    fn claimed_job() -> ClaimedJob {
        ClaimedJob {
            id: crate::extension_job_outbox::store::ExtensionJobOutboxId::generate(),
            lease_token: crate::extension_job_outbox::store::ExtensionJobOutboxLeaseToken::generate(
            ),
            extension_id: waddle_extensions::PluginId::new("message-hook-fixture")
                .expect("plugin id"),
            job_kind: waddle_extensions::JobKind::new("message-judge").expect("job kind"),
            waddle_id: waddle_extensions::WaddleId::new("default").expect("waddle id"),
            room: RoomJid::new("room@conference.example.test").ok(),
            target_stanza_id: StanzaId::new(
                "stanza-1".to_string(),
                "room@conference.example.test".parse().expect("room jid"),
            ),
            body_snapshot: "is this a question?".to_string(),
            attempt_count: 1,
            last_error: None,
            created_at_ms: 0,
        }
    }

    #[tokio::test]
    async fn run_round_trips_a_real_component_through_the_full_wit_event_conversion() {
        let manager = manager_with_granted_judge().await;
        let runner = ExtensionManagerDurableJobRunner::new(manager);

        let outcome = runner.run(&claimed_job()).await;
        match outcome {
            DurableJobRunOutcome::Success(result) => {
                assert_eq!(result.model_version.as_str(), "fixture-model");
                assert_eq!(result.scores.len(), 1);
                assert_eq!(result.scores[0].category.as_str(), "is_question");
                assert_eq!(result.scores[0].probability.value(), 0.5);
            }
            DurableJobRunOutcome::Failure { message, .. } => {
                panic!("expected the fixture's canned success, got failure: {message}")
            }
            DurableJobRunOutcome::NoHandler => panic!("expected a handler to be found"),
        }
    }

    #[tokio::test]
    async fn run_returns_no_handler_for_an_unrecognized_job_kind() {
        let manager = manager_with_granted_judge().await;
        let runner = ExtensionManagerDurableJobRunner::new(manager);
        let mut job = claimed_job();
        job.job_kind = waddle_extensions::JobKind::new("some-other-job-kind").expect("job kind");

        assert!(matches!(
            runner.run(&job).await,
            DurableJobRunOutcome::NoHandler
        ));
    }
}
