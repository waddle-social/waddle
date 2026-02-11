use crate::error::ConnectionError;
pub use crate::transport::ConnectionConfig;

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting { attempt: u32 },
}

pub struct ConnectionManager {
    state: ConnectionState,
    config: ConnectionConfig,
}

impl ConnectionManager {
    pub fn new(config: ConnectionConfig) -> Self {
        Self {
            state: ConnectionState::Disconnected,
            config,
        }
    }

    pub async fn connect(&mut self) -> Result<(), ConnectionError> {
        let _ = &self.config;
        todo!("ConnectionManager::connect")
    }

    pub async fn disconnect(&mut self) -> Result<(), ConnectionError> {
        todo!("ConnectionManager::disconnect")
    }

    pub fn state(&self) -> ConnectionState {
        self.state.clone()
    }
}
