/// XMPP client namespace
pub const JABBER_CLIENT: &str = "jabber:client";
/// XMPP server namespace
pub const JABBER_SERVER: &str = "jabber:server";
/// XMPP streams namespace
pub const STREAM: &str = "http://etherx.jabber.org/streams";
/// STARTTLS namespace
pub const TLS: &str = "urn:ietf:params:xml:ns:xmpp-tls";
/// SASL namespace
pub const SASL: &str = "urn:ietf:params:xml:ns:xmpp-sasl";
/// Resource binding namespace
pub const BIND: &str = "urn:ietf:params:xml:ns:xmpp-bind";
/// Session namespace
pub const SESSION: &str = "urn:ietf:params:xml:ns:xmpp-session";
/// Stanza error namespace
pub const STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";
/// Stream error namespace (RFC 6120 §4.9)
pub const STREAMS: &str = "urn:ietf:params:xml:ns:xmpp-streams";
/// Stream Management namespace (XEP-0198, version 3)
pub const SM: &str = "urn:xmpp:sm:3";
/// Instant Stream Resumption namespace (XEP-0397).
///
/// ADR-0017 Phase 3 Slice 8 XEP fact-check: the vendored XEP-0397 source
/// itself contains a literal typo (`htpps://...`) in its stream-feature and
/// obtain-token examples; the XMPP Registrar Considerations section
/// confirms `https://` is the canonical, registered spelling. This constant
/// uses the corrected spelling — `urn:xmpp:isr:0` (the pre-Slice-8 value)
/// was never a form XEP-0397 actually specifies.
pub const ISR: &str = "https://xmpp.org/extensions/isr/0";
/// SASL2 envelope namespace (XEP-0388), used by ADR-0017 Phase 3 Slice 8's
/// XEP-0397 instant-resume `<authenticate/>`/`<success/>`/`<failure/>`
/// nonzas. XEP-0397's own examples wrap these in the stale `urn:xmpp:sasl:1`
/// (a namespace XEP-0397 targeted before a later rename); this plan follows
/// the vendored, current XEP-0388's `urn:xmpp:sasl:2` instead, per the
/// phase plan's XEP fact-check (deviation, see Slice 8 of
/// `docs/adrs/0017-phase3-plan-ownership-claims.md`).
pub const SASL2: &str = "urn:xmpp:sasl:2";
/// Roster versioning stream feature (RFC 6121)
pub const ROSTERVER: &str = "urn:xmpp:features:rosterver";
/// Entity capabilities namespace (XEP-0115)
pub const CAPS: &str = "http://jabber.org/protocol/caps";
