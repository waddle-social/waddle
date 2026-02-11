use waddle_core::event::{Event, PresenceShow};
use waddle_xmpp::Stanza;

#[derive(Debug, Clone)]
pub struct PresenceInfo {
    pub show: PresenceShow,
    pub status: Option<String>,
}

impl Default for PresenceInfo {
    fn default() -> Self {
        Self {
            show: PresenceShow::Available,
            status: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct PresenceManager {
    own_presence: PresenceInfo,
}

impl PresenceManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn own_presence(&self) -> &PresenceInfo {
        &self.own_presence
    }

    pub fn set_own_presence(&mut self, show: PresenceShow, status: Option<String>) {
        self.own_presence = PresenceInfo { show, status };
    }

    pub fn handle_event(&self, _event: &Event) {}

    pub fn handle_stanza(&self, _stanza: &Stanza) {}
}
