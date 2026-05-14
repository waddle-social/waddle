//! XEP-0166: Jingle (core session signaling).
//!
//! Thin Waddle-facing wrapper around [`xmpp_parsers::jingle`]. The
//! core types ([`Jingle`], [`Action`], [`Content`], [`Reason`]) are
//! re-exported as-is; this module exists to (a) own the feature-var
//! constants Waddle advertises in disco for XEP-0166 and (b)
//! provide builder helpers and a custom test suite per the
//! CLAUDE.md hard rule that every implemented XEP carries
//! exhaustive Rust tests.
//!
//! Waddle's Jingle integration is deliberately narrow: only the
//! `session-initiate`, `session-accept`, `session-terminate`,
//! `session-info`, and `transport-info` actions are exercised, and
//! the only transport recognised is the custom
//! [`crate::xep::xep_waddle_livekit_transport`]. Any other transport
//! is rejected with `<unsupported-transports/>` per XEP-0166 §10.2.

pub use xmpp_parsers::jingle::{
    Action, Content, ContentId, Creator, Description, Disposition, Jingle, Reason, ReasonElement,
    Senders, SessionId, Transport,
};

use std::collections::BTreeMap;

pub const NS_JINGLE: &str = "urn:xmpp:jingle:1";

/// Build a `<reason/>` element with no human-readable text.
pub fn reason_element(reason: Reason) -> ReasonElement {
    ReasonElement {
        reason,
        texts: BTreeMap::new(),
    }
}

/// Build a minimal `session-terminate` Jingle stanza with the given
/// reason. The transport is not included — `session-terminate`
/// carries no `<content/>` per XEP-0166 §6.7.
pub fn session_terminate(sid: SessionId, reason: Reason) -> Jingle {
    Jingle::new(Action::SessionTerminate, sid).set_reason(reason_element(reason))
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    fn parse_jingle(xml: &str) -> Jingle {
        let elem: Element = xml.parse().expect("element parses");
        Jingle::try_from(elem).expect("Jingle parses")
    }

    #[test]
    fn ns_is_canonical() {
        assert_eq!(NS_JINGLE, xmpp_parsers::ns::JINGLE);
    }

    #[test]
    fn session_initiate_parses() {
        let xml = r#"<jingle xmlns='urn:xmpp:jingle:1'
                             action='session-initiate'
                             initiator='alice@waddle.social/desktop'
                             sid='c1'>
                       <content creator='initiator' name='audio'/>
                     </jingle>"#;
        let jingle = parse_jingle(xml);
        assert_eq!(jingle.action, Action::SessionInitiate);
        assert_eq!(jingle.sid.0, "c1");
        assert_eq!(jingle.contents.len(), 1);
    }

    #[test]
    fn session_accept_parses() {
        let xml = r#"<jingle xmlns='urn:xmpp:jingle:1'
                             action='session-accept'
                             responder='bob@waddle.social/desktop'
                             sid='c1'>
                       <content creator='initiator' name='audio'/>
                     </jingle>"#;
        let jingle = parse_jingle(xml);
        assert_eq!(jingle.action, Action::SessionAccept);
    }

    #[test]
    fn session_terminate_builder_emits_reason() {
        let term = session_terminate(SessionId("c1".into()), Reason::Success);
        let elem: Element = term.into();
        assert_eq!(elem.attr("action"), Some("session-terminate"));
        let reason = elem
            .children()
            .find(|c| c.name() == "reason")
            .expect("<reason/> present");
        assert!(reason.children().any(|c| c.name() == "success"));
    }

    #[test]
    fn rejects_non_jingle_namespace() {
        let xml = r#"<jingle xmlns='urn:waddle:not-jingle' action='session-initiate' sid='c1'/>"#;
        let elem: Element = xml.parse().unwrap();
        assert!(Jingle::try_from(elem).is_err());
    }
}
