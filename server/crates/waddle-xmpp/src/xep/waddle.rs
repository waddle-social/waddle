//! Waddle-specific protocol extensions under `urn:waddle:*` namespaces.
pub mod call_thread;
pub mod dm_bookmarks;
pub mod dnd;
pub mod forums;
pub mod group_dm;
pub mod in_call;
pub mod link_preview;
pub mod livekit_transport;
pub mod pin;
pub mod safety_scores;

#[cfg(test)]
pub(super) mod mam_stanza_id_tests;
