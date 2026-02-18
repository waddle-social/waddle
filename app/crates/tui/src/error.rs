use waddle_core::error::EventBusError;
use waddle_core::theme::ThemeError;

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum TuiError {
    #[error("terminal initialization failed: {0}")]
    TerminalInit(String),

    #[error("render error: {0}")]
    Render(String),

    #[error("event bus error: {0}")]
    EventBus(#[from] EventBusError),

    #[error("theme error: {0}")]
    Theme(#[from] ThemeError),
}
