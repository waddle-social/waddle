use super::*;

/// Load the bound user's persisted XEP-0191 blocklist from the
/// global database adapter and convert it into the typed
/// [`Blocklist`] shape the per-connection state machine consumes.
///
/// Used at bind time by the main loop to seed
/// [`WsConnState::ensure_state_machine`] (#229 PR13). The caller
/// **fails the bind** on `Err` rather than silently degrading to an
/// empty blocklist — that fail-closed semantic mirrors the legacy
/// per-message `is_blocked` check (`handlers/message.rs` /
/// `handlers/iq.rs` both `return vec![]` / refuse to route on
/// storage error). Treating storage failure as a session-long
/// fail-open (the agent's first cut) was a privacy/security
/// regression: any transient DB hiccup at bind would silently bypass
/// XEP-0191 enforcement for the entire connection until the client
/// reconnected. Failing the bind kicks the client to reconnect
/// instead, which is a self-healing recovery path.
///
/// Mid-session XEP-0191 IQ-set mutations remain authoritative (they
/// update the SM's internal blocklist directly); the snapshot loaded
/// here is the bind-time baseline only.
pub(super) async fn load_blocklist_for_bind(
    db_pool: &Arc<crate::db::DatabasePool>,
    full_jid: &FullJid,
) -> Result<Blocklist, crate::db::blocking::BlockingStorageError> {
    let bare = full_jid.to_bare();
    let storage = crate::db::blocking::DatabaseBlockingStorage::new(db_pool.global().clone());
    storage
        .list_blocked_jid_entries(&bare)
        .await
        .map(Blocklist::new)
}

/// Build a `<stream:error>` close frame for a fatal session-init
/// failure (XEP-0191 blocklist load failure at bind time, etc.).
/// Pairs with breaking the WebSocket main loop so the client sees
/// the close + reconnects.
pub(super) fn build_internal_server_error_stream_error(text: &str) -> String {
    let mut writer = Writer::new(Vec::new());
    let mut stream_error = BytesStart::new("stream:error");
    stream_error.push_attribute(("xmlns:stream", waddle_xmpp::ns::STREAM));
    writer
        .write_event(Event::Start(stream_error))
        .expect("serializing stream error should not fail");

    let mut internal = BytesStart::new("internal-server-error");
    internal.push_attribute(("xmlns", "urn:ietf:params:xml:ns:xmpp-streams"));
    writer
        .write_event(Event::Empty(internal))
        .expect("serializing internal-server-error should not fail");

    let mut text_elem = BytesStart::new("text");
    text_elem.push_attribute(("xmlns", "urn:ietf:params:xml:ns:xmpp-streams"));
    text_elem.push_attribute(("xml:lang", "en"));
    writer
        .write_event(Event::Start(text_elem))
        .expect("serializing stream error text should not fail");
    writer
        .write_event(Event::Text(quick_xml::events::BytesText::new(text)))
        .expect("serializing stream error text body should not fail");
    writer
        .write_event(Event::End(quick_xml::events::BytesEnd::new("text")))
        .expect("serializing stream error text close should not fail");

    writer
        .write_event(Event::End(quick_xml::events::BytesEnd::new("stream:error")))
        .expect("serializing stream error close should not fail");
    String::from_utf8(writer.into_inner()).expect("xml writer produces valid utf-8")
}
