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

mod command;
mod constants;
mod error;
mod manifest;
mod model;
mod prompt;
mod provider;
mod text;
mod tools;
mod ui;

use std::sync::OnceLock;

use bindings::exports;
#[cfg(not(test))]
use bindings::waddle::extension::runtime;
use bindings::waddle::extension::types;
use command::handle_event_with_executor;
use error::extension_error as make_extension_error;
use manifest::manifest as extension_manifest;
use model::{
    ProviderAnswer as RuntimeProviderAnswer, ProviderConfig as RuntimeProviderConfig,
    ProviderConfigError as RuntimeProviderConfigError,
    ProviderExecutionError as RuntimeProviderExecutionError,
    ProviderExecutor as RuntimeProviderExecutorTrait, ProviderRequest as RuntimeProviderRequest,
};
use provider::execute_provider_request;

struct AiChatbot;

bindings::export!(AiChatbot with_types_in bindings);

static PROVIDER_CONFIG: OnceLock<Result<RuntimeProviderConfig, RuntimeProviderConfigError>> =
    OnceLock::new();

impl exports::waddle::extension::lifecycle::Guest for AiChatbot {
    fn init(config: String) -> Result<types::ExtensionManifest, String> {
        let parsed = RuntimeProviderConfig::parse(&config);
        if let Err(error) = parsed.as_ref() {
            return Err(format!(
                "ai-chatbot provider configuration is invalid: {error}"
            ));
        }
        let _ = PROVIDER_CONFIG.set(parsed);
        Ok(extension_manifest())
    }
}

impl exports::waddle::extension::framework::Guest for AiChatbot {
    fn handle_event(
        event: types::ExtensionEvent,
    ) -> Result<types::ExtensionResponse, types::ExtensionError> {
        let executor = RuntimeProviderExecutor;
        handle_event_with_executor(event, &executor)
    }
}

struct RuntimeProviderExecutor;

impl RuntimeProviderExecutorTrait for RuntimeProviderExecutor {
    fn execute(
        &self,
        request: RuntimeProviderRequest,
    ) -> Result<RuntimeProviderAnswer, RuntimeProviderExecutionError> {
        execute_provider_request(request)
    }
}

fn provider_config() -> Result<RuntimeProviderConfig, types::ExtensionError> {
    #[cfg(not(test))]
    {
        RuntimeProviderConfig::parse(&runtime::get_config()).map_err(|error| {
            make_extension_error(
                types::ExtensionErrorCode::InvalidRequest,
                &format!("ai-chatbot provider configuration is invalid: {error}"),
            )
        })
    }
    #[cfg(test)]
    PROVIDER_CONFIG
        .get()
        .cloned()
        .unwrap_or(Err(RuntimeProviderConfigError::Missing))
        .map_err(|error| {
            make_extension_error(
                types::ExtensionErrorCode::InvalidRequest,
                &format!("ai-chatbot provider configuration is invalid: {error}"),
            )
        })
}

#[cfg(test)]
pub(crate) use command::{command_response_with_config, take_sent_room_messages};
#[cfg(test)]
pub(crate) use constants::{
    BASELINE_SYSTEM_PROMPT, COMMAND_NODE, MAX_CONTEXT_BYTES, MAX_CONTEXT_LINE_BYTES,
    MAX_PROVIDER_TOOL_CALLS_PER_ROUND, OPENROUTER_REFERER, OPENROUTER_TITLE,
};
#[cfg(test)]
pub(crate) use error::extension_error;
#[cfg(test)]
pub(crate) use manifest::manifest;
#[cfg(test)]
pub(crate) use model::{
    CleanPrompt, ExecutionContext, HostTool, NonEmptyString, ProviderAnswer, ProviderConfig,
    ProviderExecutionError, ProviderExecutor, ProviderRequest, ProviderRole, ResponseTarget,
};
#[cfg(test)]
pub(crate) use prompt::clean_prompt;
#[cfg(test)]
pub(crate) use provider::{
    assemble_provider_request, execute_provider_request_with_runtime, parse_provider_answer,
    provider_execution_error, provider_request_headers, provider_request_json,
    provider_request_json_from_parts,
};
#[cfg(test)]
pub(crate) use tools::{format_archived_messages, provider_tool_mam_query, select_host_tools};

#[cfg(test)]
mod tests;
