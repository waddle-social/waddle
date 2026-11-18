//! XML namespaces the harness uses.

pub const STREAM: &str = "http://etherx.jabber.org/streams";
pub const CLIENT: &str = "jabber:client";
pub const SERVER: &str = "jabber:server";
pub const DIALBACK: &str = "jabber:server:dialback";
pub const DIALBACK_FEATURE: &str = "urn:xmpp:features:dialback";
pub const TLS: &str = "urn:ietf:params:xml:ns:xmpp-tls";
pub const SASL: &str = "urn:ietf:params:xml:ns:xmpp-sasl";
pub const BIND: &str = "urn:ietf:params:xml:ns:xmpp-bind";
pub const SESSION: &str = "urn:ietf:params:xml:ns:xmpp-session";
pub const STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";
pub const STREAM_ERRORS: &str = "urn:ietf:params:xml:ns:xmpp-streams";
pub const FRAMING: &str = "urn:ietf:params:xml:ns:xmpp-framing";
pub const PING: &str = "urn:xmpp:ping";
pub const DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
pub const DISCO_ITEMS: &str = "http://jabber.org/protocol/disco#items";
pub const ROSTER: &str = "jabber:iq:roster";
pub const CARBONS: &str = "urn:xmpp:carbons:2";
pub const FORWARD: &str = "urn:xmpp:forward:0";
pub const MUC: &str = "http://jabber.org/protocol/muc";
pub const MUC_USER: &str = "http://jabber.org/protocol/muc#user";
pub const MUC_ADMIN: &str = "http://jabber.org/protocol/muc#admin";
pub const MAM: &str = "urn:xmpp:mam:2";
pub const RSM: &str = "http://jabber.org/protocol/rsm";
pub const DATA_FORMS: &str = "jabber:x:data";
pub const MUC_OWNER: &str = "http://jabber.org/protocol/muc#owner";
pub const MUC_ROOMCONFIG: &str = "http://jabber.org/protocol/muc#roomconfig";
pub const PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";
pub const PUBSUB_NODE_CONFIG: &str = "http://jabber.org/protocol/pubsub#node_config";
/// Waddle's owner-config field that opens a room to accounts at other
/// servers (`urn:waddle:roomconfig:federated`); off by default.
pub const WADDLE_ROOM_FEDERATED: &str = "urn:waddle:roomconfig:federated";
/// Waddle's Space configuration field that opens a Space to accounts at
/// other servers (`urn:waddle:spaces:federated`); off by default.
pub const WADDLE_SPACE_FEDERATED: &str = "urn:waddle:spaces:federated";
pub const PUBSUB: &str = "http://jabber.org/protocol/pubsub";
pub const PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";
pub const BOOKMARKS: &str = "urn:xmpp:bookmarks:1";
pub const SPACES: &str = "urn:xmpp:spaces:0";
pub const HTTP_UPLOAD: &str = "urn:xmpp:http:upload:0";
pub const MODERATION: &str = "urn:xmpp:message-moderate:1";
pub const RETRACT: &str = "urn:xmpp:message-retract:1";
