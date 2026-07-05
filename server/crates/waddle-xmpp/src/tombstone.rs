//! Shared XEP-0424 / XEP-0425 tombstone matching.
//!
//! One matcher, two consumers: the XEP-0198 SM session registry scrub
//! (`stream_management::session_registry`) and the XEP-0160
//! `pending_delivery` scrub (in-memory impl here, libSQL/Postgres impl
//! in `waddle-server`). Keeping the per-stanza predicate in one place
//! guarantees a retraction removes the SAME set of cached copies from
//! every replay-capable store.

/// Select the sequences of unacked outbound `<message/>` entries that
/// match a XEP-0424 / XEP-0425 tombstone. Pure — takes a snapshot of
/// `(sequence, stanza_xml)` pairs so the caller can parse OUTSIDE the
/// registry's write locks (issue #1145) and later remove exactly the
/// returned `(stream_id, sequence)` pairs, which is safe even if the
/// queue changed between snapshot and removal.
///
/// Parse errors and non-message frames are skipped silently — only
/// matching messages are selected.
pub fn matching_tombstone_sequences(
    entries: &[(u32, String)],
    target_id: &str,
    archive_jid: &str,
) -> Vec<u32> {
    entries
        .iter()
        .filter(
            |(_, stanza_xml)| match stanza_xml.parse::<minidom::Element>() {
                Ok(el) => message_element_matches_tombstone(&el, target_id, archive_jid),
                Err(_) => false,
            },
        )
        .map(|(sequence, _)| *sequence)
        .collect()
}

/// Per-stanza tombstone predicate. A cached message matches iff:
///   1. it is a `<message>` element,
///   2. its `from` or `to` attribute bare-equals `archive_jid` (scope
///      guard — prevents cross-conversation collateral damage when
///      short message ids collide across chats), AND
///   3. either its wire `id` attribute matches `target_id` (1:1 case)
///      or any child `<stanza-id id='…'/>` matches `target_id`
///      (groupchat case where the retraction keyed by the room's
///      XEP-0359 stamp per the "archive id == wire stanza-id"
///      invariant).
pub fn message_element_matches_tombstone(
    el: &minidom::Element,
    target_id: &str,
    archive_jid: &str,
) -> bool {
    if el.name() != "message" {
        return false;
    }
    let in_scope = el
        .attr("from")
        .map(|s| jid_bare_equals(s, archive_jid))
        .unwrap_or(false)
        || el
            .attr("to")
            .map(|s| jid_bare_equals(s, archive_jid))
            .unwrap_or(false);
    if !in_scope {
        return false;
    }
    if el.attr("id") == Some(target_id) {
        return true;
    }
    // XEP-0359 §3 scopes `<stanza-id/>` to `urn:xmpp:sid:0`. Match that
    // namespace explicitly so an unrelated extension element happening
    // to be named "stanza-id" in a different namespace cannot trigger
    // a tombstone scrub (Copilot review on PR #305).
    el.children()
        .any(|c| c.is("stanza-id", "urn:xmpp:sid:0") && c.attr("id") == Some(target_id))
}

fn jid_bare_equals(jid_str: &str, archive_jid: &str) -> bool {
    match jid_str.parse::<jid::Jid>() {
        Ok(jid) => jid.to_bare().to_string() == archive_jid,
        Err(_) => false,
    }
}
