use waddle_core::event::Event;

#[derive(Debug, Default)]
pub struct NotificationManager;

impl NotificationManager {
    pub fn new() -> Self {
        Self
    }

    pub fn handle_event(&self, _event: &Event) {}
}
