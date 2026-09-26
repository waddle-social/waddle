mod bindings {
    wit_bindgen::generate!({
        path: "../../wit",
        world: "waddle-extension",
        with: {
            "wasi:logging/logging@0.1.0-draft": generate,
            "wasi:clocks/monotonic-clock@0.2.0": generate,
            "wasi:io/poll@0.2.0": generate,
            "wasi:random/random@0.2.0": generate,
        },
    });
}

mod config;
mod constants;
mod judge;
mod manifest;
mod ui;

use bindings::exports;
#[cfg(not(test))]
use bindings::waddle::extension::runtime;
use bindings::waddle::extension::types;
use config::ProviderConfig;
use constants::JOB_KIND_MESSAGE_JUDGE;
#[cfg(not(test))]
use constants::{JEV_ENDPOINT, MAX_RESPONSE_BYTES};
use judge::JudgeError;
use manifest::manifest as extension_manifest;
use ui::display;

struct CommunitySafetyJudge;

bindings::export!(CommunitySafetyJudge with_types_in bindings);

impl exports::waddle::extension::lifecycle::Guest for CommunitySafetyJudge {
    fn init(config: String) -> Result<types::ExtensionManifest, String> {
        if let Err(error) = ProviderConfig::parse(&config) {
            return Err(format!(
                "community-safety-judge configuration is invalid: {error}"
            ));
        }
        Ok(extension_manifest())
    }
}

impl exports::waddle::extension::framework::Guest for CommunitySafetyJudge {
    fn handle_event(
        event: types::ExtensionEvent,
    ) -> Result<types::ExtensionResponse, types::ExtensionError> {
        let types::ExtensionEvent::DurableJob(job) = event else {
            return Err(extension_error(
                types::ExtensionErrorCode::UnsupportedEvent,
                "community-safety-judge only handles durable-job events",
            ));
        };
        if job.kind.value != JOB_KIND_MESSAGE_JUDGE {
            return Ok(types::ExtensionResponse {
                effects: vec![types::ExtensionEffect::DurableJobResult(
                    types::DurableJobOutcome::Failure(types::DurableJobFailure {
                        message: display(&format!(
                            "community-safety-judge does not handle job kind {:?}",
                            job.kind.value
                        )),
                        retryable: false,
                    }),
                )],
            });
        }
        let outcome = run_judge(&job.body.value);
        Ok(types::ExtensionResponse {
            effects: vec![types::ExtensionEffect::DurableJobResult(outcome)],
        })
    }
}

fn run_judge(body: &str) -> types::DurableJobOutcome {
    let config = match provider_config() {
        Ok(config) => config,
        Err(message) => {
            return types::DurableJobOutcome::Failure(types::DurableJobFailure {
                message: display(&message),
                retryable: false,
            })
        }
    };
    match judge_via_jev(&config, body) {
        Ok(batch) => types::DurableJobOutcome::Success(types::JudgmentResult {
            model_version: batch.model_version,
            scores: batch
                .judgments
                .into_iter()
                .map(|judgment| types::JudgmentScore {
                    category: judgment.judgment_name.to_string(),
                    probability: judgment.probability,
                    taxonomy_version: judgment.taxonomy_version.to_string(),
                })
                .collect(),
        }),
        Err(error) => types::DurableJobOutcome::Failure(types::DurableJobFailure {
            message: display(&error.to_string()),
            // Both `JudgeError` variants here are transient from the host's
            // point of view: a transport failure (network, non-2xx) may
            // succeed on retry, and an invalid-shape response from a live
            // vendor is far more likely a transient vendor-side hiccup than
            // a permanent condition this exact job will keep hitting
            // forever — so both back off and retry rather than
            // dead-lettering on the first miss.
            retryable: true,
        }),
    }
}

fn judge_via_jev(config: &ProviderConfig, body: &str) -> Result<judge::JudgmentBatch, JudgeError> {
    let request_body = judge::build_request_body(&config.model, body);
    let request_bytes = serde_json::to_string(&request_body).map_err(|error| {
        JudgeError::InvalidResponse(format!("failed to serialize jev request body: {error}"))
    })?;
    let response = execute_http_request(config, request_bytes)?;
    if !(200..300).contains(&response.status) {
        return Err(JudgeError::Transport(judge::transport_error_message(
            response.status,
            response.body.as_bytes(),
        )));
    }
    judge::parse_response(response.body.as_bytes(), &config.model)
}

#[cfg(not(test))]
fn execute_http_request(
    config: &ProviderConfig,
    request_body: String,
) -> Result<types::HttpResponse, JudgeError> {
    if request_body.len() > MAX_RESPONSE_BYTES {
        return Err(JudgeError::Transport(
            "jev request body exceeded extension limit".to_string(),
        ));
    }
    runtime::http_request(&types::OutgoingHttpRequest {
        method: types::HttpMethod::Post,
        url: types::Url {
            value: JEV_ENDPOINT.to_string(),
        },
        headers: vec![
            types::HttpHeader {
                name: "authorization".to_string(),
                value: format!("Bearer {}", config.api_key),
            },
            types::HttpHeader {
                name: "content-type".to_string(),
                value: "application/json".to_string(),
            },
        ],
        body: Some(request_body),
    })
    .map_err(|error| JudgeError::Transport(format!("jev request failed: {}", error.message.value)))
}

#[cfg(test)]
fn execute_http_request(
    _config: &ProviderConfig,
    _request_body: String,
) -> Result<types::HttpResponse, JudgeError> {
    unreachable!("tests exercise judge_via_jev's callers directly via #[cfg(test)] seams")
}

fn extension_error(code: types::ExtensionErrorCode, message: &str) -> types::ExtensionError {
    types::ExtensionError {
        code,
        message: display(message),
    }
}

fn provider_config() -> Result<ProviderConfig, String> {
    #[cfg(not(test))]
    {
        ProviderConfig::parse(&runtime::get_config()).map_err(|error| error.to_string())
    }
    #[cfg(test)]
    {
        Err("test builds never call provider_config".to_string())
    }
}

// `init`/`handle_event`'s WIT-facing shape (the actual host<->guest
// `DurableJob`/`durable-job-result` event conversion round trip through a
// real wasmtime component) is exercised at the integration level in
// `waddle-server::extension_job_outbox::runner`'s tests, against the
// shared `message_hook.wasm` test fixture rather than this crate directly
// (that fixture answers a `DurableJob` event with a canned success without
// needing network access; this crate's own Jev request/response logic has
// no cross-crate dependency to test against and is covered here instead).
// This crate's own unit tests cover the pieces that don't need a live
// component instance to call: config parsing (`config.rs`) and the Jev
// request/response logic (`judge.rs`).
