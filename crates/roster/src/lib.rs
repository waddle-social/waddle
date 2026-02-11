use std::marker::PhantomData;

use waddle_core::event::Event;
use waddle_storage::Database;
use waddle_xmpp::Stanza;

#[derive(Debug)]
pub struct RosterManager<D>
where
    D: Database,
{
    database: PhantomData<D>,
}

impl<D> Default for RosterManager<D>
where
    D: Database,
{
    fn default() -> Self {
        Self {
            database: PhantomData,
        }
    }
}

impl<D> RosterManager<D>
where
    D: Database,
{
    pub fn new() -> Self {
        Self::default()
    }

    pub fn handle_event(&self, _event: &Event) {}

    pub fn handle_stanza(&self, _stanza: &Stanza) {}
}
