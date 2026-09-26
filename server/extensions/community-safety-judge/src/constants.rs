pub(crate) const PLUGIN_ID: &str = "community-safety-judge";
pub(crate) const PLUGIN_NAME: &str = "Community Safety Judge";
pub(crate) const VERSION: &str = "0.1.0";

/// The one `durable-job` kind this extension declares and handles. Must
/// match the job kind the host enqueues under (see
/// `waddle-server::extension_job_outbox`'s production enqueue call site in
/// `ingress::durable::apply_durable`).
pub(crate) const JOB_KIND_MESSAGE_JUDGE: &str = "message-judge";

/// Jev's Decisions API endpoint, confirmed from OpenRouter's own docs
/// (<https://openrouter.ai/docs/guides/community/jev> and
/// <https://openrouter.ai/docs/api/api-reference/alphadecisions/submit-a-decisions-request>,
/// fetched 2026-09-25). Not a guess. Only referenced from the real
/// (non-test) transport.
#[cfg(not(test))]
pub(crate) const JEV_ENDPOINT: &str = "https://openrouter.ai/api/alpha/decisions";

/// Pinned model identifier, rather than the `~typesafe/jev-latest` alias
/// OpenRouter also documents: this extension records `model_version` on
/// every judgment result (to detect model drift) and should not have its
/// underlying model silently change out from under a fixed config value.
/// Bump this deliberately when moving to a newer Jev release, or override
/// via the `model` config key.
pub(crate) const DEFAULT_JEV_MODEL: &str = "typesafe/jev-1.13";

/// Response bodies are expected to be a small typed-answer JSON document;
/// the host's own `outbound-http-request` capability already enforces a
/// hard cap (`waddle-extensions::runtime::http`'s
/// `EXTENSION_HTTP_MAX_BODY_BYTES`) — this is just this extension's own
/// sanity bound on top of that. Only referenced from the real (non-test)
/// transport.
#[cfg(not(test))]
pub(crate) const MAX_RESPONSE_BYTES: usize = 64 * 1024;
