//! XEP-0492: Chat notification settings.
//!
//! Public facade for the official XEP-0492 notify model plus Waddle's
//! DM-bookmark carrier. Submodules keep the official `<notify/>` merge
//! rules separate from the project-local PEP carrier/IQ builders.

mod dm_bookmark;
mod notify;

pub use dm_bookmark::{
    build_dm_bookmark_element, build_fetch_dm_bookmarks_iq, build_publish_dm_bookmark_iq,
    build_retract_dm_bookmark_iq, dm_notify_is_default, read_dm_bookmark_notify,
    DM_BOOKMARK_MAX_ITEMS, NS_WADDLE_DM_BOOKMARKS,
};
pub use notify::{
    merge_notify, merge_notify_into_extensions, read_fallback_mode, read_rich_payload_opt_in,
    surface_bookmark_notify, surface_dm_notify, ConversationKind, NotifyMode,
    NotifySettingsSurface, NS_NOTIFICATION_SETTINGS, NS_PUSH_RICH_PAYLOAD,
};

#[cfg(test)]
mod tests;
