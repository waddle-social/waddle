use crate::{
    bindings::{
        self, exports,
        waddle::extension::{runtime, types},
    },
    config::JevConfig,
    decisions::{self, Failure},
};

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
                types::ExtensionCapability::OutboundHttpRequest,
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
        // This is the same operator-approved provider request as the former
        // core worker. The host enforces the configured HTTPS origin, four
        // second timeout, single request, and 64 KiB response limit.
        let response = runtime::http_request(&types::OutgoingHttpRequest {
            method: types::HttpMethod::Post,
            url: types::Url {
                value: config.endpoint.to_string(),
            },
            headers: vec![
                types::HttpHeader {
                    name: "authorization".to_string(),
                    value: format!("Bearer {}", config.api_key.as_str()),
                },
                types::HttpHeader {
                    name: "content-type".to_string(),
                    value: "application/json".to_string(),
                },
            ],
            body: Some(decisions::request_body(
                &config.model,
                &observation.body.value,
            )),
        })
        .map_err(|_| provider_error(Failure::Transport))?;
        let batch = decisions::parse_http_response(response.status, &response.body, &config.model)
            .map_err(provider_error)?;
        let usage = types::InvocationUsage {
            provider: types::ProviderId {
                value: "openrouter".to_string(),
            },
            model: types::ModelId {
                value: batch.model_version.clone(),
            },
            cost_micro_usd: batch.cost_micro_usd().map_err(provider_error)?,
        };
        Ok(types::ExtensionResponse {
            effects: vec![types::ExtensionEffect::PublishRoomResult(
                crate::payload::safety_scores(&batch),
            )],
            usage: Some(usage),
        })
    }
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
