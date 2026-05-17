//! XEP-0490: Message Displayed Synchronization.
//!
//! Multi-device "read up to here" sync over a private PEP node
//! `urn:xmpp:mds:displayed:0`. Each item id is the bare JID of the
//! chat (1:1 contact or MUC room); the payload is a single
//! `<displayed/>` element wrapping one XEP-0359 `<stanza-id/>` that
//! points at the latest displayed message in that chat.
//!
//! ## XML
//!
//! ```xml
//! <item id='romeo@montague.lit'>
//!   <displayed xmlns='urn:xmpp:mds:displayed:0'>
//!     <stanza-id xmlns='urn:xmpp:sid:0'
//!                by='juliet@capulet.lit'
//!                id='0f710f2b-52ed-4d52-b928-784dad74a52b'/>
//!   </displayed>
//! </item>
//! ```
//!
//! For group chats, the `by` attribute on `<stanza-id/>` is the room
//! bare JID (XEP-0359 room-injected stanza-id). For 1:1 chats, it is
//! the user's own server JID (XEP-0359 user-server-injected stanza-id).
//!
//! ## Server scope
//!
//! XEP-0490 §3 declares server requirements minimal: XEP-0163 (PEP) +
//! XEP-0223 (Best Practices for Persistent Storage). Waddle satisfies
//! those by:
//!
//! - Registering `urn:xmpp:mds:displayed:0` as a well-known PEP node
//!   (see `PepHandler::is_well_known_node`), so authz routes through
//!   the PEP-self path on first publish.
//! - Defaulting newly auto-created MDS nodes to the spec-mandated
//!   shape: `access_model=whitelist`, `max_items=u32::MAX` (the
//!   `max` token), `persist_items=true`, `send_last_published_item=
//!   Never` (see `NodeConfig::mds_displayed`).
//! - Advertising the `urn:xmpp:mds:displayed:0+notify` filter from
//!   chat clients via XEP-0115 caps so the XEP-0163 §3.4 owner-self
//!   fan-out reaches every other connected resource.
//!
//! Server-assist (§3.5) is deliberately out of scope for this PR.

use jid::BareJid;
use minidom::Element;
use thiserror::Error;

/// PEP / payload namespace for XEP-0490.
pub const NS_MDS_DISPLAYED: &str = "urn:xmpp:mds:displayed:0";

/// PEP node id for XEP-0490. Identical to the namespace string.
pub const PEP_NODE_MDS_DISPLAYED: &str = "urn:xmpp:mds:displayed:0";

/// XEP-0115 `+notify` feature var for MDS.
pub const NS_MDS_DISPLAYED_NOTIFY: &str = "urn:xmpp:mds:displayed:0+notify";

/// XEP-0359 stanza-id namespace.
pub const NS_STANZA_ID: &str = "urn:xmpp:sid:0";

/// Opaque XEP-0359 stanza-id value. Newtype to keep the typed-payloads
/// rule satisfied at every protocol boundary: the wire form is a
/// string, but no internal API ever sees `String` for "stanza-id".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StanzaId(String);

impl StanzaId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for StanzaId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Typed XEP-0490 `<displayed/>` payload value.
///
/// `stanza_id_by` is the bare JID that injected the stanza-id (the
/// room for MUC, the user's own server for 1:1). Per XEP-0490 §3, this
/// is the JID the client used to scope its `<stanza-id>` lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdsDisplayed {
    pub stanza_id: StanzaId,
    pub stanza_id_by: BareJid,
}

impl MdsDisplayed {
    pub fn new(stanza_id: StanzaId, stanza_id_by: BareJid) -> Self {
        Self {
            stanza_id,
            stanza_id_by,
        }
    }
}

/// Parsing errors for the `<displayed/>` payload.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum MdsDisplayedError {
    #[error("expected <displayed/> in namespace '{NS_MDS_DISPLAYED}'")]
    WrongElement,
    #[error("missing <stanza-id/> child in '{NS_STANZA_ID}'")]
    MissingStanzaId,
    #[error("missing id attribute on <stanza-id/>")]
    MissingStanzaIdId,
    #[error("missing by attribute on <stanza-id/>")]
    MissingStanzaIdBy,
    #[error("invalid 'by' JID on <stanza-id/>: {0}")]
    InvalidByJid(String),
}

/// Build a `<displayed/>` element from a typed value.
pub fn build_displayed_element(displayed: &MdsDisplayed) -> Element {
    Element::builder("displayed", NS_MDS_DISPLAYED)
        .append(
            Element::builder("stanza-id", NS_STANZA_ID)
                .attr("id", displayed.stanza_id.as_str())
                .attr("by", displayed.stanza_id_by.to_string())
                .build(),
        )
        .build()
}

/// True when `elem` is a `<displayed/>` element in `NS_MDS_DISPLAYED`.
pub fn is_displayed_element(elem: &Element) -> bool {
    elem.is("displayed", NS_MDS_DISPLAYED)
}

/// Parse a `<displayed/>` element into a typed value.
pub fn parse_displayed_element(elem: &Element) -> Result<MdsDisplayed, MdsDisplayedError> {
    if !is_displayed_element(elem) {
        return Err(MdsDisplayedError::WrongElement);
    }
    let stanza_id_elem = elem
        .get_child("stanza-id", NS_STANZA_ID)
        .ok_or(MdsDisplayedError::MissingStanzaId)?;
    let id = stanza_id_elem
        .attr("id")
        .ok_or(MdsDisplayedError::MissingStanzaIdId)?;
    let by_raw = stanza_id_elem
        .attr("by")
        .ok_or(MdsDisplayedError::MissingStanzaIdBy)?;
    let by: BareJid = by_raw
        .parse()
        .map_err(|e: jid::Error| MdsDisplayedError::InvalidByJid(e.to_string()))?;
    Ok(MdsDisplayed {
        stanza_id: StanzaId::new(id),
        stanza_id_by: by,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jid(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }

    #[test]
    fn round_trip_dm() {
        let displayed = MdsDisplayed::new(
            StanzaId::new("0f710f2b-52ed-4d52-b928-784dad74a52b"),
            jid("juliet@capulet.lit"),
        );
        let elem = build_displayed_element(&displayed);
        let parsed = parse_displayed_element(&elem).expect("parse");
        assert_eq!(parsed, displayed);
    }

    #[test]
    fn round_trip_muc() {
        let displayed = MdsDisplayed::new(
            StanzaId::new("ca21deaf-812c-48f1-8f16-339a674f2864"),
            jid("example@conference.shakespeare.lit"),
        );
        let elem = build_displayed_element(&displayed);
        let parsed = parse_displayed_element(&elem).expect("parse");
        assert_eq!(parsed, displayed);
    }

    #[test]
    fn wrong_element_rejected() {
        let elem = Element::builder("displayed", "urn:xmpp:chat-markers:0").build();
        assert_eq!(
            parse_displayed_element(&elem).unwrap_err(),
            MdsDisplayedError::WrongElement
        );
    }

    #[test]
    fn missing_stanza_id_rejected() {
        let elem = Element::builder("displayed", NS_MDS_DISPLAYED).build();
        assert_eq!(
            parse_displayed_element(&elem).unwrap_err(),
            MdsDisplayedError::MissingStanzaId
        );
    }

    #[test]
    fn stanza_id_missing_id_rejected() {
        let elem = Element::builder("displayed", NS_MDS_DISPLAYED)
            .append(
                Element::builder("stanza-id", NS_STANZA_ID)
                    .attr("by", "juliet@capulet.lit")
                    .build(),
            )
            .build();
        assert_eq!(
            parse_displayed_element(&elem).unwrap_err(),
            MdsDisplayedError::MissingStanzaIdId
        );
    }

    #[test]
    fn stanza_id_missing_by_rejected() {
        let elem = Element::builder("displayed", NS_MDS_DISPLAYED)
            .append(
                Element::builder("stanza-id", NS_STANZA_ID)
                    .attr("id", "abc")
                    .build(),
            )
            .build();
        assert_eq!(
            parse_displayed_element(&elem).unwrap_err(),
            MdsDisplayedError::MissingStanzaIdBy
        );
    }

    #[test]
    fn invalid_by_jid_rejected() {
        let elem = Element::builder("displayed", NS_MDS_DISPLAYED)
            .append(
                Element::builder("stanza-id", NS_STANZA_ID)
                    .attr("id", "abc")
                    .attr("by", "not a jid /resource/extra")
                    .build(),
            )
            .build();
        match parse_displayed_element(&elem).unwrap_err() {
            MdsDisplayedError::InvalidByJid(_) => {}
            other => panic!("expected InvalidByJid, got {other:?}"),
        }
    }

    #[test]
    fn pep_node_matches_namespace() {
        assert_eq!(PEP_NODE_MDS_DISPLAYED, NS_MDS_DISPLAYED);
    }

    #[test]
    fn notify_feature_var_uses_plus_notify_suffix() {
        assert_eq!(
            NS_MDS_DISPLAYED_NOTIFY,
            format!("{NS_MDS_DISPLAYED}+notify")
        );
    }
}
