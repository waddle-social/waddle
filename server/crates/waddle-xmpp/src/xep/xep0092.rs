//! XEP-0092: Software Version
//!
//! Allows clients to query the server (or any entity) for its software
//! name and version, with optional operating-system disclosure. Waddle keeps
//! the helper generic, while the server omits OS details by default so cache
//! keys do not depend on per-commit build metadata.
//!
//! ## XML Format
//!
//! Query:
//! ```xml
//! <iq type='get' to='example.com' id='v-1'>
//!   <query xmlns='jabber:iq:version'/>
//! </iq>
//! ```
//!
//! Response:
//! ```xml
//! <iq type='result' from='example.com' id='v-1'>
//!   <query xmlns='jabber:iq:version'>
//!     <name>Waddle</name>
//!     <version>0.1.0</version>
//!   </query>
//! </iq>
//! ```

use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};

/// Namespace for XEP-0092 Software Version.
pub const NS_VERSION: &str = "jabber:iq:version";

/// Parsed software version payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoftwareVersion {
    pub name: String,
    pub version: String,
    pub os: Option<String>,
}

/// Check if an IQ stanza is a software version query.
pub fn is_version_query(iq: &Iq) -> bool {
    matches!(&iq.payload, IqType::Get(elem) if elem.name() == "query" && elem.ns() == NS_VERSION)
}

/// Build a software version response element.
pub fn build_version_element(info: &SoftwareVersion) -> Element {
    let mut builder = Element::builder("query", NS_VERSION)
        .append(
            Element::builder("name", NS_VERSION)
                .append(info.name.as_str())
                .build(),
        )
        .append(
            Element::builder("version", NS_VERSION)
                .append(info.version.as_str())
                .build(),
        );

    if let Some(os) = &info.os {
        builder = builder.append(
            Element::builder("os", NS_VERSION)
                .append(os.as_str())
                .build(),
        );
    }

    builder.build()
}

/// Build a software version response IQ.
pub fn build_version_response(original_iq: &Iq, info: &SoftwareVersion) -> Iq {
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(build_version_element(info))),
    }
}

/// Parse a software version response back into a struct.
pub fn parse_version_response(iq: &Iq) -> Option<SoftwareVersion> {
    let elem = match &iq.payload {
        IqType::Result(Some(elem)) if elem.name() == "query" && elem.ns() == NS_VERSION => elem,
        _ => return None,
    };

    let name = elem
        .children()
        .find(|c| c.is("name", NS_VERSION))
        .map(|c| c.text())?;

    let version = elem
        .children()
        .find(|c| c.is("version", NS_VERSION))
        .map(|c| c.text())?;

    let os = elem
        .children()
        .find(|c| c.is("os", NS_VERSION))
        .map(|c| c.text())
        .filter(|s| !s.is_empty());

    Some(SoftwareVersion { name, version, os })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_version_query() -> Iq {
        let query_elem = Element::builder("query", NS_VERSION).build();
        Iq {
            from: Some("alice@example.com".parse().expect("valid jid")),
            to: Some("example.com".parse().expect("valid jid")),
            id: "v-1".to_string(),
            payload: IqType::Get(query_elem),
        }
    }

    fn sample_info() -> SoftwareVersion {
        SoftwareVersion {
            name: "Waddle".to_string(),
            version: "0.1.0 (abcdef123456)".to_string(),
            os: Some("Linux".to_string()),
        }
    }

    #[test]
    fn xep0092_positive_detects_version_get() {
        let iq = make_version_query();
        assert!(is_version_query(&iq));
    }

    #[test]
    fn xep0092_negative_ignores_non_version_namespace() {
        let other = Element::builder("query", "jabber:iq:roster").build();
        let iq = Iq {
            from: None,
            to: None,
            id: "v-ns".to_string(),
            payload: IqType::Get(other),
        };
        assert!(!is_version_query(&iq));
    }

    #[test]
    fn xep0092_negative_ignores_set_iq() {
        let query = Element::builder("query", NS_VERSION).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "v-set".to_string(),
            payload: IqType::Set(query),
        };
        assert!(!is_version_query(&iq));
    }

    #[test]
    fn xep0092_negative_ignores_result_iq() {
        let query = Element::builder("query", NS_VERSION).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "v-res".to_string(),
            payload: IqType::Result(Some(query)),
        };
        assert!(!is_version_query(&iq));
    }

    #[test]
    fn xep0092_positive_builds_response_with_expected_children() {
        let query = make_version_query();
        let info = sample_info();
        let response = build_version_response(&query, &info);

        assert_eq!(response.id, "v-1");
        assert_eq!(response.from, query.to);
        assert_eq!(response.to, query.from);

        let IqType::Result(Some(elem)) = &response.payload else {
            panic!("Expected Result payload");
        };
        assert_eq!(elem.name(), "query");
        assert_eq!(elem.ns(), NS_VERSION);

        let name = elem
            .children()
            .find(|c| c.is("name", NS_VERSION))
            .expect("name");
        assert_eq!(name.text(), "Waddle");

        let version = elem
            .children()
            .find(|c| c.is("version", NS_VERSION))
            .expect("version");
        assert_eq!(version.text(), "0.1.0 (abcdef123456)");

        let os = elem
            .children()
            .find(|c| c.is("os", NS_VERSION))
            .expect("os");
        assert_eq!(os.text(), "Linux");
    }

    #[test]
    fn xep0092_positive_builds_response_without_os() {
        let query = make_version_query();
        let info = SoftwareVersion {
            name: "Waddle".to_string(),
            version: "0.1.0".to_string(),
            os: None,
        };
        let response = build_version_response(&query, &info);

        let IqType::Result(Some(elem)) = &response.payload else {
            panic!("Expected Result payload");
        };
        assert!(elem.children().all(|c| !c.is("os", NS_VERSION)));
    }

    #[test]
    fn xep0092_consistency_roundtrip_parse() {
        let query = make_version_query();
        let info = sample_info();
        let response = build_version_response(&query, &info);

        let parsed = parse_version_response(&response).expect("parseable");
        assert_eq!(parsed, info);
    }

    #[test]
    fn xep0092_negative_parse_rejects_get_iq() {
        let query = make_version_query();
        assert!(parse_version_response(&query).is_none());
    }

    #[test]
    fn xep0092_negative_parse_requires_name_and_version() {
        let incomplete = Element::builder("query", NS_VERSION)
            .append(
                Element::builder("name", NS_VERSION)
                    .append("Waddle")
                    .build(),
            )
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "v-parse".to_string(),
            payload: IqType::Result(Some(incomplete)),
        };
        assert!(parse_version_response(&iq).is_none());
    }

    #[test]
    fn xep0092_consistency_namespace_constant() {
        assert_eq!(NS_VERSION, "jabber:iq:version");
    }

    #[test]
    fn xep0092_positive_response_swaps_to_from() {
        let query = make_version_query();
        let info = sample_info();
        let response = build_version_response(&query, &info);

        assert_eq!(response.from, query.to);
        assert_eq!(response.to, query.from);
    }
}
