//! `urn:waddle:extension:1` slash-command surface: XEP-0050 ad-hoc
//! commands published by the extensions component.
//!
//! Shared typed discovery helpers, invoke/submit builders, and result
//! parsers used by the FFI (Android/Apple) clients — mirroring the
//! wasm chat client's TypeScript implementation
//! (`chat/src/lib/xmpp/extension-commands/`) one-to-one.
//!
//! Wire shape:
//! - Discovery: `disco#items` on the account domain finds the
//!   extension service (a component advertising BOTH
//!   [`NS_WADDLE_EXTENSION_1`] and the XEP-0050 commands feature),
//!   with `extensions.<domain>` as the conventional fallback;
//!   `disco#items` on the service with node
//!   `http://jabber.org/protocol/commands` (XEP-0050 §2.2) lists the
//!   commands; per-command `disco#info` (XEP-0050 §2.3) carries a
//!   XEP-0128 extension form (FORM_TYPE [`NS_WADDLE_EXTENSION_1`] or
//!   [`EXTENSION_COMMAND_FORM_TYPE`]) with the composer metadata
//!   fields `waddle#command_scope`, `waddle#composer_prefix`,
//!   `waddle#inline_field`, and `waddle#composer_execute`.
//! - Invocation: XEP-0050 §2.4 `<iq type='set'/>` execute requests;
//!   in-room invocations carry a `jabber:x:data` submit form with a
//!   `waddle#room_jid` field (legitimately custom: no XEP defines a
//!   room-context field for ad-hoc commands). Multi-stage sessions
//!   thread the `sessionid` verbatim per XEP-0050 §3.
//! - Responses: `<command status='…'/>` with optional `<actions/>`,
//!   `<note/>` diagnostics, and a XEP-0004 form.

mod builders;
mod parse;
mod types;

#[cfg(test)]
mod tests;

pub use builders::{build_extension_invoke_iq, build_extension_submit_iq, ROOM_JID_FIELD};
pub use parse::{
    extension_command_descriptor, extension_command_refs, extension_service_candidates,
    fallback_extension_service_jid, is_extension_service, parse_extension_command_metadata,
    parse_extension_command_result, ExtensionResponseError,
};
pub use types::{
    ExtensionCommandDescriptor, ExtensionCommandForm, ExtensionCommandItemRef,
    ExtensionCommandMetadata, ExtensionCommandNote, ExtensionCommandResult, ExtensionCommandScope,
    ExtensionFieldType, ExtensionFormFieldDescriptor, ExtensionFormOption, ExtensionNoteType,
    ExtensionSubmitField,
};

/// Waddle extension service feature namespace.
pub const NS_WADDLE_EXTENSION_1: &str = "urn:waddle:extension:1";

/// Internal server-side dispatch node; never a user-facing command,
/// so discovery drops it from the command list.
pub const INVOKE_COMMAND_NODE: &str = "urn:waddle:extension:1:invoke";

/// XEP-0128 FORM_TYPE of the per-command metadata form emitted by the
/// server (`extension_command_metadata_form`).
pub const EXTENSION_COMMAND_FORM_TYPE: &str = "urn:waddle:extension:1:command";
