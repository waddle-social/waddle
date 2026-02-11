use thiserror::Error;

/// The universal error type for the Waddle application.
#[derive(Error, Debug)]
pub enum WaddleError {
    #[error("Configuration error: {0}")]
    Config(#[from] crate::config::ConfigError),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("XMPP error: {0}")]
    Xmpp(String),

    #[error("I18n error: {0}")]
    I18n(String),

    #[error("Theme error: {0}")]
    Theme(#[from] crate::theme::ThemeError),

    #[error("Plugin error: {0}")]
    Plugin(String),

    #[error("Event bus error: {0}")]
    EventBus(#[from] EventBusError),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Unknown error")]
    Unknown,
}

/// A specialized Result type for Waddle operations.
pub type Result<T> = std::result::Result<T, WaddleError>;

#[derive(thiserror::Error, Debug, Clone)]
pub enum EventBusError {
    #[error("Invalid channel: {0}")]
    InvalidChannel(String),

    #[error("Invalid pattern: {0}")]
    InvalidPattern(String),

    #[error("Channel closed")]
    ChannelClosed,

    #[error("Subscriber lagged: {0} events missed")]
    Lagged(u64),
}
