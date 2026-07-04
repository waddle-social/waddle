use super::*;
use crate::server::routes::websocket::handlers::pubsub_fanout;

/// `true` when `item` is a well-formed RSVP for the publishing
/// session: item id of the form `<uid>-rsvp-<localpart>` where
/// `<localpart>` matches the session, payload is a single VEVENT
/// carrying exactly one `<attendee>` whose URI bare JID matches
/// `<localpart>@<user_domain>`, and the event contains no
/// master-event-only fields (no SUMMARY/DTSTART/RRULE).
pub(super) fn is_well_formed_rsvp_item(
    session: Option<&Session>,
    user_domain: &str,
    item: &PubSubItem,
) -> bool {
    let Some(session) = session else {
        return false;
    };
    let Some(item_id) = &item.id else {
        return false;
    };
    let Some((master_uid, localpart)) = parse_rsvp_item_id(item_id) else {
        return false;
    };
    if !localpart.eq_ignore_ascii_case(&session.xmpp_localpart) {
        return false;
    }
    // Master UID must be non-empty; we don't constrain its shape
    // further (matches the master item's id).
    if master_uid.is_empty() {
        return false;
    }
    let Some(payload) = &item.payload else {
        return false;
    };
    if !waddle_xmpp_core::xcal::is_vcalendar_element(payload) {
        return false;
    }
    let ns_xcal = waddle_xmpp_core::xcal::NS_XCAL;
    let Some(vevent) = payload
        .children()
        .find(|c| c.name() == "vevent" && c.ns() == ns_xcal)
    else {
        return false;
    };
    // Master-event-only fields MUST NOT appear on an RSVP item.
    let forbidden = [
        "summary",
        "dtstart",
        "dtend",
        "rrule",
        "description",
        "location",
        "organizer",
    ];
    for child in vevent.children() {
        if child.ns() != ns_xcal {
            return false;
        }
        if forbidden.contains(&child.name()) {
            return false;
        }
    }
    let attendees: Vec<_> = vevent
        .children()
        .filter(|c| c.name() == "attendee" && c.ns() == ns_xcal)
        .collect();
    if attendees.len() != 1 {
        return false;
    }
    let uri = attendees[0].text();
    let uri_trimmed = uri.trim();
    let attendee_bare = waddle_xmpp_core::xcal::xmpp_uri_to_bare_jid(uri_trimmed);
    let expected_jid = format!(
        "{}@{}",
        localpart.to_ascii_lowercase(),
        user_domain.to_ascii_lowercase()
    );
    attendee_bare.as_deref() == Some(expected_jid.as_str())
}

/// Split a string like `evt-launch-rsvp-alice` into
/// `("evt-launch", "alice")`. Returns `None` for inputs without the
/// `-rsvp-` separator.
fn parse_rsvp_item_id(item_id: &str) -> Option<(&str, &str)> {
    let (master_uid, localpart) = item_id.rsplit_once("-rsvp-")?;
    if localpart.is_empty() {
        return None;
    }
    Some((master_uid, localpart))
}

/// Extract (author bare-JID, master event UID, partstat) from a
/// well-formed RSVP pubsub item. The item is already validated by
/// `is_well_formed_rsvp_item` at this point — we only return `Some`
/// when every field needed for the feed bridge is intact.
fn rsvp_bridge_context(
    item: &PubSubItem,
) -> Option<(BareJid, String, waddle_xmpp_core::xcal::PartStat)> {
    let item_id = item.id.as_deref()?;
    let (master_uid, _localpart) = parse_rsvp_item_id(item_id)?;
    let payload = item.payload.as_ref()?;
    if !waddle_xmpp_core::xcal::is_vcalendar_element(payload) {
        return None;
    }
    let ns_xcal = waddle_xmpp_core::xcal::NS_XCAL;
    let vevent = payload
        .children()
        .find(|c| c.name() == "vevent" && c.ns() == ns_xcal)?;
    let attendee = vevent
        .children()
        .find(|c| c.name() == "attendee" && c.ns() == ns_xcal)?;
    let partstat = attendee
        .attr("partstat")
        .and_then(waddle_xmpp_core::xcal::PartStat::from_str_value)?;
    let uri = attendee.text();
    let bare = waddle_xmpp_core::xcal::xmpp_uri_to_bare_jid(uri.trim())?;
    let author_jid = bare.parse::<BareJid>().ok()?;
    Some((author_jid, master_uid.to_string(), partstat))
}

pub(super) async fn handle_community_rsvp_publish(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    community_domain: &str,
    node: &str,
    item: PubSubItem,
) -> Vec<String> {
    let bridge_context = rsvp_bridge_context(&item);
    let Ok(community_jid) = community_domain.parse::<BareJid>() else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    match state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&community_jid, node)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))],
        Err(error) => {
            warn!(node, error = %error, "Failed to resolve community node for RSVP publish");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
        }
    }
    match state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&community_jid, node, &item, None, false)
        .await
    {
        Ok(result) => {
            pubsub_fanout::fan_out_publish(
                state,
                pubsub_fanout::FanOutRequest {
                    owner: &community_jid,
                    node,
                    published_item: &item,
                    item_id: &result.item_id,
                    publisher: None,
                    publisher_full: None,
                    is_pep: false,
                },
            )
            .await;
            // Bridge into the social feed so "X is going to <event>"
            // surfaces alongside manual posts. Best-effort: failures
            // are logged inside `observe_rsvp` and never block the
            // RSVP publish itself.
            if let Some((author_jid, master_uid, partstat)) = bridge_context {
                let _ = state
                    .deps
                    .protocol
                    .pep_feed_bridge
                    .observe_rsvp(
                        &state.deps.protocol.pubsub_storage,
                        &community_jid,
                        &author_jid,
                        &master_uid,
                        partstat,
                    )
                    .await;
            }
            vec![iq_to_xml(build_pubsub_publish_result(
                iq,
                node,
                &result.item_id,
            ))]
        }
        Err(error) => {
            warn!(node, error = %error, "Failed to publish community RSVP item");
            vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))]
        }
    }
}
