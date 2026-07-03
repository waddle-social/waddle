use super::*;

/// PEP self-or-to check (XEP-0163 §3).
///
/// Returns `true` when the IQ is directed at `target_jid` (a PEP service) *or*
/// when no `to=` attribute is present and `user_jid` is the implicit PEP owner.
/// Use this in every pubsub IQ arm so that to-less self-targeted IQs receive
/// the same owner-derived affiliation as explicitly addressed PEP requests.
pub(super) fn is_pep_self_or_to(
    iq: &xmpp_parsers::iq::Iq,
    target_jid: &BareJid,
    user_jid: &BareJid,
) -> bool {
    is_pep_request_to(iq, target_jid) || is_pep_request(iq, user_jid)
}
