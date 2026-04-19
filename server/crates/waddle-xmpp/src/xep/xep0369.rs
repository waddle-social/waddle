//! XEP-0369: Mediated Information eXchange (MIX) — core.
//!
//! This module is the XEP-identified entry point for MIX core stanzas. The
//! heavy lifting lives in [`crate::mix`]; re-exports here give callers a
//! single import surface aligned with the rest of the `xep/` modules.

pub use crate::mix::{
    build_join_result, build_leave_result, build_setnick_result, build_update_subscription_result,
    parse_join, parse_leave, parse_setnick, parse_update_subscription, JoinRequest, LeaveRequest,
    MixError, MixLeafNode, SetnickRequest, UpdateSubscriptionRequest, NS_MIX_CORE,
};

use xmpp_parsers::iq::{Iq, IqType};

/// Returns true if an IQ carries a MIX-core payload this module can parse.
pub fn is_mix_core_iq(iq: &Iq) -> bool {
    let elem = match &iq.payload {
        IqType::Set(e) | IqType::Get(e) => e,
        _ => return false,
    };
    if elem.ns() != NS_MIX_CORE {
        return false;
    }
    matches!(
        elem.name(),
        "join" | "leave" | "setnick" | "update-subscription"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[test]
    fn test_is_mix_core_iq() {
        let iq = Iq {
            from: None,
            to: None,
            id: "x".into(),
            payload: IqType::Set(Element::builder("join", NS_MIX_CORE).build()),
        };
        assert!(is_mix_core_iq(&iq));

        let iq2 = Iq {
            from: None,
            to: None,
            id: "x".into(),
            payload: IqType::Set(
                Element::builder("join", "http://jabber.org/protocol/muc").build(),
            ),
        };
        assert!(!is_mix_core_iq(&iq2));
    }
}
