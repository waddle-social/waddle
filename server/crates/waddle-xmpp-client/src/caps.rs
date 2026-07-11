use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use minidom::Element;
use sha1::{Digest, Sha1};

use crate::discovery::DISCO_INFO_NS;
use crate::messaging::{
    NS_CHAT_MARKERS, NS_CHAT_STATES, NS_MESSAGE_CORRECT, NS_MESSAGE_RETRACT, NS_REACTIONS,
};

pub const NS_CAPS: &str = "http://jabber.org/protocol/caps";
pub const CAPS_NODE: &str = "https://waddle.social/caps";

const IDENTITY_CATEGORY: &str = "client";
const IDENTITY_TYPE: &str = "pc";
const IDENTITY_NAME: &str = "Waddle";

pub fn client_caps_features() -> Vec<&'static str> {
    vec![
        DISCO_INFO_NS,
        NS_CAPS,
        NS_CHAT_MARKERS,
        crate::mds::NS_MDS_NOTIFY,
        // XEP-0163 §3: advertise interest in peers' User Activity so the server
        // fans their in-call overlay (ADR-010 Phase 3) out to us. Without this
        // the activity PEP node is published but no contact is ever notified.
        crate::pep::NS_ACTIVITY_NOTIFY,
        // XEP-0163 §3.4: advertise interest in our OWN status-preference node
        // (ADR-010 Phase 4) so the server's owner-self fan-out delivers a pick
        // made on one device to our other resources live. Without this the
        // node is published but no resource is ever notified.
        crate::pep::NS_STATUS_PREFERENCE_NOTIFY,
        // #1258 implement ⇒ advertise: each of the following is
        // implemented by this client (builders + parse + UI), and its
        // XEP requires/recommends the disco feature on supporting
        // clients.
        //
        // XEP-0085 §Determining Support: an implementing entity MUST
        // advertise the chatstates namespace in disco#info.
        NS_CHAT_STATES,
        //
        // XEP-0424 §Discovering support: a client implementing message
        // retraction MUST advertise this.
        NS_MESSAGE_RETRACT,
        // NOTE: urn:xmpp:message-moderate:1 is deliberately NOT
        // advertised. XEP-0425 defines the feature for the groupchat
        // SERVICE only ("If a groupchat supports moderated message
        // retraction, it MUST specify ..."); no clause assigns meaning
        // to a client advertising it, and the repo hard rule forbids
        // official namespaces without XEP-defined semantics.
        //
        // XEP-0444 §Discovering support: MUST for clients implementing
        // reactions.
        NS_REACTIONS,
        // XEP-0308 §Discovering support: MUST for clients implementing
        // correction.
        NS_MESSAGE_CORRECT,
        // XEP-0359 §Business Rules: entities supporting stanza-id
        // SHOULD announce it.
        crate::messaging::NS_STANZA_ID,
    ]
}

pub fn client_caps_verification_string() -> String {
    let mut s = String::new();
    s.push_str(IDENTITY_CATEGORY);
    s.push('/');
    s.push_str(IDENTITY_TYPE);
    s.push_str("//");
    s.push_str(IDENTITY_NAME);
    s.push('<');

    let mut features = client_caps_features();
    features.sort_unstable();
    for feature in features {
        s.push_str(feature);
        s.push('<');
    }

    BASE64_STANDARD.encode(Sha1::digest(s.as_bytes()))
}

pub fn client_caps_node_ver() -> String {
    format!("{}#{}", CAPS_NODE, client_caps_verification_string())
}

pub fn build_client_caps_element() -> Element {
    Element::builder("c", NS_CAPS)
        .attr(minidom::rxml::xml_ncname!("hash").to_owned(), "sha-1")
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), CAPS_NODE)
        .attr(
            minidom::rxml::xml_ncname!("ver").to_owned(),
            client_caps_verification_string(),
        )
        .build()
}

pub fn build_client_caps_disco_info_response(
    request: &Element,
    from: Option<&jid::FullJid>,
) -> Option<Element> {
    if request.name() != "iq" || request.attr("type") != Some("get") {
        return None;
    }
    let query = request.get_child("query", DISCO_INFO_NS)?;
    if let Some(node) = query.attr("node") {
        if node != client_caps_node_ver() {
            return None;
        }
    }

    let mut iq = Element::builder("iq", crate::bootstrap::NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result");
    if let Some(id) = request.attr("id") {
        iq = iq.attr(minidom::rxml::xml_ncname!("id").to_owned(), id);
    }
    if let Some(to) = request.attr("from") {
        iq = iq.attr(minidom::rxml::xml_ncname!("to").to_owned(), to);
    }
    if let Some(from) = from {
        iq = iq.attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            from.to_string(),
        );
    }

    let mut query_builder = Element::builder("query", DISCO_INFO_NS);
    if let Some(node) = query.attr("node") {
        query_builder = query_builder.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    }
    query_builder = query_builder.append(
        Element::builder("identity", DISCO_INFO_NS)
            .attr(
                minidom::rxml::xml_ncname!("category").to_owned(),
                IDENTITY_CATEGORY,
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), IDENTITY_TYPE)
            .attr(minidom::rxml::xml_ncname!("name").to_owned(), IDENTITY_NAME)
            .build(),
    );
    let mut features = client_caps_features();
    features.sort_unstable();
    for feature in features {
        query_builder = query_builder.append(
            Element::builder("feature", DISCO_INFO_NS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), feature)
                .build(),
        );
    }

    Some(iq.append(query_builder.build()).build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_element_advertises_mds_notify_and_chat_markers() {
        let caps = build_client_caps_element();
        assert_eq!(caps.name(), "c");
        assert_eq!(caps.ns(), NS_CAPS);
        assert_eq!(caps.attr("hash"), Some("sha-1"));
        assert_eq!(caps.attr("node"), Some(CAPS_NODE));
        assert_eq!(
            caps.attr("ver"),
            Some(client_caps_verification_string().as_str())
        );
        assert!(client_caps_features().contains(&crate::mds::NS_MDS_NOTIFY));
        assert!(client_caps_features().contains(&NS_CHAT_MARKERS));
        // ADR-010 Phase 3: the in-call overlay receive path depends on this
        // being advertised, or the server never fans a peer's activity to us.
        assert!(client_caps_features().contains(&crate::pep::NS_ACTIVITY_NOTIFY));
        // ADR-010 Phase 4: the cross-device manual-status sync depends on
        // this, or the server's §3.4 owner-self fan-out never delivers the
        // user's pick to their other resources.
        assert!(client_caps_features().contains(&crate::pep::NS_STATUS_PREFERENCE_NOTIFY));
    }

    /// #1258 implement ⇒ advertise: XEP-0424 retraction (MUST), XEP-0444
    /// reactions (MUST), XEP-0308 correction (MUST), and XEP-0359
    /// stanza-id (SHOULD) are all implemented by this client and must
    /// therefore appear in its disco/caps feature set. XEP-0425
    /// moderation is a groupchat-SERVICE feature with no client-side
    /// disco semantics, so a client caps advertisement would be an
    /// official namespace without XEP-defined meaning — asserted absent.
    #[test]
    fn caps_advertise_implemented_messaging_features() {
        let features = client_caps_features();
        assert!(features.contains(&NS_MESSAGE_RETRACT), "XEP-0424");
        assert!(features.contains(&NS_REACTIONS), "XEP-0444");
        assert!(features.contains(&NS_MESSAGE_CORRECT), "XEP-0308");
        assert!(
            features.contains(&crate::messaging::NS_STANZA_ID),
            "XEP-0359"
        );
        assert!(
            !features.contains(&crate::messaging::NS_MESSAGE_MODERATE),
            "XEP-0425 defines no client-side feature"
        );
    }

    #[test]
    fn caps_advertise_xep0085_chatstates_exactly_once() {
        let count = client_caps_features()
            .into_iter()
            .filter(|feature| *feature == crate::messaging::NS_CHAT_STATES)
            .count();
        assert_eq!(count, 1, "XEP-0085 chatstates feature cardinality");
    }

    /// XEP-0115 §5.1 golden vector for Waddle's concrete client identity and
    /// feature set. The expected SHA-1/Base64 value was calculated outside
    /// this implementation from the byte-sorted verification string using
    /// `openssl dgst -sha1 -binary | openssl base64 -A`. Pinning the literal
    /// makes this sensitive to identity fields, feature membership, octet
    /// ordering, and every required `<` separator.
    #[test]
    fn client_caps_verification_hash_matches_fixed_golden_vector() {
        let mut features = client_caps_features();
        features.sort_unstable();
        assert_eq!(
            features,
            vec![
                crate::pep::NS_ACTIVITY_NOTIFY,
                NS_CAPS,
                crate::messaging::NS_CHAT_STATES,
                DISCO_INFO_NS,
                crate::pep::NS_STATUS_PREFERENCE_NOTIFY,
                NS_CHAT_MARKERS,
                crate::mds::NS_MDS_NOTIFY,
                NS_MESSAGE_CORRECT,
                NS_MESSAGE_RETRACT,
                NS_REACTIONS,
                crate::messaging::NS_STANZA_ID,
            ],
            "the golden hash is valid only for this exact concrete feature set"
        );
        assert_eq!(
            client_caps_verification_string(),
            "xOykj9pu3F2sZWOM/yD+WvlIkgU="
        );
    }

    /// XEP-0115 §5.1: the ver string is derived from the sorted feature
    /// set, so every advertised feature must be unique or the hash is
    /// ill-formed (§5.4 validation would reject it).
    #[test]
    fn caps_features_contain_no_duplicates() {
        let mut features = client_caps_features();
        let total = features.len();
        features.sort_unstable();
        features.dedup();
        assert_eq!(features.len(), total, "duplicate disco feature advertised");
    }

    #[test]
    fn status_preference_notify_matches_core_node() {
        assert_eq!(
            crate::pep::NS_STATUS_PREFERENCE_NOTIFY,
            format!(
                "{}+notify",
                waddle_xmpp_core::waddle_status_preference::PEP_NODE_WADDLE_STATUS_PREFERENCE
            )
        );
    }

    #[test]
    fn disco_info_response_returns_caps_features_for_caps_node() {
        let request = Element::builder("iq", crate::bootstrap::NS_CLIENT)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "disco-1")
            .attr(
                minidom::rxml::xml_ncname!("from").to_owned(),
                "peer@example.com/resource",
            )
            .append(
                Element::builder("query", DISCO_INFO_NS)
                    .attr(
                        minidom::rxml::xml_ncname!("node").to_owned(),
                        client_caps_node_ver(),
                    )
                    .build(),
            )
            .build();
        let from = "alice@example.com/waddle"
            .parse::<jid::FullJid>()
            .expect("full JID parses");

        let response = build_client_caps_disco_info_response(&request, Some(&from))
            .expect("caps request is answered");
        assert_eq!(response.attr("type"), Some("result"));
        assert_eq!(response.attr("id"), Some("disco-1"));
        assert_eq!(response.attr("to"), Some("peer@example.com/resource"));
        assert_eq!(response.attr("from"), Some("alice@example.com/waddle"));
        let query = response
            .get_child("query", DISCO_INFO_NS)
            .expect("disco query present");
        assert_eq!(query.attr("node"), Some(client_caps_node_ver().as_str()));
        assert!(query.children().any(|child| {
            child.name() == "feature"
                && child.ns() == DISCO_INFO_NS
                && child.attr("var") == Some(crate::mds::NS_MDS_NOTIFY)
        }));
        assert_eq!(
            query
                .children()
                .filter(|child| {
                    child.name() == "feature"
                        && child.ns() == DISCO_INFO_NS
                        && child.attr("var") == Some(crate::messaging::NS_CHAT_STATES)
                })
                .count(),
            1,
            "the current caps node must disclose XEP-0085 exactly once"
        );
    }

    #[test]
    fn disco_info_response_ignores_unknown_caps_node() {
        let request = Element::builder("iq", crate::bootstrap::NS_CLIENT)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .append(
                Element::builder("query", DISCO_INFO_NS)
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), "unknown")
                    .build(),
            )
            .build();
        assert!(build_client_caps_disco_info_response(&request, None).is_none());
    }
}
