//! `urn:waddle:group-dm:*` lifecycle commands and the mediated-invite
//! extension.
//!
//! Group DMs are hidden members-only XEP-0045 rooms classified by the
//! `urn:waddle:group-dm:0` disco feature and discovered only via
//! XEP-0402 bookmarks. Their lifecycle is driven by XEP-0050 ad-hoc
//! commands (§2.4 single-stage execute with a XEP-0004 submit form
//! whose hidden `FORM_TYPE` mirrors the command node, per XEP-0068):
//!
//! - create (`urn:waddle:group-dm:create:0`): IQ-set to the bare
//!   server domain; the completed result form carries the new
//!   `room_jid`.
//! - rename (`urn:waddle:group-dm:rename:0`): IQ-set to the ROOM jid;
//!   an empty `name` clears the custom name. The server rewrites every
//!   member's bookmark.
//! - leave (`urn:waddle:group-dm:leave:0`): IQ-set to the bare server
//!   domain; the server retracts the leaver's bookmark.
//!
//! Adding a member is NOT a command: it is a XEP-0045 §7.8.2 mediated
//! invite to the room, optionally extended with a Waddle
//! `<history-access xmlns='urn:waddle:group-dm:0' mode='full'/>`
//! child. The server treats an absent child as `from-join` (the
//! privacy-preserving default), so the builder omits it in that case.
//!
//! The command nodes are legitimately custom (`urn:waddle:*`): no XEP
//! defines group-DM lifecycle CRUD. Shared by the wasm chat client
//! and the FFI (Android/Apple) clients — one implementation per verb.

mod builders;
mod parse;
mod types;

#[cfg(test)]
mod tests;

pub use builders::{
    build_group_dm_create_iq, build_group_dm_invite_message, build_group_dm_leave_iq,
    build_group_dm_rename_iq,
};
pub use parse::{
    parse_group_dm_create_result, parse_group_dm_leave_result, parse_group_dm_rename_result,
    GroupDmResponseError,
};
pub use types::{
    GroupDmCreateArgs, GroupDmCreated, GroupDmHistoryAccess, GroupDmLeft, GroupDmRenamed,
};

/// `urn:waddle:group-dm:create:0` command node.
pub const NODE_GROUP_DM_CREATE: &str = "urn:waddle:group-dm:create:0";
/// `urn:waddle:group-dm:rename:0` command node.
pub const NODE_GROUP_DM_RENAME: &str = "urn:waddle:group-dm:rename:0";
/// `urn:waddle:group-dm:leave:0` command node.
pub const NODE_GROUP_DM_LEAVE: &str = "urn:waddle:group-dm:leave:0";
