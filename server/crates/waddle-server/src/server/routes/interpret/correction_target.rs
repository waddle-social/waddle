use std::sync::Arc;

use waddle_xmpp::mam::{MamStorage, MamStorageError};
use xmpp_parsers::message::MessageType;

use super::MamArchivedMessage;

/// XEP-0424 §Business rules: "A retraction applies to a message together
/// with all of its corrections ... A receiving entity SHOULD treat a
/// retraction that refers to a correction as retracting the original message
/// and all of its corrections."
///
/// Maps a retraction or moderation target row to the row to tombstone: when
/// `row` is a live XEP-0308 correction, the original it replaces (looked up
/// in `archive` by the `<replace id/>` value, same author only); otherwise
/// `row` itself. Tombstoning the original cascades to every correction, so
/// each keeps its `<replace/>` (XEP-0425 §Tombstones).
pub(crate) async fn resolve_correction_original(
    mam_storage: &Arc<dyn MamStorage>,
    archive: &jid::BareJid,
    row: MamArchivedMessage,
) -> Result<MamArchivedMessage, MamStorageError> {
    let Some(replaces_id) = row
        .rich
        .as_ref()
        .and_then(|rich| rich.correction_replaces_id())
        .cloned()
    else {
        return Ok(row);
    };
    let Some(original) = mam_storage
        .get_message_by_message_id(archive, replaces_id.as_str())
        .await?
    else {
        return Ok(row);
    };
    let same_author = if row.message_type == MessageType::Groupchat {
        original.from == row.from
    } else {
        original.from.to_bare() == row.from.to_bare()
    };
    let is_correction = original
        .rich
        .as_ref()
        .is_some_and(|rich| rich.correction_replaces_id().is_some());
    if original.id == row.id || !same_author || is_correction {
        return Ok(row);
    }
    Ok(original)
}
