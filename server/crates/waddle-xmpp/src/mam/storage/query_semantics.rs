use std::collections::HashSet;

use jid::BareJid;
use waddle_xmpp_core::mam::{ArchivedMessage, MamQuery, MamResult};

use super::MamArchiveKind;

pub(super) fn uses_backward_pagination(query: &MamQuery) -> bool {
    // XEP-0059 §2.5: an empty <before/> element requests the last page of
    // results. We treat any present `before_id` — including `Some("")` — as a
    // backward-pagination signal so the SQL emits ORDER BY id DESC and
    // `finalize_result` reverses back to chronological order. The downstream
    // `id < ?` predicate is still skipped for the empty case, so no rows are
    // filtered out.
    query.before_id.is_some()
}

pub(super) fn missing_requested_id_in_set(
    available_ids: &HashSet<&str>,
    requested_ids: &[String],
) -> Option<String> {
    requested_ids
        .iter()
        .find(|id| !available_ids.contains(id.as_str()))
        .cloned()
}

pub(super) fn missing_requested_id(
    messages: &[ArchivedMessage],
    requested_ids: &[String],
) -> Option<String> {
    if requested_ids.is_empty() {
        return None;
    }

    let available_ids = messages
        .iter()
        .map(|message| message.id.as_str())
        .collect::<HashSet<_>>();
    missing_requested_id_in_set(&available_ids, requested_ids)
}

pub(super) fn finalize_result(
    mut messages: Vec<ArchivedMessage>,
    query: &MamQuery,
    count: Option<u32>,
) -> MamResult {
    let actual_limit = query.max.unwrap_or(100).min(500) as usize;
    let complete = actual_limit == 0 || messages.len() <= actual_limit;

    if messages.len() > actual_limit {
        messages.pop();
    }

    if uses_backward_pagination(query) {
        messages.reverse();
    }

    let first_id = messages.first().map(|message| message.id.clone());
    let last_id = messages.last().map(|message| message.id.clone());

    MamResult {
        messages,
        complete,
        first_id,
        last_id,
        count,
    }
}

pub(super) fn archive_order_before(message: &ArchivedMessage, cursor: &ArchivedMessage) -> bool {
    message.timestamp < cursor.timestamp
        || (message.timestamp == cursor.timestamp && message.id < cursor.id)
}

pub(super) fn archive_order_after(message: &ArchivedMessage, cursor: &ArchivedMessage) -> bool {
    message.timestamp > cursor.timestamp
        || (message.timestamp == cursor.timestamp && message.id > cursor.id)
}

/// XEP-0313 §4.3.1 `with` predicate, evaluated against a single
/// archived JID (either the row's `from` or `to`).
///
/// - `with_bare`: pre-computed bare form of the query's `with`.
/// - `with_has_resource`: whether the query's `with` carries a
///   resource part (i.e. it is a full JID).
/// - `with_full`: the original query JID, used for exact full-JID
///   equality when the query specified a resource.
///
/// A bare `with` matches any archived JID whose bare form equals
/// `with_bare`, regardless of resource. A full `with` matches only
/// when the archived JID equals it exactly. Comparing structurally
/// via parsed JIDs (rather than `starts_with`) prevents the prefix-
/// collision class of bug where `alice@example.com` would otherwise
/// match `alice@example.com.evil/whatever`.
pub(super) fn jid_matches_with_filter(
    archived: &jid::Jid,
    with_bare: &BareJid,
    with_has_resource: bool,
    with_full: &jid::Jid,
) -> bool {
    if with_has_resource {
        archived == with_full
    } else {
        &archived.to_bare() == with_bare
    }
}

/// XEP-0313 §4.3.1 `with` predicate for an archived message.
///
/// In a personal archive, a query for the owner's own bare JID is the
/// self-chat special case:
/// both endpoints must be bare-equivalent to the archive owner. Room
/// archives and all other queries keep ordinary sender-or-recipient
/// semantics.
pub(super) fn message_matches_with_filter(
    archive_jid: &BareJid,
    archive_kind: MamArchiveKind,
    from: &jid::Jid,
    to: &jid::Jid,
    with: &jid::Jid,
) -> bool {
    let with_bare = with.to_bare();
    if archive_kind == MamArchiveKind::Personal
        && with.resource().is_none()
        && &with_bare == archive_jid
    {
        return &from.to_bare() == archive_jid && &to.to_bare() == archive_jid;
    }

    let with_has_resource = with.resource().is_some();
    jid_matches_with_filter(from, &with_bare, with_has_resource, with)
        || jid_matches_with_filter(to, &with_bare, with_has_resource, with)
}

pub(super) fn matches_thread_filter(message: &ArchivedMessage, thread_id: &str) -> bool {
    let archived_thread_id = message.thread.as_ref().map(|t| t.id.as_str());
    let archived_reply_id = message.reply.as_ref().map(|r| r.id.as_str());
    let archived_stanza_id = message.stanza_id.as_ref().map(|s| s.id.as_str());
    message.id == thread_id
        || archived_stanza_id == Some(thread_id)
        || archived_thread_id == Some(thread_id)
        || (archived_thread_id.is_none() && archived_reply_id == Some(thread_id))
}
