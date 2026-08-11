/// The exact pre-routing addressed form parsed from a stanza.
///
/// `Absent` means the stanza had no `to` attribute at all. This is captured
/// before any routing rewrite that might supply the sender's bare JID as an
/// implicit target, so such a rewrite is never visible here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NormalizedTarget {
    Absent,
    Bare(jid::BareJid),
    Full(jid::FullJid),
}
