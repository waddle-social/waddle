const CSI_NS: &str = "urn:xmpp:csi:0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClientState {
    #[default]
    Active,
    Inactive,
}

#[derive(Debug)]
pub struct CsiManager {
    state: ClientState,
    server_supports_csi: bool,
}

impl CsiManager {
    pub fn new() -> Self {
        Self {
            state: ClientState::Active,
            server_supports_csi: false,
        }
    }

    pub fn state(&self) -> ClientState {
        self.state
    }

    pub fn server_supports_csi(&self) -> bool {
        self.server_supports_csi
    }

    pub fn set_server_support(&mut self, supported: bool) {
        self.server_supports_csi = supported;
    }

    pub fn set_inactive(&mut self) -> Option<Vec<u8>> {
        if !self.server_supports_csi {
            return None;
        }

        if matches!(self.state, ClientState::Inactive) {
            return None;
        }

        self.state = ClientState::Inactive;
        Some(build_inactive())
    }

    pub fn set_active(&mut self) -> Option<Vec<u8>> {
        if !self.server_supports_csi {
            return None;
        }

        if matches!(self.state, ClientState::Active) {
            return None;
        }

        self.state = ClientState::Active;
        Some(build_active())
    }

    pub fn on_stream_started(&mut self) -> Option<Vec<u8>> {
        if !self.server_supports_csi {
            return None;
        }

        if matches!(self.state, ClientState::Inactive) {
            Some(build_inactive())
        } else {
            None
        }
    }

    pub fn reset(&mut self) {
        self.state = ClientState::Active;
        self.server_supports_csi = false;
    }
}

impl Default for CsiManager {
    fn default() -> Self {
        Self::new()
    }
}

fn build_inactive() -> Vec<u8> {
    format!("<inactive xmlns='{CSI_NS}'/>").into_bytes()
}

fn build_active() -> Vec<u8> {
    format!("<active xmlns='{CSI_NS}'/>").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_manager_starts_active() {
        let manager = CsiManager::new();
        assert_eq!(manager.state(), ClientState::Active);
        assert!(!manager.server_supports_csi());
    }

    #[test]
    fn set_inactive_without_server_support_returns_none() {
        let mut manager = CsiManager::new();
        assert!(manager.set_inactive().is_none());
        assert_eq!(manager.state(), ClientState::Active);
    }

    #[test]
    fn set_inactive_with_server_support_returns_stanza() {
        let mut manager = CsiManager::new();
        manager.set_server_support(true);

        let stanza = manager.set_inactive();
        assert!(stanza.is_some());
        assert_eq!(manager.state(), ClientState::Inactive);

        let xml = String::from_utf8(stanza.unwrap()).unwrap();
        assert!(xml.contains("<inactive"));
        assert!(xml.contains(CSI_NS));
    }

    #[test]
    fn set_inactive_when_already_inactive_returns_none() {
        let mut manager = CsiManager::new();
        manager.set_server_support(true);
        manager.set_inactive();
        assert!(manager.set_inactive().is_none());
    }

    #[test]
    fn set_active_without_server_support_returns_none() {
        let mut manager = CsiManager::new();
        assert!(manager.set_active().is_none());
    }

    #[test]
    fn set_active_from_inactive_returns_stanza() {
        let mut manager = CsiManager::new();
        manager.set_server_support(true);
        manager.set_inactive();

        let stanza = manager.set_active();
        assert!(stanza.is_some());
        assert_eq!(manager.state(), ClientState::Active);

        let xml = String::from_utf8(stanza.unwrap()).unwrap();
        assert!(xml.contains("<active"));
        assert!(xml.contains(CSI_NS));
    }

    #[test]
    fn set_active_when_already_active_returns_none() {
        let mut manager = CsiManager::new();
        manager.set_server_support(true);
        assert!(manager.set_active().is_none());
    }

    #[test]
    fn on_stream_started_sends_inactive_if_state_is_inactive() {
        let mut manager = CsiManager::new();
        manager.set_server_support(true);
        manager.set_inactive();

        let stanza = manager.on_stream_started();
        assert!(stanza.is_some());

        let xml = String::from_utf8(stanza.unwrap()).unwrap();
        assert!(xml.contains("<inactive"));
    }

    #[test]
    fn on_stream_started_sends_nothing_if_active() {
        let mut manager = CsiManager::new();
        manager.set_server_support(true);
        assert!(manager.on_stream_started().is_none());
    }

    #[test]
    fn on_stream_started_sends_nothing_without_server_support() {
        let mut manager = CsiManager::new();
        assert!(manager.on_stream_started().is_none());
    }

    #[test]
    fn reset_returns_to_initial_state() {
        let mut manager = CsiManager::new();
        manager.set_server_support(true);
        manager.set_inactive();

        manager.reset();
        assert_eq!(manager.state(), ClientState::Active);
        assert!(!manager.server_supports_csi());
    }

    #[test]
    fn toggle_active_inactive_active() {
        let mut manager = CsiManager::new();
        manager.set_server_support(true);

        let s1 = manager.set_inactive();
        assert!(s1.is_some());
        assert_eq!(manager.state(), ClientState::Inactive);

        let s2 = manager.set_active();
        assert!(s2.is_some());
        assert_eq!(manager.state(), ClientState::Active);

        let s3 = manager.set_inactive();
        assert!(s3.is_some());
        assert_eq!(manager.state(), ClientState::Inactive);
    }
}
