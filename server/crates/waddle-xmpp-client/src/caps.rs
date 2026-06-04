use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use minidom::Element;
use sha1::{Digest, Sha1};

use crate::discovery::DISCO_INFO_NS;
use crate::messaging::NS_CHAT_MARKERS;

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
