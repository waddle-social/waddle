#[cfg(not(test))]
use crate::bindings::waddle::extension::inference;
use crate::bindings::waddle::extension::{runtime, types};
use crate::bindings::{self, exports};
use crate::config::JevConfig;
#[cfg(not(test))]
use crate::config::MAX_RESPONSE_BYTES;
#[cfg(not(test))]
use crate::decisions;
use crate::decisions::Failure;

struct JevJudgments;
bindings::export!(JevJudgments with_types_in bindings);

impl exports::waddle::extension::lifecycle::Guest for JevJudgments {
    fn init(config: String) -> Result<types::ExtensionManifest, String> {
        JevConfig::parse(&config).map_err(|error| error.to_string())?;
        Ok(types::ExtensionManifest {
            id: types::PluginId {
                value: "jev-judgments".to_string(),
            },
            name: display("Jev Judgments"),
            version: types::PluginVersion {
                value: "0.1.0".to_string(),
            },
            payloads: vec![types::PayloadRule {
                surface: types::PayloadSurface::RoomResult,
                root: types::PayloadRoot {
                    namespace: types::PayloadNamespace {
                        value: crate::payload::NAMESPACE.to_string(),
                    },
                    local_name: crate::payload::ROOT.to_string(),
                },
            }],
            capabilities: vec![
                types::ExtensionCapability::MessageObserve,
                types::ExtensionCapability::RoomResultPublish,
            ],
            commands: vec![],
            routes: vec![],
            pubsub_nodes: vec![],
            profile: None,
            artifact: None,
        })
    }
}

impl exports::waddle::extension::framework::Guest for JevJudgments {
    fn handle_event(
        event: types::ExtensionEvent,
        _delivery: Option<&bindings::waddle::extension::delivery::DeliveryKey>,
    ) -> Result<types::ExtensionResponse, types::ExtensionError> {
        let types::ExtensionEvent::RoomMessageObserve(observation) = event else {
            return Err(error(
                types::ExtensionErrorCode::UnsupportedEvent,
                "Jev accepts room observations",
            ));
        };
        let config = JevConfig::parse(&runtime::get_config()).map_err(|_| {
            error(
                types::ExtensionErrorCode::InvalidRequest,
                "Jev configuration is invalid",
            )
        })?;
        invoke_brokered_decisions(&config, &observation.body.value)
    }
}

#[cfg(not(test))]
fn invoke_brokered_decisions(
    config: &JevConfig,
    body: &str,
) -> Result<types::ExtensionResponse, types::ExtensionError> {
    let stream = inference::start_stream(&inference::InferenceRequest {
        credential: inference::CredentialRef::Tenant(config.credential_name.clone()),
        provider: types::ProviderId {
            value: config.provider.clone(),
        },
        deployment: config.deployment.clone(),
        model: types::ModelId {
            value: config.model.clone(),
        },
        messages: vec![inference::InferenceMessage {
            role: "user".to_string(),
            content: decisions::request_body(&config.model, body),
        }],
        max_output_tokens: None,
    })
    .map_err(|err| map_inference_error(&err))?;

    let mut buffer = Vec::new();
    while let Some(chunk) = stream
        .next_chunk()
        .map_err(|err| map_inference_error(&err))?
    {
        if buffer.len().saturating_add(chunk.delta.len()) > MAX_RESPONSE_BYTES {
            return Err(provider_error(Failure::ResponseTooLarge));
        }
        buffer.extend_from_slice(&chunk.delta);
    }
    let verified = stream.terminal_usage();
    let payload_text = String::from_utf8(buffer).map_err(|_| {
        provider_error(Failure::InvalidResponse(
            decisions::InvalidResponse::DocumentShape,
        ))
    })?;
    let batch = decisions::parse_http_response(200, &payload_text, &config.model)
        .map_err(provider_error)?;
    let usage = verified.map(|v| types::InvocationUsage {
        provider: v.provider,
        model: v.model,
        cost_micro_usd: v.cost_microcents,
    });
    Ok(types::ExtensionResponse {
        effects: vec![types::ExtensionEffect::PublishRoomResult(
            crate::payload::safety_scores(&batch),
        )],
        usage,
    })
}

#[cfg(not(test))]
fn map_inference_error(err: &inference::InferenceError) -> types::ExtensionError {
    let code = match err.code {
        inference::InferenceErrorCode::Denied | inference::InferenceErrorCode::Canceled => {
            types::ExtensionErrorCode::Denied
        }
        inference::InferenceErrorCode::InvalidRequest => types::ExtensionErrorCode::InvalidRequest,
        inference::InferenceErrorCode::RateLimited
        | inference::InferenceErrorCode::TemporaryFailure
        | inference::InferenceErrorCode::ChunkLimitExceeded => {
            types::ExtensionErrorCode::TemporaryFailure
        }
    };
    error(
        code,
        &format!("Jev brokered inference failed: {:?}", err.code),
    )
}

#[cfg(test)]
fn invoke_brokered_decisions(
    _config: &JevConfig,
    _body: &str,
) -> Result<types::ExtensionResponse, types::ExtensionError> {
    Err(provider_error(Failure::Transport))
}

fn display(value: &str) -> types::DisplayText {
    types::DisplayText {
        value: value.to_string(),
    }
}
fn error(code: types::ExtensionErrorCode, message: &str) -> types::ExtensionError {
    types::ExtensionError {
        code,
        message: display(message),
    }
}
fn provider_error(failure: Failure) -> types::ExtensionError {
    let code = match failure {
        Failure::HttpStatus(401 | 403) => types::ExtensionErrorCode::Denied,
        Failure::HttpStatus(status)
            if (400..500).contains(&status) && !matches!(status, 402 | 408 | 429) =>
        {
            types::ExtensionErrorCode::InvalidRequest
        }
        _ => types::ExtensionErrorCode::TemporaryFailure,
    };
    // Fixed category/status only: never include response or submitted text.
    error(code, &failure.to_string())
}
