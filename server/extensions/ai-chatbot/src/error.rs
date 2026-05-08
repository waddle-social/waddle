use crate::bindings::waddle::extension::types;
use crate::ui::display;

pub(crate) fn extension_error(
    code: types::ExtensionErrorCode,
    message: &str,
) -> types::ExtensionError {
    types::ExtensionError {
        code,
        message: display(message),
    }
}

#[cfg(not(test))]
pub(crate) fn extension_error_from_host_tool(error: types::HostToolError) -> types::ExtensionError {
    let code = match error.code {
        types::HostToolErrorCode::Denied => types::ExtensionErrorCode::InvalidRequest,
        types::HostToolErrorCode::InvalidRequest => types::ExtensionErrorCode::InvalidRequest,
        types::HostToolErrorCode::NotFound => types::ExtensionErrorCode::InvalidRequest,
        types::HostToolErrorCode::Unsupported => types::ExtensionErrorCode::UnsupportedEvent,
        types::HostToolErrorCode::TemporaryFailure => types::ExtensionErrorCode::TemporaryFailure,
    };
    types::ExtensionError {
        code,
        message: error.message,
    }
}
