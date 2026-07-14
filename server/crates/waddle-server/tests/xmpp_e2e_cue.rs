//! CUE-authored XMPP E2E scenarios over the active WebSocket C2S transport.

use waddle_ws_test_support as ws_common;

use anyhow::{anyhow, Context, Result};
use jid::Jid;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use waddle_xmpp::xep::xep0334::{self, Hint};
use waddle_xmpp::xep::xep0444;
use waddle_xmpp::Stanza;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::iq::{Iq, IqPayload};
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::minidom::Element;
use xmpp_parsers::presence::{Presence, Show as PresenceShow, Type as PresenceType};

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());
const RECV_TIMEOUT: Duration = Duration::from_secs(10);

const ADVERTISED_FEATURE_XEPS: &[(&str, &str)] = &[
    ("http://jabber.org/protocol/disco#info", "XEP-0030"),
    ("http://jabber.org/protocol/disco#items", "XEP-0030"),
    ("http://jabber.org/protocol/caps", "XEP-0115"),
    ("urn:xmpp:features:rosterver", "XEP-0237"),
    ("urn:xmpp:mam:2", "XEP-0313"),
    ("urn:xmpp:mam:2#extended", "XEP-0313"),
    ("urn:xmpp:sid:0", "XEP-0359"),
    ("urn:xmpp:reply:0", "XEP-0461"),
    ("urn:xmpp:message-correct:0", "XEP-0308"),
    ("urn:xmpp:chat-markers:0", "XEP-0333"),
    // XEP-0184 urn:xmpp:receipts is deliberately absent: ack generation
    // is the receiving client's job; the server routes receipts verbatim
    // and no longer advertises (or fabricates) them (#1247).
    ("urn:xmpp:message-retract:1", "XEP-0424"),
    ("urn:xmpp:message-retract:1#tombstone", "XEP-0424"),
    ("urn:xmpp:message-moderate:1", "XEP-0425"),
    ("urn:xmpp:reactions:0", "XEP-0444"),
    ("urn:xmpp:reference:0", "XEP-0372"),
    ("urn:xmpp:fallback:0", "XEP-0428"),
    // XEP-0201 is Informational and defines no disco#info feature.
    // The <thread/> element (with optional parent= attribute) is
    // emitted on messages without a feature advertisement.
    ("urn:xmpp:sm:3", "XEP-0198"),
    ("urn:xmpp:carbons:2", "XEP-0280"),
    ("urn:xmpp:carbons:rules:0", "XEP-0280"),
    ("urn:xmpp:http:upload:0", "XEP-0363"),
    ("jabber:iq:last", "XEP-0012"),
    ("urn:xmpp:blocking", "XEP-0191"),
    ("urn:xmpp:ping", "XEP-0199"),
    ("urn:xmpp:time", "XEP-0202"),
    ("jabber:iq:version", "XEP-0092"),
    ("http://jabber.org/protocol/pubsub", "XEP-0060"),
    ("http://jabber.org/protocol/pubsub#pep", "XEP-0163"),
    ("http://jabber.org/protocol/pubsub#auto-create", "XEP-0060"),
    (
        "http://jabber.org/protocol/pubsub#persistent-items",
        "XEP-0060",
    ),
    ("http://jabber.org/protocol/pubsub#publish", "XEP-0060"),
    (
        "http://jabber.org/protocol/pubsub#retrieve-items",
        "XEP-0060",
    ),
    ("http://jabber.org/protocol/pubsub#subscribe", "XEP-0060"),
    (
        "http://jabber.org/protocol/pubsub#access-whitelist",
        "XEP-0060",
    ),
    (
        "http://jabber.org/protocol/pubsub#access-presence",
        "XEP-0060",
    ),
    (
        "http://jabber.org/protocol/pubsub#auto-subscribe",
        "XEP-0060",
    ),
    (
        "http://jabber.org/protocol/pubsub#filtered-notifications",
        "XEP-0060",
    ),
    ("http://jabber.org/protocol/pubsub#create-nodes", "XEP-0060"),
    ("http://jabber.org/protocol/pubsub#config-node", "XEP-0060"),
    ("http://jabber.org/protocol/pubsub#meta-data", "XEP-0060"),
    (
        "http://jabber.org/protocol/pubsub#manage-subscriptions",
        "XEP-0060",
    ),
    (
        "http://jabber.org/protocol/pubsub#modify-affiliations",
        "XEP-0060",
    ),
    (
        "http://jabber.org/protocol/pubsub#retrieve-affiliations",
        "XEP-0060",
    ),
    ("http://jabber.org/protocol/pubsub#delete-nodes", "XEP-0060"),
    ("http://jabber.org/protocol/pubsub#delete-items", "XEP-0060"),
    (
        "http://jabber.org/protocol/pubsub#retract-items",
        "XEP-0060",
    ),
    ("http://jabber.org/protocol/pubsub#multi-items", "XEP-0060"),
    ("http://jabber.org/protocol/pubsub#item-ids", "XEP-0060"),
    (
        "http://jabber.org/protocol/pubsub#publish-only-affiliation",
        "XEP-0060",
    ),
    ("jabber:iq:private", "XEP-0049"),
    ("http://jabber.org/protocol/commands", "XEP-0050"),
    ("vcard-temp", "XEP-0054"),
    ("urn:ietf:params:xml:ns:vcard-4.0", "XEP-0292"),
    ("urn:xmpp:push:0", "XEP-0357"),
    ("urn:xmpp:bookmarks:1#compat", "XEP-0402"),
    ("urn:xmpp:bookmarks:1#compat-pep", "XEP-0402"),
    ("msgoffline", "XEP-0160"),
    ("http://jabber.org/protocol/muc", "XEP-0045"),
    // XEP-0045 §7.4 stable-id: reflected groupchat messages keep the
    // sender's original id (#1265 item 14).
    ("http://jabber.org/protocol/muc#stable_id", "XEP-0045"),
    (
        "http://jabber.org/protocol/muc#self-ping-optimization",
        "XEP-0410",
    ),
    ("http://jabber.org/protocol/chatstates", "XEP-0085"),
    ("urn:xmpp:fulltext:0", "XEP-0431"),
    ("urn:xmpp:occupant-id:0", "XEP-0421"),
    ("urn:xmpp:hats:0", "XEP-0317"),
    ("urn:xmpp:mentions:0", "XEP-0513"),
    ("urn:xmpp:mentions:0#channel", "XEP-0513"),
    ("jabber:iq:search", "XEP-0055"),
    ("urn:xmpp:jingle:1", "XEP-0166"),
    ("urn:xmpp:jingle:apps:rtp:1", "XEP-0167"),
    ("urn:xmpp:jingle:apps:rtp:audio", "XEP-0167"),
    ("urn:xmpp:jingle:apps:rtp:video", "XEP-0167"),
    ("urn:xmpp:jingle-message:0", "XEP-0353"),
    ("urn:xmpp:extdisco:2", "XEP-0215"),
    ("urn:xmpp:jingle:muji:0", "XEP-0272"),
    ("urn:xmpp:pubsub-social-feed:1", "XEP-0472"),
    ("urn:xmpp:pubsub-social-feed:stories:0", "XEP-0501"),
    ("urn:xmpp:spaces:0", "XEP-0503"),
    ("urn:xmpp:inbox:1", "XEP-0430"),
    // xCal calendar — XSF ProtoXEP "Calendaring Extensions to
    // Publish-Subscribe", no assigned XEP number. Mapped to
    // "PROTO-CALENDAR" so the coverage check accepts it without
    // pretending the unassigned number is real.
    ("urn:ietf:params:xml:ns:xcal", "PROTO-CALENDAR"),
];

const ADVERTISED_FEATURE_EXEMPTIONS: &[FeatureCoverageExemption] = &[
    FeatureCoverageExemption {
        feature: "jabber:iq:roster",
        reason: "Core roster feature has no XEP-numbered module in this repository.",
    },
    FeatureCoverageExemption {
        feature: "muc_membersonly",
        reason: "XEP-0045 room configuration identity flag, not a separate XEP namespace.",
    },
    FeatureCoverageExemption {
        feature: "muc_moderated",
        reason: "XEP-0045 room configuration identity flag, not a separate XEP namespace.",
    },
    FeatureCoverageExemption {
        feature: "muc_nonanonymous",
        reason: "XEP-0045 room configuration identity flag, not a separate XEP namespace.",
    },
    FeatureCoverageExemption {
        feature: "muc_open",
        reason: "XEP-0045 room configuration identity flag, not a separate XEP namespace.",
    },
    FeatureCoverageExemption {
        feature: "muc_persistent",
        reason: "XEP-0045 room configuration identity flag, not a separate XEP namespace.",
    },
    FeatureCoverageExemption {
        feature: "muc_public",
        reason: "XEP-0045 room configuration identity flag, not a separate XEP namespace.",
    },
    FeatureCoverageExemption {
        feature: "muc_hidden",
        reason: "XEP-0045 room configuration identity flag, not a separate XEP namespace.",
    },
    FeatureCoverageExemption {
        feature: "muc_temporary",
        reason: "XEP-0045 room configuration identity flag, not a separate XEP namespace.",
    },
    FeatureCoverageExemption {
        feature: "muc_unsecured",
        reason: "XEP-0045 room configuration identity flag, not a separate XEP namespace.",
    },
    FeatureCoverageExemption {
        feature: "muc_unmoderated",
        reason: "XEP-0045 room configuration identity flag, not a separate XEP namespace.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:admin:channels:affiliations:0",
        reason: "Private Waddle ad-hoc command namespace, not official XEP coverage.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:admin:channels:create:0",
        reason: "Private Waddle ad-hoc command namespace, not official XEP coverage.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:admin:channels:delete:0",
        reason: "Private Waddle ad-hoc command namespace, not official XEP coverage.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:admin:channels:kick:0",
        reason: "Private Waddle ad-hoc command namespace, not official XEP coverage.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:admin:channels:list:0",
        reason: "Private Waddle ad-hoc command namespace, not official XEP coverage.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:admin:channels:occupants:0",
        reason: "Private Waddle ad-hoc command namespace, not official XEP coverage.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:admin:channels:set-affiliation:0",
        reason: "Private Waddle ad-hoc command namespace, not official XEP coverage.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:admin:channels:update:0",
        reason: "Private Waddle ad-hoc command namespace, not official XEP coverage.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:admin:spaces:create:0",
        reason: "Private Waddle ad-hoc command namespace, not official XEP coverage.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:admin:spaces:delete:0",
        reason: "Private Waddle ad-hoc command namespace, not official XEP coverage.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:admin:spaces:list:0",
        reason: "Private Waddle ad-hoc command namespace, not official XEP coverage.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:admin:spaces:members:0",
        reason: "Private Waddle ad-hoc command namespace, not official XEP coverage.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:admin:spaces:set-role:0",
        reason: "Private Waddle ad-hoc command namespace, not official XEP coverage.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:admin:spaces:update:0",
        reason: "Private Waddle ad-hoc command namespace, not official XEP coverage.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:admin:users:list:0",
        reason: "Private Waddle ad-hoc command namespace, not official XEP coverage.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:group-dm:create:0",
        reason: "Private Waddle ad-hoc command namespace, not official XEP coverage.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:group-dm:leave:0",
        reason: "Private Waddle ad-hoc command namespace, covered by group_dm_ws.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:mam-thread:0",
        reason: "Private Waddle MAM extension namespace, not official XEP coverage.",
    },
    FeatureCoverageExemption {
        feature: "urn:waddle:transports:livekit:0",
        reason: "Private Waddle Jingle transport namespace, not official XEP coverage.",
    },
];

const CUE_ONLY_XEP_TAGS: &[XepCoverageExemption] = &[
    XepCoverageExemption {
        xep: "XEP-0184",
        reason: "Receipt payloads are routed transparently and exercised by CUE; the server neither advertises nor generates receipts.",
    },
    XepCoverageExemption {
        xep: "XEP-0103",
        reason: "URL address payload is exercised through the FileShare CUE DSL; no standalone module exists.",
    },
    XepCoverageExemption {
        xep: "XEP-0511",
        reason: "Link metadata enrichment is server behavior exercised by CUE; no standalone module exists.",
    },
    XepCoverageExemption {
        xep: "PROTO-CALENDAR",
        reason: "XSF ProtoXEP xCal node has no assigned XEP number.",
    },
];

const IMPLEMENTED_XEP_COVERAGE_EXEMPTIONS: &[XepCoverageExemption] = &[
    XepCoverageExemption {
        xep: "XEP-0077",
        reason: "Library-only registration parser is not advertised by the server; XML-builder fix/coverage tracked separately.",
    },
    XepCoverageExemption {
        xep: "XEP-0249",
        reason: "Library-only direct-invite helper is not wired into server behavior; XML-builder fix/coverage tracked separately.",
    },
    XepCoverageExemption {
        xep: "XEP-0319",
        reason: "Library-only idle-presence element helper is not advertised or synthesized by the server.",
    },
    XepCoverageExemption {
        xep: "XEP-0377",
        reason: "Spam-reporting payload helper is not wired into server blocking behavior yet.",
    },
    XepCoverageExemption {
        xep: "XEP-0392",
        reason: "Pure deterministic color-generation library with no server wire behavior.",
    },
    XepCoverageExemption {
        xep: "XEP-0401",
        reason: "Invite command data model is not registered as a server ad-hoc command yet.",
    },
    XepCoverageExemption {
        xep: "XEP-0437",
        reason: "Unread tracker is an in-memory library helper with no advertised server protocol surface.",
    },
    XepCoverageExemption {
        xep: "XEP-0445",
        reason: "Pre-auth registration element helper is not wired into a server registration flow yet.",
    },
    XepCoverageExemption {
        xep: "XEP-0448",
        reason: "Encrypted SFS payload model is client-carried message XML with no server-specific behavior.",
    },
    XepCoverageExemption {
        xep: "XEP-0449",
        reason: "Sticker payload model is client-carried message XML with no server-specific behavior.",
    },
    XepCoverageExemption {
        xep: "XEP-0452",
        reason: "Legacy MUC mention-notification payload is not advertised; server behavior uses XEP-0513.",
    },
    XepCoverageExemption {
        xep: "XEP-0469",
        reason: "Bookmark pinning helper is only consumed inside bookmark payload projection, not advertised separately.",
    },
    XepCoverageExemption {
        xep: "XEP-0470",
        reason: "PubSub attachment helper backs Waddle pin projection but is not advertised as standalone XEP-0470 support.",
    },
    XepCoverageExemption {
        xep: "XEP-0486",
        reason: "MUC avatar cache helper has no advertised server feature or dedicated server behavior yet.",
    },
    XepCoverageExemption {
        xep: "XEP-0488",
        reason: "MUC token invite payload helper is not wired into server room invite behavior yet.",
    },
    XepCoverageExemption {
        xep: "XEP-0500",
        reason: "Slow-mode config/rate-limit helper is not wired into MUC send authorization yet.",
    },
    XepCoverageExemption {
        xep: "XEP-0502",
        reason: "MUC activity payload helper is not advertised or emitted by the server yet.",
    },
];

#[derive(Debug, Clone, Copy)]
struct XepCoverageExemption {
    xep: &'static str,
    reason: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct FeatureCoverageExemption {
    feature: &'static str,
    reason: &'static str,
}

#[derive(Debug, Deserialize)]
struct ScenarioFile {
    scenario: Scenario,
}

#[derive(Debug, Deserialize)]
struct Scenario {
    name: String,
    #[serde(default)]
    xeps: Vec<String>,
    domain: String,
    users: BTreeMap<String, User>,
    steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
struct User {
    devices: BTreeMap<String, Actor>,
}

#[derive(Debug, Clone, Deserialize)]
struct Actor {
    user: String,
    device: String,
    username: String,
    resource: String,
    #[serde(rename = "bareJid")]
    bare_jid: String,
    jid: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
enum Step {
    #[serde(rename = "enableCarbons")]
    EnableCarbons { actor: Actor },
    #[serde(rename = "streamManagement")]
    StreamManagement {
        actor: Actor,
        action: StreamManagementAction,
        #[serde(default)]
        resume: Option<bool>,
        #[serde(default)]
        max: Option<u32>,
        #[serde(default, rename = "previdFrom")]
        previd_from: Option<String>,
        #[serde(default)]
        h: Option<u32>,
    },
    #[serde(rename = "connectActor")]
    ConnectActor {
        actor: Actor,
        #[serde(default)]
        bind: Option<bool>,
    },
    #[serde(rename = "disconnectActor")]
    DisconnectActor {
        actor: Actor,
        #[serde(default)]
        graceful: Option<bool>,
    },
    #[serde(rename = "waitMillis")]
    WaitMillis { millis: u64 },
    #[serde(rename = "sendIq")]
    SendIq {
        actor: Actor,
        #[serde(rename = "type")]
        type_: IqKindSpec,
        id: Option<String>,
        to: Option<String>,
        payload: Option<XmlElementSpec>,
    },
    #[serde(rename = "expectIq")]
    ExpectIq {
        target: Actor,
        id: Option<String>,
        #[serde(rename = "type")]
        type_: Option<IqResponseKind>,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        absent: Vec<String>,
        #[serde(default)]
        elements: Vec<XmlElementSpec>,
        #[serde(default, rename = "absentElements")]
        absent_elements: Vec<XmlElementSpec>,
        #[serde(default)]
        captures: Vec<AttributeCapture>,
    },
    #[serde(rename = "sendPresence")]
    SendPresence {
        actor: Actor,
        to: Option<String>,
        #[serde(rename = "type")]
        type_: Option<PresenceKind>,
        show: Option<String>,
        status: Option<String>,
        priority: Option<i8>,
        #[serde(default)]
        payloads: Vec<XmlElementSpec>,
    },
    #[serde(rename = "sendMessage")]
    SendMessage {
        from: Actor,
        to: Option<Actor>,
        #[serde(rename = "toJid")]
        to_jid: Option<String>,
        #[serde(rename = "type")]
        type_: MessageKind,
        id: Option<String>,
        body: Option<String>,
        #[serde(default)]
        payloads: Vec<Payload>,
    },
    #[serde(rename = "sendMessageBurst")]
    SendMessageBurst {
        from: Actor,
        to: Option<Actor>,
        #[serde(rename = "toJid")]
        to_jid: Option<String>,
        #[serde(rename = "type")]
        type_: MessageKind,
        #[serde(rename = "idPrefix")]
        id_prefix: String,
        #[serde(rename = "bodyPrefix")]
        body_prefix: String,
        count: u32,
    },
    #[serde(rename = "expectMessage")]
    ExpectMessage {
        target: Actor,
        body: Option<String>,
        #[serde(default, rename = "bodyAbsent")]
        body_absent: bool,
        from: Option<Actor>,
        #[serde(default, rename = "captureStanzaIdAs")]
        capture_stanza_id_as: Option<String>,
        #[serde(default, rename = "captureStanzaIdBy")]
        capture_stanza_id_by: Option<String>,
        #[serde(default)]
        payloads: Vec<Payload>,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        absent: Vec<String>,
        #[serde(default)]
        elements: Vec<XmlElementSpec>,
        #[serde(default, rename = "absentElements")]
        absent_elements: Vec<XmlElementSpec>,
    },
    #[serde(rename = "expectCarbon")]
    ExpectCarbon {
        target: Actor,
        carbon: CarbonKind,
        body: Option<String>,
        #[serde(default, rename = "bodyAbsent")]
        body_absent: bool,
        #[serde(default)]
        payloads: Vec<Payload>,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        absent: Vec<String>,
        #[serde(default)]
        elements: Vec<XmlElementSpec>,
        #[serde(default, rename = "absentElements")]
        absent_elements: Vec<XmlElementSpec>,
    },
    #[serde(rename = "joinMuc")]
    JoinMuc {
        actor: Actor,
        room: String,
        nick: String,
    },
    #[serde(rename = "setMucAffiliation")]
    SetMucAffiliation {
        actor: Actor,
        room: String,
        jid: String,
        affiliation: String,
        id: Option<String>,
    },
    #[serde(rename = "expectMucAffiliation")]
    ExpectMucAffiliation {
        actor: Actor,
        room: String,
        jid: String,
        affiliation: String,
        id: Option<String>,
    },
    #[serde(rename = "expectMucAdminDenied")]
    ExpectMucAdminDenied {
        actor: Actor,
        room: String,
        jid: String,
        affiliation: String,
        id: Option<String>,
    },
    #[serde(rename = "expectPresence")]
    ExpectPresence {
        target: Actor,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        elements: Vec<XmlElementSpec>,
        #[serde(default, rename = "absentElements")]
        absent_elements: Vec<XmlElementSpec>,
        #[serde(default)]
        captures: Vec<AttributeCapture>,
    },
    #[serde(rename = "queryMam")]
    QueryMam {
        actor: Actor,
        archive: String,
        id: Option<String>,
        max: u32,
        after: Option<String>,
        #[serde(default, rename = "afterFrom")]
        after_from: Option<String>,
        before: Option<String>,
        #[serde(default, rename = "beforeFrom")]
        before_from: Option<String>,
        #[serde(rename = "with")]
        with_jid: Option<String>,
        fulltext: Option<String>,
        #[serde(default)]
        ids: Vec<String>,
        #[serde(default, rename = "idsFrom")]
        ids_from: Vec<String>,
    },
    #[serde(rename = "expectMamResult")]
    ExpectMamResult {
        body: Option<String>,
        #[serde(default, rename = "bodyAbsent")]
        body_absent: bool,
        #[serde(default)]
        payloads: Vec<Payload>,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        absent: Vec<String>,
        #[serde(default)]
        elements: Vec<XmlElementSpec>,
        #[serde(default, rename = "absentElements")]
        absent_elements: Vec<XmlElementSpec>,
        #[serde(default)]
        captures: Vec<AttributeCapture>,
    },
    #[serde(rename = "expectNoMamResult")]
    ExpectNoMamResult {
        body: Option<String>,
        #[serde(default, rename = "bodyAbsent")]
        body_absent: bool,
        #[serde(default)]
        payloads: Vec<Payload>,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        elements: Vec<XmlElementSpec>,
    },
    #[serde(rename = "expectFrame")]
    ExpectFrame {
        target: Actor,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        absent: Vec<String>,
        #[serde(default)]
        elements: Vec<XmlElementSpec>,
        #[serde(default, rename = "absentElements")]
        absent_elements: Vec<XmlElementSpec>,
        #[serde(default)]
        captures: Vec<AttributeCapture>,
    },
    #[serde(rename = "drainFrames")]
    DrainFrames {
        target: Actor,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        elements: Vec<XmlElementSpec>,
        #[serde(default)]
        millis: u64,
        #[serde(default)]
        min: Option<u64>,
        #[serde(default)]
        max: Option<u64>,
    },
    #[serde(rename = "expectNoStanza")]
    ExpectNoStanza {
        target: Actor,
        body: Option<String>,
        #[serde(default)]
        contains: Vec<String>,
        millis: u64,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum MessageKind {
    Chat,
    Normal,
    Groupchat,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum StreamManagementAction {
    Enable,
    RequestAck,
    Resume,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum IqKindSpec {
    Get,
    Set,
    Result,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum IqResponseKind {
    Result,
    Error,
    Get,
    Set,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PresenceKind {
    Available,
    Unavailable,
    Subscribe,
    Subscribed,
    Unsubscribe,
    Unsubscribed,
    Probe,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CarbonKind {
    Sent,
    Received,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
enum Payload {
    #[serde(rename = "fileShare")]
    FileShare {
        disposition: String,
        name: String,
        #[serde(rename = "mediaType")]
        media_type: String,
        size: u64,
        url: String,
    },
    #[serde(rename = "linkMetadata")]
    LinkMetadata {
        about: String,
        title: String,
        description: String,
        url: String,
    },
    #[serde(rename = "messageCorrection")]
    MessageCorrection { id: String },
    #[serde(rename = "reactions")]
    Reactions {
        id: Option<String>,
        #[serde(rename = "idFrom")]
        id_from: Option<String>,
        #[serde(default)]
        emojis: Vec<String>,
    },
    #[serde(rename = "processingHint")]
    ProcessingHint { name: ProcessingHint },
    #[serde(rename = "pinAttachment")]
    PinAttachment {
        id: Option<String>,
        #[serde(rename = "idFrom")]
        id_from: Option<String>,
        action: PinAction,
    },
    #[serde(rename = "pinEvent")]
    PinEvent {
        id: Option<String>,
        #[serde(rename = "idFrom")]
        id_from: Option<String>,
        action: PinAction,
    },
    #[serde(rename = "xml")]
    Xml {
        element: XmlElementSpec,
        #[serde(default, rename = "expectElements")]
        expect_elements: Vec<XmlElementSpec>,
    },
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum PinAction {
    Pinned,
    Unpinned,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ProcessingHint {
    NoPermanentStore,
    NoStore,
    NoCopy,
    Store,
}

#[derive(Debug, Clone, Deserialize)]
struct XmlElementSpec {
    name: String,
    ns: String,
    #[serde(default)]
    attrs: BTreeMap<String, String>,
    #[serde(default, rename = "attrsFrom")]
    attrs_from: BTreeMap<String, String>,
    #[serde(default, rename = "attrsPresent")]
    attrs_present: Vec<String>,
    text: Option<String>,
    #[serde(default)]
    children: Vec<XmlElementSpec>,
}

#[derive(Debug, Deserialize)]
struct AttributeCapture {
    #[serde(rename = "as")]
    capture_as: String,
    name: String,
    element: Option<String>,
    ns: Option<String>,
    contains: Option<String>,
}

struct ScenarioContext {
    clients: HashMap<String, WsXmppClient>,
    pending_frames: HashMap<String, VecDeque<String>>,
    last_mam_frames: Vec<String>,
    last_mam_frame_index: usize,
    captures: HashMap<String, String>,
    /// Per-actor XEP-0198 stream-management state. Tracks SM
    /// enabled-ness so we only auto-ack on streams that negotiated
    /// it, plus the inbound countable-stanza counter we send back as
    /// `<a h='N'/>`. Populated lazily — actors without SM never get
    /// an entry.
    sm_state: HashMap<String, ActorSmState>,
    ws_url: String,
    domain: String,
    admin_password: String,
    account_passwords: BTreeMap<String, String>,
}

#[tokio::test]
async fn cue_scenarios_run_over_websocket() -> Result<()> {
    let _serial = TEST_SERIAL.lock().await;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/xmpp_e2e_scenarios");
    let mut scenarios = Vec::new();
    for scenario_file in discover_scenario_files(&root)? {
        let scenario = load_scenario_from_file(&root, &scenario_file)
            .with_context(|| format!("load {}", scenario_file.display()))?;
        scenarios.push((scenario_file, scenario));
    }

    for (scenario_file, scenario) in scenarios {
        run_scenario(scenario)
            .await
            .with_context(|| format!("scenario {} failed", scenario_file.display()))?;
    }
    Ok(())
}

#[test]
fn cue_scenario_xep_tags_are_known_and_evidence_backed() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/xmpp_e2e_scenarios");
    let known = known_cue_xep_tags()?;
    let mut unknown = Vec::new();
    let mut missing_evidence = Vec::new();

    for scenario_file in discover_scenario_files(&root)? {
        let scenario = load_scenario_from_file(&root, &scenario_file)
            .with_context(|| format!("load {}", scenario_file.display()))?;
        let evidence = scenario_xep_evidence(&scenario);
        for xep in &scenario.xeps {
            if !known.contains(xep.as_str()) {
                unknown.push(format!("{} declares {xep}", scenario.name));
                continue;
            }
            if !known_xep_evidence_rules().contains(xep.as_str()) {
                missing_evidence.push(format!(
                    "{} declares {xep} with no evidence rule",
                    scenario.name
                ));
                continue;
            }
            if !evidence.contains(xep.as_str()) {
                missing_evidence.push(format!(
                    "{} declares {xep} without structural step evidence",
                    scenario.name
                ));
            }
        }
    }

    if unknown.is_empty() && missing_evidence.is_empty() {
        return Ok(());
    }

    Err(anyhow!(
        "CUE XEP tag drift: unknown tags [{}]; missing evidence [{}]",
        unknown.join(", "),
        missing_evidence.join(", ")
    ))
}

#[test]
fn implemented_xep_modules_have_explicit_coverage() -> Result<()> {
    let implemented = implemented_official_xep_modules()?;
    let rust = dedicated_rust_xep_suites()?;
    let exemptions = xep_exemption_map(IMPLEMENTED_XEP_COVERAGE_EXEMPTIONS)?;
    let mut missing = Vec::new();
    let mut stale_exemptions = Vec::new();

    for xep in &implemented {
        if rust.contains(xep.as_str()) || exemptions.contains_key(xep.as_str()) {
            continue;
        }
        missing.push(xep.clone());
    }

    for xep in exemptions.keys() {
        if !implemented.contains(*xep) {
            stale_exemptions.push((*xep).to_string());
        }
    }

    if missing.is_empty() && stale_exemptions.is_empty() {
        return Ok(());
    }

    Err(anyhow!(
        "implemented XEP coverage drift: missing explicit coverage [{}]; stale exemptions [{}]",
        missing.join(", "),
        stale_exemptions.join(", ")
    ))
}

#[test]
fn advertised_features_have_explicit_xep_coverage() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/xmpp_e2e_scenarios");
    let cue_features = meaningful_cue_feature_coverage(&root)?;
    let rust_features = meaningful_rust_feature_coverage()?;
    let feature_xeps = ADVERTISED_FEATURE_XEPS
        .iter()
        .copied()
        .collect::<BTreeMap<_, _>>();
    let exemptions = feature_exemption_map(ADVERTISED_FEATURE_EXEMPTIONS)?;
    let advertised = advertised_feature_vars();

    let unmapped = advertised
        .iter()
        .filter(|feature| !feature_xeps.contains_key(feature.as_str()))
        .filter(|feature| !exemptions.contains_key(feature.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let uncovered = advertised
        .iter()
        .filter_map(|feature| {
            feature_xeps
                .get(feature.as_str())
                .filter(|_| {
                    !rust_features.contains(feature.as_str())
                        && !cue_features.contains(feature.as_str())
                })
                .map(|xep| format!("{feature} -> {xep}"))
        })
        .collect::<Vec<_>>();

    if unmapped.is_empty() && uncovered.is_empty() {
        return Ok(());
    }

    Err(anyhow!(
        "advertised feature coverage drift: unmapped features [{}]; uncovered feature XEPs [{}]",
        unmapped.join(", "),
        uncovered.join(", ")
    ))
}

fn known_cue_xep_tags() -> Result<BTreeSet<String>> {
    let mut known = BTreeSet::new();
    known.extend(implemented_official_xep_modules()?);
    known.extend(
        ADVERTISED_FEATURE_XEPS
            .iter()
            .map(|(_, xep)| (*xep).to_string()),
    );
    known.extend(
        CUE_ONLY_XEP_TAGS
            .iter()
            .map(|exemption| exemption.xep.to_string()),
    );
    Ok(known)
}

fn meaningful_cue_feature_coverage(root: &Path) -> Result<BTreeSet<String>> {
    let mut covered = BTreeSet::new();
    for scenario_file in discover_scenario_files(root)? {
        let scenario = load_scenario_from_file(root, &scenario_file)
            .with_context(|| format!("load {}", scenario_file.display()))?;
        covered.extend(scenario_feature_evidence(&scenario));
    }
    Ok(covered)
}

fn known_xep_evidence_rules() -> BTreeSet<&'static str> {
    [
        "PROTO-CALENDAR",
        "XEP-0004",
        "XEP-0012",
        "XEP-0030",
        "XEP-0045",
        "XEP-0048",
        "XEP-0049",
        "XEP-0050",
        "XEP-0054",
        "XEP-0055",
        "XEP-0059",
        "XEP-0060",
        "XEP-0084",
        "XEP-0085",
        "XEP-0092",
        "XEP-0103",
        "XEP-0107",
        "XEP-0108",
        "XEP-0115",
        "XEP-0118",
        "XEP-0153",
        "XEP-0160",
        "XEP-0163",
        "XEP-0184",
        "XEP-0191",
        "XEP-0198",
        "XEP-0199",
        "XEP-0201",
        "XEP-0202",
        "XEP-0203",
        "XEP-0237",
        "XEP-0280",
        "XEP-0292",
        "XEP-0297",
        "XEP-0308",
        "XEP-0313",
        "XEP-0317",
        "XEP-0333",
        "XEP-0334",
        "XEP-0357",
        "XEP-0359",
        "XEP-0363",
        "XEP-0372",
        "XEP-0402",
        "XEP-0410",
        "XEP-0421",
        "XEP-0424",
        "XEP-0425",
        "XEP-0428",
        "XEP-0430",
        "XEP-0431",
        "XEP-0433",
        "XEP-0444",
        "XEP-0446",
        "XEP-0447",
        "XEP-0461",
        "XEP-0472",
        "XEP-0492",
        "XEP-0501",
        "XEP-0503",
        "XEP-0511",
        "XEP-0513",
    ]
    .into_iter()
    .collect()
}

fn scenario_xep_evidence(scenario: &Scenario) -> BTreeSet<&'static str> {
    let mut evidence = BTreeSet::new();
    for step in &scenario.steps {
        add_step_xep_evidence(step, &mut evidence);
    }
    evidence
}

fn scenario_feature_evidence(scenario: &Scenario) -> BTreeSet<String> {
    let mut evidence = BTreeSet::new();
    let mut disco_info_ids = BTreeSet::new();
    for step in &scenario.steps {
        if let Step::SendIq {
            id: Some(id),
            payload: Some(payload),
            ..
        } = step
        {
            if payload.name == "query" && payload.ns == "http://jabber.org/protocol/disco#info" {
                disco_info_ids.insert(id.as_str());
            }
        }
        if let Step::ExpectIq {
            id: Some(id),
            type_: Some(IqResponseKind::Result),
            contains,
            elements,
            ..
        } = step
        {
            if disco_info_ids.contains(id.as_str()) {
                for text in contains {
                    add_text_feature_evidence(text, &mut evidence);
                }
                for element in elements {
                    add_xml_spec_feature_evidence(element, &mut evidence);
                }
            }
        }
    }
    evidence
}

fn add_step_xep_evidence(step: &Step, evidence: &mut BTreeSet<&'static str>) {
    match step {
        Step::EnableCarbons { .. } => {
            evidence.insert("XEP-0280");
        }
        Step::StreamManagement { .. } => {
            evidence.insert("XEP-0198");
        }
        Step::DisconnectActor { .. } => {
            evidence.insert("XEP-0160");
        }
        Step::ConnectActor { .. } | Step::WaitMillis { .. } | Step::ExpectNoStanza { .. } => {}
        Step::SendIq { payload, .. } => {
            if let Some(payload) = payload {
                add_xml_spec_xep_evidence(payload, evidence);
            }
            if let Step::SendIq {
                to: Some(to),
                payload: Some(payload),
                ..
            } = step
            {
                if payload.name == "ping" && payload.ns == "urn:xmpp:ping" && to.contains('/') {
                    evidence.insert("XEP-0410");
                }
            }
        }
        Step::ExpectIq {
            contains, elements, ..
        } => {
            for text in contains {
                add_text_xep_evidence(text, evidence);
            }
            for element in elements {
                add_xml_spec_xep_evidence(element, evidence);
            }
        }
        Step::SendPresence { payloads, .. }
        | Step::ExpectPresence {
            elements: payloads, ..
        } => {
            for payload in payloads {
                add_xml_spec_xep_evidence(payload, evidence);
            }
        }
        Step::SendMessage { body, payloads, .. } => {
            add_payloads_xep_evidence(payloads, evidence);
            if body.is_some()
                && payloads
                    .iter()
                    .any(|payload| matches!(payload, Payload::FileShare { .. }))
            {
                evidence.insert("XEP-0428");
            }
        }
        Step::SendMessageBurst { .. } => {}
        Step::ExpectMessage {
            payloads,
            contains,
            elements,
            ..
        }
        | Step::ExpectCarbon {
            payloads,
            contains,
            elements,
            ..
        }
        | Step::ExpectMamResult {
            payloads,
            contains,
            elements,
            ..
        } => {
            add_payloads_xep_evidence(payloads, evidence);
            for text in contains {
                add_text_xep_evidence(text, evidence);
            }
            for element in elements {
                add_xml_spec_xep_evidence(element, evidence);
            }
            if matches!(step, Step::ExpectMamResult { .. }) {
                evidence.insert("XEP-0297");
            }
        }
        Step::JoinMuc { .. }
        | Step::SetMucAffiliation { .. }
        | Step::ExpectMucAffiliation { .. }
        | Step::ExpectMucAdminDenied { .. } => {
            evidence.insert("XEP-0045");
        }
        Step::QueryMam { fulltext, .. } => {
            evidence.insert("XEP-0004");
            evidence.insert("XEP-0059");
            evidence.insert("XEP-0313");
            if fulltext.is_some() {
                evidence.insert("XEP-0431");
            }
        }
        Step::ExpectNoMamResult {
            contains, elements, ..
        } => {
            for text in contains {
                add_text_xep_evidence(text, evidence);
            }
            for element in elements {
                add_xml_spec_xep_evidence(element, evidence);
            }
        }
        Step::ExpectFrame {
            contains, elements, ..
        } => {
            for text in contains {
                add_text_xep_evidence(text, evidence);
            }
            for element in elements {
                add_xml_spec_xep_evidence(element, evidence);
            }
        }
        Step::DrainFrames {
            contains, elements, ..
        } => {
            for text in contains {
                add_text_xep_evidence(text, evidence);
            }
            for element in elements {
                add_xml_spec_xep_evidence(element, evidence);
            }
        }
    }
}

fn add_payloads_xep_evidence(payloads: &[Payload], evidence: &mut BTreeSet<&'static str>) {
    for payload in payloads {
        match payload {
            Payload::FileShare { .. } => {
                evidence.insert("XEP-0103");
                evidence.insert("XEP-0446");
                evidence.insert("XEP-0447");
            }
            Payload::LinkMetadata { .. } => {
                evidence.insert("XEP-0511");
            }
            Payload::MessageCorrection { .. } => {
                evidence.insert("XEP-0308");
            }
            Payload::Reactions { .. } => {
                evidence.insert("XEP-0444");
            }
            Payload::ProcessingHint { .. } => {
                evidence.insert("XEP-0334");
            }
            Payload::PinAttachment { .. } | Payload::PinEvent { .. } => {}
            Payload::Xml {
                element,
                expect_elements,
            } => {
                add_xml_spec_xep_evidence(element, evidence);
                for element in expect_elements {
                    add_xml_spec_xep_evidence(element, evidence);
                }
            }
        }
    }
}

fn add_xml_spec_xep_evidence(element: &XmlElementSpec, evidence: &mut BTreeSet<&'static str>) {
    add_namespace_xep_evidence(&element.ns, evidence);
    for value in element.attrs.values().chain(element.attrs_from.values()) {
        add_text_xep_evidence(value, evidence);
    }
    if let Some(text) = &element.text {
        add_text_xep_evidence(text, evidence);
    }
    for child in &element.children {
        add_xml_spec_xep_evidence(child, evidence);
    }
}

fn add_xml_spec_feature_evidence(element: &XmlElementSpec, evidence: &mut BTreeSet<String>) {
    add_text_feature_evidence(&element.ns, evidence);
    for value in element.attrs.values().chain(element.attrs_from.values()) {
        add_text_feature_evidence(value, evidence);
    }
    if let Some(text) = &element.text {
        add_text_feature_evidence(text, evidence);
    }
    for child in &element.children {
        add_xml_spec_feature_evidence(child, evidence);
    }
}

fn add_text_xep_evidence(text: &str, evidence: &mut BTreeSet<&'static str>) {
    for (needle, xep) in [
        ("urn:ietf:params:xml:ns:xcal", "PROTO-CALENDAR"),
        ("jabber:x:data", "XEP-0004"),
        ("jabber:iq:last", "XEP-0012"),
        ("http://jabber.org/protocol/disco#info", "XEP-0030"),
        ("http://jabber.org/protocol/disco#items", "XEP-0030"),
        ("http://jabber.org/protocol/muc", "XEP-0045"),
        ("storage:bookmarks", "XEP-0048"),
        ("jabber:iq:private", "XEP-0049"),
        ("http://jabber.org/protocol/commands", "XEP-0050"),
        ("vcard-temp", "XEP-0054"),
        ("jabber:iq:search", "XEP-0055"),
        ("http://jabber.org/protocol/pubsub", "XEP-0060"),
        ("http://jabber.org/protocol/pubsub#pep", "XEP-0163"),
        ("urn:xmpp:avatar:metadata", "XEP-0084"),
        ("urn:xmpp:avatar:data", "XEP-0084"),
        ("http://jabber.org/protocol/chatstates", "XEP-0085"),
        ("jabber:iq:version", "XEP-0092"),
        ("http://jabber.org/protocol/mood", "XEP-0107"),
        ("http://jabber.org/protocol/activity", "XEP-0108"),
        ("http://jabber.org/protocol/caps", "XEP-0115"),
        ("http://jabber.org/protocol/tune", "XEP-0118"),
        ("vcard-temp:x:update", "XEP-0153"),
        ("urn:xmpp:receipts", "XEP-0184"),
        ("urn:xmpp:blocking", "XEP-0191"),
        ("urn:xmpp:sm:3", "XEP-0198"),
        ("urn:xmpp:ping", "XEP-0199"),
        ("urn:xmpp:time", "XEP-0202"),
        ("urn:xmpp:delay", "XEP-0203"),
        ("urn:xmpp:features:rosterver", "XEP-0237"),
        ("urn:ietf:params:xml:ns:vcard-4.0", "XEP-0292"),
        ("urn:xmpp:forward:0", "XEP-0297"),
        ("urn:xmpp:message-correct:0", "XEP-0308"),
        ("urn:xmpp:mam:2", "XEP-0313"),
        ("urn:xmpp:hats:0", "XEP-0317"),
        ("urn:xmpp:chat-markers:0", "XEP-0333"),
        ("urn:xmpp:push:0", "XEP-0357"),
        ("urn:xmpp:sid:0", "XEP-0359"),
        ("urn:xmpp:http:upload:0", "XEP-0363"),
        ("urn:xmpp:reference:0", "XEP-0372"),
        ("urn:xmpp:bookmarks:1", "XEP-0402"),
        ("urn:xmpp:occupant-id:0", "XEP-0421"),
        ("urn:xmpp:message-retract:1", "XEP-0424"),
        ("urn:xmpp:message-moderate:1", "XEP-0425"),
        ("urn:xmpp:fallback:0", "XEP-0428"),
        ("urn:xmpp:inbox:1", "XEP-0430"),
        ("urn:xmpp:fulltext:0", "XEP-0431"),
        ("urn:xmpp:channel-search:0", "XEP-0433"),
        ("urn:xmpp:reactions:0", "XEP-0444"),
        ("urn:xmpp:file:metadata:0", "XEP-0446"),
        ("urn:xmpp:sfs:0", "XEP-0447"),
        ("urn:xmpp:reply:0", "XEP-0461"),
        ("urn:xmpp:pubsub-social-feed:1", "XEP-0472"),
        ("urn:xmpp:notification-settings:1", "XEP-0492"),
        ("urn:xmpp:pubsub-social-feed:stories:0", "XEP-0501"),
        ("urn:xmpp:spaces:0", "XEP-0503"),
        ("urn:xmpp:mentions:0", "XEP-0513"),
    ] {
        if text.contains(needle) {
            evidence.insert(xep);
        }
    }
    if text.contains("thread") {
        evidence.insert("XEP-0201");
    }
    if text.contains("cue-muc-self-ping") {
        evidence.insert("XEP-0410");
    }
}

fn add_namespace_xep_evidence(ns: &str, evidence: &mut BTreeSet<&'static str>) {
    add_text_xep_evidence(ns, evidence);
}

fn add_text_feature_evidence(text: &str, evidence: &mut BTreeSet<String>) {
    for (feature, _) in ADVERTISED_FEATURE_XEPS {
        if text == *feature
            || text.contains(&format!("var='{feature}'"))
            || text.contains(&format!("\"{feature}\""))
            || text.contains(&format!("'{feature}'"))
        {
            evidence.insert((*feature).to_string());
        }
    }
}

fn implemented_official_xep_modules() -> Result<BTreeSet<String>> {
    let xep_mod = server_root().join("crates/waddle-xmpp/src/xep/mod.rs");
    let content =
        fs::read_to_string(&xep_mod).with_context(|| format!("read {}", xep_mod.display()))?;
    let mut implemented = BTreeSet::new();
    for line in content.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("pub mod xep") else {
            continue;
        };
        let Some(number) = rest.strip_suffix(';') else {
            continue;
        };
        if number.len() == 4 && number.chars().all(|c| c.is_ascii_digit()) {
            implemented.insert(format!("XEP-{number}"));
        }
    }
    for xep in discover_xep_modules_in_dir(&server_root().join("crates/waddle-xmpp-core/src"))? {
        implemented.insert(xep);
    }
    Ok(implemented)
}

fn discover_xep_modules_in_dir(dir: &Path) -> Result<BTreeSet<String>> {
    let mut modules = BTreeSet::new();
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file_name.starts_with("xep")
            || path.extension().and_then(|ext| ext.to_str()) != Some("rs")
        {
            continue;
        }
        modules.extend(xep_ids_in_name(file_name));
    }
    Ok(modules)
}

fn dedicated_rust_xep_suites() -> Result<BTreeSet<String>> {
    let server = server_root();
    let mut suites = BTreeSet::new();

    for dir in [
        server.join("crates/waddle-xmpp/tests"),
        server.join("crates/waddle-server/tests"),
    ] {
        collect_integration_xep_suites(&dir, &mut suites)?;
    }

    collect_module_xep_suites(&server.join("crates/waddle-xmpp/src/xep"), &mut suites)?;
    collect_module_xep_suites(&server.join("crates/waddle-xmpp-core/src"), &mut suites)?;

    Ok(suites)
}

fn collect_integration_xep_suites(dir: &Path, suites: &mut BTreeSet<String>) -> Result<()> {
    for path in rust_files_in(dir)? {
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file_name.starts_with("xep") {
            continue;
        }
        if !rust_file_has_test_function(&path)? {
            continue;
        }
        suites.extend(xep_ids_in_name(file_name));
    }
    Ok(())
}

fn collect_module_xep_suites(dir: &Path, suites: &mut BTreeSet<String>) -> Result<()> {
    for path in rust_files_in(dir)? {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if file_name == "tests.rs" {
            if !rust_file_has_test_function(&path)? {
                continue;
            }
            if let Some(parent) = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
            {
                suites.extend(xep_ids_in_name(parent));
            }
        } else if (file_name.contains("_tests") && rust_file_has_test_function(&path)?)
            || (file_name.starts_with("xep") && source_file_has_inline_test_module(&path)?)
        {
            suites.extend(xep_ids_in_name(file_name));
        }
    }
    Ok(())
}

fn source_file_has_inline_test_module(path: &Path) -> Result<bool> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(content.contains("#[cfg(test)]")
        && content.contains("mod tests")
        && rust_file_has_test_function(path)?)
}

fn meaningful_rust_feature_coverage() -> Result<BTreeSet<String>> {
    let server = server_root();
    let mut covered = BTreeSet::new();
    for dir in [
        server.join("crates/waddle-server/tests"),
        server.join("crates/waddle-xmpp/tests"),
    ] {
        for path in rust_files_in(&dir)? {
            if path.file_name().and_then(|name| name.to_str()) == Some("xmpp_e2e_cue.rs") {
                continue;
            }
            if !rust_file_has_test_function(&path)? {
                continue;
            }
            let content =
                fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
            for line in content
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
            {
                add_text_feature_evidence(line, &mut covered);
            }
        }
    }
    Ok(covered)
}

fn rust_file_has_test_function(path: &Path) -> Result<bool> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(content.contains("#[test]") || content.contains("#[tokio::test]"))
}

fn rust_files_in(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_rust_files(dir, &mut files)?;
    Ok(files)
}

fn collect_rust_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_rust_files(&path, files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn xep_ids_in_name(name: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let bytes = name.as_bytes();
    for (idx, window) in bytes.windows(4).enumerate() {
        if !window.iter().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let before = idx.checked_sub(1).and_then(|i| bytes.get(i)).copied();
        let after = bytes.get(idx + 4).copied();
        if before.is_some_and(|byte| byte.is_ascii_digit())
            || after.is_some_and(|byte| byte.is_ascii_digit())
        {
            continue;
        }
        let number = &name[idx..idx + 4];
        ids.insert(format!("XEP-{number}"));
    }
    ids
}

fn xep_exemption_map(
    exemptions: &'static [XepCoverageExemption],
) -> Result<BTreeMap<&'static str, &'static str>> {
    let mut by_xep = BTreeMap::new();
    let mut errors = Vec::new();
    for exemption in exemptions {
        if exemption.reason.trim().is_empty() {
            errors.push(format!("{} has an empty exemption reason", exemption.xep));
        }
        if by_xep.insert(exemption.xep, exemption.reason).is_some() {
            errors.push(format!("{} has duplicate exemptions", exemption.xep));
        }
    }
    if errors.is_empty() {
        Ok(by_xep)
    } else {
        Err(anyhow!(
            "invalid XEP coverage exemptions: {}",
            errors.join(", ")
        ))
    }
}

fn feature_exemption_map(
    exemptions: &'static [FeatureCoverageExemption],
) -> Result<BTreeMap<&'static str, &'static str>> {
    let mut by_feature = BTreeMap::new();
    let mut errors = Vec::new();
    for exemption in exemptions {
        if exemption.reason.trim().is_empty() {
            errors.push(format!(
                "{} has an empty exemption reason",
                exemption.feature
            ));
        }
        if by_feature
            .insert(exemption.feature, exemption.reason)
            .is_some()
        {
            errors.push(format!("{} has duplicate exemptions", exemption.feature));
        }
    }
    if errors.is_empty() {
        Ok(by_feature)
    } else {
        Err(anyhow!(
            "invalid advertised feature coverage exemptions: {}",
            errors.join(", ")
        ))
    }
}

fn server_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("waddle-server lives under server/crates")
        .to_path_buf()
}

fn advertised_feature_vars() -> BTreeSet<String> {
    waddle_xmpp::disco::info::server_features()
        .into_iter()
        .chain(waddle_xmpp::disco::info::upload_service_features())
        .chain(waddle_xmpp::disco::info::pubsub_service_features())
        .chain(waddle_xmpp::disco::info::push_service_features())
        .chain(waddle_xmpp::disco::info::community_service_features())
        .chain(waddle_xmpp::disco::info::spaces_service_features())
        .chain(waddle_xmpp::disco::info::muc_service_features())
        .chain(waddle_xmpp::disco::info::muc_room_features(
            true, true, false, false, true,
        ))
        .chain(waddle_xmpp::disco::info::muc_room_features(
            true, false, true, false, false,
        ))
        .chain(waddle_xmpp::disco::info::call_features())
        .chain(waddle_xmpp::pubsub::pep_features())
        .chain([waddle_xmpp::disco::Feature::new("jabber:iq:search")])
        .map(|feature| feature.0)
        .collect()
}

fn discover_scenario_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("cue")
            && path.file_name().and_then(|name| name.to_str()) != Some("schema.cue")
        {
            files.push(path);
        }
    }
    files.sort();
    if files.is_empty() {
        return Err(anyhow!("no CUE scenario files in {}", root.display()));
    }
    Ok(files)
}

fn load_scenario_from_file(root: &Path, scenario_file: &Path) -> Result<Scenario> {
    let temp_dir = tempfile::tempdir().context("create temporary CUE package")?;
    copy_dir_recursive(&root.join("cue.mod"), &temp_dir.path().join("cue.mod"))?;
    fs::copy(root.join("schema.cue"), temp_dir.path().join("schema.cue"))?;
    fs::copy(scenario_file, temp_dir.path().join("scenario.cue"))?;

    let parsed: ScenarioFile =
        cuengine::evaluate_cue_package_typed(temp_dir.path(), "xmpp_e2e_scenarios")
            .with_context(|| format!("evaluate CUE package for {}", scenario_file.display()))?;
    validate_scenario(&parsed.scenario)?;
    Ok(parsed.scenario)
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target).with_context(|| format!("create {}", target.display()))?;
    for entry in fs::read_dir(source).with_context(|| format!("read {}", source.display()))? {
        let entry = entry?;
        let path = entry.path();
        let destination = target.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &destination)?;
        } else {
            fs::copy(&path, &destination)
                .with_context(|| format!("copy {} to {}", path.display(), destination.display()))?;
        }
    }
    Ok(())
}

fn validate_scenario(scenario: &Scenario) -> Result<()> {
    if scenario.users.is_empty() {
        return Err(anyhow!("scenario {} has no users", scenario.name));
    }
    if scenario.steps.is_empty() {
        return Err(anyhow!("scenario {} has no steps", scenario.name));
    }
    Ok(())
}

async fn run_scenario(scenario: Scenario) -> Result<()> {
    let accounts = scenario_accounts(&scenario);
    let account_refs = accounts
        .iter()
        .map(|(username, password)| (username.as_str(), password.as_str()))
        .collect::<Vec<_>>();
    let server = TestServer::start_with_extra_accounts(&account_refs);
    let ws_url = server.ws_url();
    let admin_password = server.fixed_account_password().to_string();
    let mut clients = HashMap::new();

    for user in scenario.users.values() {
        for actor in user.devices.values() {
            let password = account_password(&accounts, &admin_password, &actor.username)?;
            let client = WsXmppClient::connect_and_auth(
                &ws_url,
                &scenario.domain,
                &actor.username,
                password,
                &actor.resource,
            )
            .await
            .map_err(|error| anyhow!("connect {}.{}: {error}", actor.user, actor.device))?;
            clients.insert(actor_key(actor), client);
        }
    }

    let mut ctx = ScenarioContext {
        clients,
        pending_frames: HashMap::new(),
        last_mam_frames: Vec::new(),
        last_mam_frame_index: 0,
        captures: HashMap::new(),
        sm_state: HashMap::new(),
        ws_url,
        domain: scenario.domain.clone(),
        admin_password,
        account_passwords: accounts,
    };

    let mut step_result = Ok(());
    for (index, step) in scenario.steps.iter().enumerate() {
        if let Err(error) = execute_step(&mut ctx, step)
            .await
            .with_context(|| format!("step {index} in scenario {}", scenario.name))
        {
            step_result = Err(error);
            break;
        }
    }
    if step_result.is_ok() {
        step_result = assert_mam_results_consumed(&ctx)
            .and_then(|()| assert_no_pending_frames(&ctx))
            .with_context(|| format!("scenario {} ended", scenario.name));
    }

    let close_result = close_clients(ctx.clients).await;
    step_result?;
    close_result?;
    Ok(())
}

fn scenario_accounts(scenario: &Scenario) -> BTreeMap<String, String> {
    let mut accounts = BTreeMap::new();
    for user in scenario.users.values() {
        for actor in user.devices.values() {
            if actor.username == "admin" {
                continue;
            }
            accounts
                .entry(actor.username.clone())
                .or_insert_with(|| format!("{}-{}", actor.username, uuid::Uuid::new_v4()));
        }
    }
    accounts
}

fn account_password<'a>(
    accounts: &'a BTreeMap<String, String>,
    admin_password: &'a str,
    username: &str,
) -> Result<&'a str> {
    accounts
        .get(username)
        .map(String::as_str)
        .or_else(|| (username == "admin").then_some(admin_password))
        .ok_or_else(|| anyhow!("missing password for {username}"))
}

async fn close_clients(clients: HashMap<String, WsXmppClient>) -> Result<()> {
    let mut errors = Vec::new();
    for client in clients.into_values() {
        if let Err(error) = client.close().await {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(
            "failed to close {} scenario client(s): {}",
            errors.len(),
            errors.join("; ")
        ))
    }
}

async fn disconnect_actor(ctx: &mut ScenarioContext, actor: &Actor, graceful: bool) -> Result<()> {
    let key = actor_key(actor);
    assert_actor_has_no_pending_frames(ctx, actor)?;
    ctx.pending_frames.remove(&key);
    // SM state is per-stream — the next ConnectActor opens a fresh
    // TCP/WebSocket and any continuation comes through a `<resume/>`
    // action that re-flips `enabled`. Carrying stale state across
    // the disconnect boundary would let the harness auto-ack on a
    // new stream that the server hasn't actually negotiated SM on
    // yet.
    ctx.sm_state.remove(&key);
    if let Some(client) = ctx.clients.remove(&key) {
        if graceful {
            client
                .close()
                .await
                .map_err(|error| anyhow!("disconnect {}.{}: {error}", actor.user, actor.device))?;
        } else {
            drop(client);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    Ok(())
}

async fn reconnect_actor(ctx: &mut ScenarioContext, actor: &Actor, bind: bool) -> Result<()> {
    disconnect_actor(ctx, actor, true).await?;
    let password = account_password(&ctx.account_passwords, &ctx.admin_password, &actor.username)?;
    let client = if bind {
        WsXmppClient::connect_and_auth(
            &ctx.ws_url,
            &ctx.domain,
            &actor.username,
            password,
            &actor.resource,
        )
        .await
    } else {
        let mut client = WsXmppClient::connect(&ctx.ws_url)
            .await
            .map_err(|error| anyhow!(error))?;
        client
            .authenticate(&ctx.domain, &actor.username, password)
            .await
            .map_err(|error| anyhow!(error))?;
        Ok(client)
    }
    .map_err(|error| anyhow!("reconnect {}.{}: {error}", actor.user, actor.device))?;
    ctx.clients.insert(actor_key(actor), client);
    Ok(())
}

async fn execute_step(ctx: &mut ScenarioContext, step: &Step) -> Result<()> {
    match step {
        Step::EnableCarbons { actor } => {
            let id = format!("cue-enable-carbons-{}", uuid::Uuid::new_v4());
            let enable = Element::builder("enable", "urn:xmpp:carbons:2").build();
            let iq = Iq::Set {
                from: None,
                to: None,
                id: id.clone(),
                payload: enable,
            };
            let client = client_mut(ctx, actor)?;
            client
                .send(&stanza_xml(Stanza::Iq(Box::new(iq)))?)
                .await
                .map_err(|error| anyhow!(error))?;
            let response = recv_matching(ctx, actor, |frame| frame.contains(&id)).await?;
            assert_contains_all(&response, ["type='result'"], "enable carbons response")?;
        }
        Step::StreamManagement {
            actor,
            action,
            resume,
            max,
            previd_from,
            h,
        } => {
            let element = match action {
                StreamManagementAction::Enable => {
                    let mut builder = Element::builder("enable", "urn:xmpp:sm:3");
                    if let Some(resume) = resume {
                        builder = builder.attr(
                            minidom::rxml::xml_ncname!("resume").to_owned(),
                            if *resume { "true" } else { "false" },
                        );
                    }
                    let max_value = max.map(|value| value.to_string());
                    if let Some(max) = max_value.as_deref() {
                        builder = builder.attr(minidom::rxml::xml_ncname!("max").to_owned(), max);
                    }
                    builder.build()
                }
                StreamManagementAction::RequestAck => {
                    Element::builder("r", "urn:xmpp:sm:3").build()
                }
                StreamManagementAction::Resume => {
                    let capture = previd_from
                        .as_deref()
                        .ok_or_else(|| anyhow!("streamManagement resume requires previdFrom"))?;
                    let previd = ctx
                        .captures
                        .get(capture)
                        .ok_or_else(|| anyhow!("unknown captured stream id {capture:?}"))?;
                    let h = h.ok_or_else(|| anyhow!("streamManagement resume requires h"))?;
                    Element::builder("resume", "urn:xmpp:sm:3")
                        .attr(
                            minidom::rxml::xml_ncname!("previd").to_owned(),
                            previd.as_str(),
                        )
                        .attr(minidom::rxml::xml_ncname!("h").to_owned(), h.to_string())
                        .build()
                }
            };
            let xml = element_xml(&element)?;
            client_mut(ctx, actor)?
                .send(&xml)
                .await
                .map_err(|error| anyhow!(error))?;
            // Mark SM negotiated on the actor's stream so subsequent
            // server-side `<r/>` requests get auto-acked by
            // `recv_timeout`. `RequestAck` is a client-initiated
            // probe that doesn't change SM enabled-ness.
            //
            // We flip the flag eagerly (before waiting for
            // `<enabled/>` / `<resumed/>`) because the negotiation
            // response arrives back through `recv_timeout` itself —
            // we'd deadlock if the auto-ack path waited for
            // negotiation to complete first. A premature auto-ack on
            // a stream the server rejects with `<failed/>` is
            // harmless: the server is tearing the stream down anyway.
            if matches!(
                action,
                StreamManagementAction::Enable | StreamManagementAction::Resume
            ) {
                let state = ctx.sm_state.entry(actor_key(actor)).or_default();
                state.enabled = true;
            }
        }
        Step::DisconnectActor { actor, graceful } => {
            disconnect_actor(ctx, actor, graceful.unwrap_or(true)).await?;
        }
        Step::WaitMillis { millis } => {
            tokio::time::sleep(Duration::from_millis(*millis)).await;
        }
        Step::ConnectActor { actor, bind } => {
            reconnect_actor(ctx, actor, bind.unwrap_or(true)).await?;
        }
        Step::SendIq {
            actor,
            type_,
            id,
            to,
            payload,
        } => {
            let id = id
                .clone()
                .unwrap_or_else(|| format!("cue-iq-{}", uuid::Uuid::new_v4()));
            let payload = payload
                .as_ref()
                .map(|payload| xml_element(payload, Some(ctx)))
                .transpose()?;
            let iq_payload = match (type_, payload) {
                (IqKindSpec::Get, Some(payload)) => IqPayload::Get(payload),
                (IqKindSpec::Set, Some(payload)) => IqPayload::Set(payload),
                (IqKindSpec::Result, payload) => IqPayload::Result(payload),
                (IqKindSpec::Get | IqKindSpec::Set, None) => {
                    return Err(anyhow!("sendIq get/set requires a payload"))
                }
            };
            let iq = iq_payload.assemble(xmpp_parsers::iq::IqHeader {
                from: None,
                to: to.as_deref().map(str::parse).transpose()?,
                id,
            });
            client_mut(ctx, actor)?
                .send(&stanza_xml(Stanza::Iq(Box::new(iq)))?)
                .await
                .map_err(|error| anyhow!(error))?;
        }
        Step::ExpectIq {
            target,
            id,
            type_,
            contains,
            absent,
            elements,
            absent_elements,
            captures,
        } => {
            let mut expected = contains.clone();
            if let Some(id) = id {
                expected.push(format!("id='{id}'"));
            }
            if let Some(type_) = type_ {
                expected.push(format!("type='{}'", iq_response_kind_name(type_)));
            }
            let captures_snapshot = ctx.captures.clone();
            let frame = recv_matching(ctx, target, |frame| {
                frame.contains("<iq")
                    && id
                        .as_ref()
                        .is_none_or(|id| frame.contains(&format!("id='{id}'")))
                    && type_.as_ref().is_none_or(|type_| {
                        frame.contains(&format!("type='{}'", iq_response_kind_name(type_)))
                    })
                    && contains.iter().all(|part| frame.contains(part))
                    && elements
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
            })
            .await?;
            assert_contains_all(&frame, &expected, "IQ expectation")?;
            assert_absent_all(&frame, absent, "IQ expectation")?;
            assert_elements_present(&frame, elements, &captures_snapshot, "IQ expectation")?;
            assert_elements_absent(
                &frame,
                absent_elements,
                &captures_snapshot,
                "IQ expectation",
            )?;
            for capture in captures {
                let value = extract_attr_capture(&frame, capture)?;
                ctx.captures.insert(capture.capture_as.clone(), value);
            }
        }
        Step::SendPresence {
            actor,
            to,
            type_,
            show,
            status,
            priority,
            payloads,
        } => {
            let mut presence = Presence::new(match type_ {
                None | Some(PresenceKind::Available) => PresenceType::None,
                Some(PresenceKind::Unavailable) => PresenceType::Unavailable,
                Some(PresenceKind::Subscribe) => PresenceType::Subscribe,
                Some(PresenceKind::Subscribed) => PresenceType::Subscribed,
                Some(PresenceKind::Unsubscribe) => PresenceType::Unsubscribe,
                Some(PresenceKind::Unsubscribed) => PresenceType::Unsubscribed,
                Some(PresenceKind::Probe) => PresenceType::Probe,
            });
            presence.to = to.as_deref().map(str::parse).transpose()?;
            if let Some(show) = show {
                presence.show = Some(parse_presence_show(show)?);
            }
            if let Some(status) = status {
                presence
                    .statuses
                    .insert(xmpp_parsers::message::Lang::new(), status.clone());
            }
            if let Some(priority) = priority {
                presence = presence.with_priority(*priority);
            }
            for payload in payloads {
                presence.payloads.push(xml_element(payload, Some(ctx))?);
            }
            client_mut(ctx, actor)?
                .send(&stanza_xml(Stanza::Presence(presence))?)
                .await
                .map_err(|error| anyhow!(error))?;
        }
        Step::SendMessage {
            from,
            to,
            to_jid,
            type_,
            id,
            body,
            payloads,
        } => {
            let to = to_jid
                .clone()
                .or_else(|| to.as_ref().map(|actor| actor.jid.clone()))
                .ok_or_else(|| anyhow!("sendMessage requires to or toJid"))?;
            let mut message = Message::new_with_type(message_type(type_), Some(to.parse::<Jid>()?));
            message.id = id.clone().map(xmpp_parsers::message::Id);
            if let Some(body) = body {
                message
                    .bodies
                    .insert(xmpp_parsers::message::Lang(String::new()), body.clone());
            }
            for payload in payloads {
                message.payloads.push(payload_element(payload, ctx)?);
            }
            if body.is_some()
                && payloads
                    .iter()
                    .any(|payload| matches!(payload, Payload::FileShare { .. }))
            {
                validate_file_share_fallback_body(body.as_deref(), payloads)?;
                message.payloads.push(file_share_fallback_element());
            }
            let xml = stanza_xml(Stanza::Message(message))?;
            client_mut(ctx, from)?
                .send(&xml)
                .await
                .map_err(|error| anyhow!(error))?;
        }
        Step::SendMessageBurst {
            from,
            to,
            to_jid,
            type_,
            id_prefix,
            body_prefix,
            count,
        } => {
            let to = to_jid
                .clone()
                .or_else(|| to.as_ref().map(|actor| actor.jid.clone()))
                .ok_or_else(|| anyhow!("sendMessageBurst requires to or toJid"))?;
            for index in 0..*count {
                let mut message =
                    Message::new_with_type(message_type(type_), Some(to.parse::<Jid>()?));
                message.id = Some(xmpp_parsers::message::Id(format!("{id_prefix}-{index}")));
                message.bodies.insert(
                    xmpp_parsers::message::Lang::new(),
                    format!("{body_prefix}-{index}"),
                );
                let xml = stanza_xml(Stanza::Message(message))?;
                client_mut(ctx, from)?
                    .send(&xml)
                    .await
                    .map_err(|error| anyhow!(error))?;
            }
        }
        Step::ExpectMessage {
            target,
            body,
            body_absent,
            from,
            capture_stanza_id_as,
            capture_stanza_id_by,
            payloads,
            contains,
            absent,
            elements,
            absent_elements,
        } => {
            let mut expected = contains.clone();
            if let Some(body) = body {
                expected.push(body_text_marker(body));
            }
            let payload_expectations = payload_expectations(payloads, ctx)?;
            let payload_element_expectations = payload_element_expectations(payloads);
            expected.extend(payload_expectations.clone());
            if let Some(from) = from {
                // xmpp-parsers 0.22 serializes attribute values with single
                // quotes; accept either quote style at runtime.
                expected.push(format!("from='{}", from.bare_jid));
            }
            let captures_snapshot = ctx.captures.clone();
            let frame = recv_matching(ctx, target, |frame| {
                frame_is_live_message(frame)
                    && body
                        .as_ref()
                        .is_none_or(|body| frame_root_message_has_body(frame, Some(body)))
                    && (!*body_absent || !frame_root_message_has_body(frame, None))
                    && from.as_ref().is_none_or(|from| {
                        frame.contains(&format!("from=\"{}", from.bare_jid))
                            || frame.contains(&format!("from='{}", from.bare_jid))
                    })
                    && payload_expectations.iter().all(|part| frame.contains(part))
                    && payload_element_expectations
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
                    && contains.iter().all(|part| frame.contains(part))
                    && elements
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
            })
            .await?;
            if *body_absent && frame_root_message_has_body(&frame, None) {
                return Err(anyhow!(
                    "message expectation expected no <body> element, got: {frame}"
                ));
            }
            assert_contains_all(&frame, &expected, "message expectation")?;
            assert_absent_all(&frame, absent, "message expectation")?;
            assert_elements_present(
                &frame,
                &payload_element_expectations,
                &captures_snapshot,
                "message payload expectation",
            )?;
            assert_elements_present(&frame, elements, &captures_snapshot, "message expectation")?;
            assert_elements_absent(
                &frame,
                absent_elements,
                &captures_snapshot,
                "message expectation",
            )?;
            if let Some(capture_name) = capture_stanza_id_as {
                let stanza_id =
                    extract_stanza_id_from_frame(&frame, capture_stanza_id_by.as_deref())?;
                ctx.captures.insert(capture_name.clone(), stanza_id);
            }
        }
        Step::ExpectCarbon {
            target,
            carbon,
            body,
            body_absent,
            payloads,
            contains,
            absent,
            elements,
            absent_elements,
        } => {
            let carbon_tag = match carbon {
                CarbonKind::Sent => "<sent",
                CarbonKind::Received => "<received",
            };
            let mut expected = contains.clone();
            expected.push("urn:xmpp:carbons:2".to_string());
            expected.push(carbon_tag.to_string());
            if let Some(body) = body {
                expected.push(body_text_marker(body));
            }
            let payload_expectations = payload_expectations(payloads, ctx)?;
            let payload_element_expectations = payload_element_expectations(payloads);
            expected.extend(payload_expectations.clone());
            let captures_snapshot = ctx.captures.clone();
            let frame = recv_matching(ctx, target, |frame| {
                frame.contains("urn:xmpp:carbons:2")
                    && frame.contains(carbon_tag)
                    && body
                        .as_ref()
                        .is_none_or(|body| frame_contains_body(frame, body))
                    && (!*body_absent || !frame_has_direct_message_body(frame))
                    && payload_expectations.iter().all(|part| frame.contains(part))
                    && payload_element_expectations
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
                    && contains.iter().all(|part| frame.contains(part))
                    && elements
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
            })
            .await?;
            if *body_absent && frame_has_direct_message_body(&frame) {
                return Err(anyhow!(
                    "carbon expectation expected no <body> element, got: {frame}"
                ));
            }
            assert_contains_all(&frame, &expected, "carbon expectation")?;
            assert_absent_all(&frame, absent, "carbon expectation")?;
            assert_elements_present(
                &frame,
                &payload_element_expectations,
                &captures_snapshot,
                "carbon payload expectation",
            )?;
            assert_elements_present(&frame, elements, &captures_snapshot, "carbon expectation")?;
            assert_elements_absent(
                &frame,
                absent_elements,
                &captures_snapshot,
                "carbon expectation",
            )?;
        }
        Step::JoinMuc { actor, room, nick } => {
            let mut presence = Presence::available();
            presence.to = Some(format!("{room}/{nick}").parse()?);
            presence.payloads.push(
                Element::builder("x", "http://jabber.org/protocol/muc")
                    .append(
                        Element::builder("history", "http://jabber.org/protocol/muc")
                            .attr(minidom::rxml::xml_ncname!("maxstanzas").to_owned(), "0")
                            .build(),
                    )
                    .build(),
            );
            let xml = stanza_xml(Stanza::Presence(presence))?;
            let client = client_mut(ctx, actor)?;
            client.send(&xml).await.map_err(|error| anyhow!(error))?;
            recv_matching(ctx, actor, |frame| {
                frame_is_muc_join_self_presence(frame, room, nick)
            })
            .await?;
        }
        Step::SetMucAffiliation {
            actor,
            room,
            jid,
            affiliation,
            id,
        } => {
            let id = id
                .clone()
                .unwrap_or_else(|| format!("cue-muc-admin-set-{}", uuid::Uuid::new_v4()));
            send_muc_admin_iq(ctx, actor, room, jid, affiliation, &id, IqKind::Set).await?;
            let response = recv_matching(ctx, actor, |frame| frame.contains(&id)).await?;
            assert_contains_all(&response, ["type='result'"], "MUC admin set response")?;
        }
        Step::ExpectMucAffiliation {
            actor,
            room,
            jid,
            affiliation,
            id,
        } => {
            let id = id
                .clone()
                .unwrap_or_else(|| format!("cue-muc-admin-get-{}", uuid::Uuid::new_v4()));
            send_muc_admin_iq(ctx, actor, room, jid, affiliation, &id, IqKind::Get).await?;
            let response = recv_matching(ctx, actor, |frame| frame.contains(&id)).await?;
            assert_contains_all(
                &response,
                [
                    "type='result'",
                    "http://jabber.org/protocol/muc#admin",
                    jid.as_str(),
                    affiliation.as_str(),
                ],
                "MUC admin affiliation query",
            )?;
        }
        Step::ExpectMucAdminDenied {
            actor,
            room,
            jid,
            affiliation,
            id,
        } => {
            let id = id
                .clone()
                .unwrap_or_else(|| format!("cue-muc-admin-denied-{}", uuid::Uuid::new_v4()));
            send_muc_admin_iq(ctx, actor, room, jid, affiliation, &id, IqKind::Set).await?;
            let response = recv_matching(ctx, actor, |frame| frame.contains(&id)).await?;
            assert_contains_all(&response, ["type='error'", "forbidden"], "MUC admin denial")?;
        }
        Step::ExpectPresence {
            target,
            contains,
            elements,
            absent_elements,
            captures,
        } => {
            let captures_snapshot = ctx.captures.clone();
            let frame = recv_matching(ctx, target, |frame| {
                frame.contains("<presence")
                    && contains.iter().all(|part| frame.contains(part))
                    && elements
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
            })
            .await?;
            assert_contains_all(&frame, contains, "presence expectation")?;
            assert_elements_present(&frame, elements, &captures_snapshot, "presence expectation")?;
            assert_elements_absent(
                &frame,
                absent_elements,
                &captures_snapshot,
                "presence expectation",
            )?;
            for capture in captures {
                let value = extract_attr_capture(&frame, capture)?;
                ctx.captures.insert(capture.capture_as.clone(), value);
            }
        }
        Step::QueryMam {
            actor,
            archive,
            id,
            max,
            after,
            after_from,
            before,
            before_from,
            with_jid,
            fulltext,
            ids,
            ids_from,
        } => {
            assert_mam_results_consumed(ctx)?;
            let id = id
                .clone()
                .unwrap_or_else(|| format!("cue-mam-{}", uuid::Uuid::new_v4()));
            let mut query_ids = ids.clone();
            for capture in ids_from {
                let value = ctx
                    .captures
                    .get(capture)
                    .ok_or_else(|| anyhow!("unknown captured MAM id {capture}"))?;
                query_ids.push(value.clone());
            }
            let after_cursor = match (after.as_deref(), after_from.as_deref()) {
                (Some(_), Some(_)) => {
                    return Err(anyhow!("queryMam cannot set both after and afterFrom"));
                }
                (Some(after), None) => Some(after.to_string()),
                (None, Some(capture)) => Some(
                    ctx.captures
                        .get(capture)
                        .ok_or_else(|| anyhow!("unknown captured MAM after cursor {capture}"))?
                        .clone(),
                ),
                (None, None) => None,
            };
            let before_cursor = match (before.as_deref(), before_from.as_deref()) {
                (Some(_), Some(_)) => {
                    return Err(anyhow!("queryMam cannot set both before and beforeFrom"));
                }
                (Some(before), None) => Some(before.to_string()),
                (None, Some(capture)) => Some(
                    ctx.captures
                        .get(capture)
                        .ok_or_else(|| anyhow!("unknown captured MAM before cursor {capture}"))?
                        .clone(),
                ),
                (None, None) => None,
            };
            if after_cursor.is_some() && before_cursor.is_some() {
                return Err(anyhow!("queryMam cannot set both after and before cursors"));
            }
            let query = mam_query_element(
                &id,
                *max,
                after_cursor.as_deref(),
                before_cursor.as_deref(),
                with_jid.as_deref(),
                fulltext.as_deref(),
                &query_ids,
            );
            let iq = Iq::Set {
                from: None,
                to: Some(archive.parse()?),
                id: id.clone(),
                payload: query,
            };
            client_mut(ctx, actor)?
                .send(&stanza_xml(Stanza::Iq(Box::new(iq)))?)
                .await
                .map_err(|error| anyhow!(error))?;
            let mut mam_frames = Vec::new();
            let mut delayed_frames = Vec::new();
            loop {
                let frame = recv_next(ctx, actor).await?;
                if frame_is_mam_fin_for_query(&frame, &id) {
                    break;
                }
                if frame_contains_mam_result(&frame) {
                    match frame_mam_result_query_id(&frame).as_deref() {
                        Some(query_id) if query_id == id => mam_frames.push(frame),
                        Some(query_id) => {
                            return Err(anyhow!(
                                "received MAM result for query id {query_id} while waiting for {id}: {frame}"
                            ));
                        }
                        None => {
                            return Err(anyhow!(
                                "received MAM result without queryid while waiting for {id}: {frame}"
                            ));
                        }
                    }
                } else {
                    delayed_frames.push(frame);
                }
            }
            for frame in delayed_frames.into_iter().rev() {
                push_pending_front(ctx, actor, frame);
            }
            ctx.last_mam_frames = mam_frames;
            ctx.last_mam_frame_index = 0;
        }
        Step::ExpectMamResult {
            body,
            body_absent,
            payloads,
            contains,
            absent,
            elements,
            absent_elements,
            captures,
        } => {
            let payload_expectations = payload_expectations(payloads, ctx)?;
            let payload_element_expectations = payload_element_expectations(payloads);
            let captures_snapshot = ctx.captures.clone();
            let Some(frame) = ctx.last_mam_frames.get(ctx.last_mam_frame_index) else {
                return Err(anyhow!(
                    "no next MAM result available for body {:?} and contains {:?}; frames: {:?}",
                    body,
                    contains,
                    ctx.last_mam_frames
                ));
            };
            if !frame_contains_mam_result(frame)
                || body
                    .as_ref()
                    .is_some_and(|body| !frame_contains_body(frame, body))
                || (*body_absent && frame_has_direct_message_body(frame))
                || !payload_expectations.iter().all(|part| frame.contains(part))
                || !payload_element_expectations
                    .iter()
                    .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
                || !contains.iter().all(|part| frame.contains(part))
                || !elements
                    .iter()
                    .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
            {
                return Err(anyhow!(
                    "next MAM result did not match body {:?} and contains {:?}: {frame}",
                    body,
                    contains
                ));
            }
            let frame = frame.clone();
            ctx.last_mam_frame_index += 1;
            if let Some(body) = body {
                assert_contains_all(&frame, std::slice::from_ref(body), "MAM result body")?;
            }
            if *body_absent && frame_has_direct_message_body(&frame) {
                return Err(anyhow!(
                    "MAM result expected no <body> element, got: {frame}"
                ));
            }
            assert_contains_all(&frame, &payload_expectations, "MAM result payloads")?;
            assert_elements_present(
                &frame,
                &payload_element_expectations,
                &captures_snapshot,
                "MAM result payloads",
            )?;
            assert_contains_all(&frame, contains, "MAM result contains")?;
            assert_absent_all(&frame, absent, "MAM result absent")?;
            assert_elements_present(&frame, elements, &captures_snapshot, "MAM result elements")?;
            assert_elements_absent(
                &frame,
                absent_elements,
                &captures_snapshot,
                "MAM result elements",
            )?;
            for capture in captures {
                let value = extract_attr_capture(&frame, capture)?;
                ctx.captures.insert(capture.capture_as.clone(), value);
            }
        }
        Step::ExpectNoMamResult {
            body,
            body_absent,
            payloads,
            contains,
            elements,
        } => {
            let payload_expectations = payload_expectations(payloads, ctx)?;
            let payload_element_expectations = payload_element_expectations(payloads);
            let captures_snapshot = ctx.captures.clone();
            let matched = ctx.last_mam_frames.iter().find(|frame| {
                frame.contains("<forwarded")
                    && body
                        .as_ref()
                        .is_none_or(|body| frame_contains_body(frame, body))
                    && (!*body_absent || !frame_has_direct_message_body(frame))
                    && payload_expectations.iter().all(|part| frame.contains(part))
                    && payload_element_expectations
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
                    && contains.iter().all(|part| frame.contains(part))
                    && elements
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
            });
            if let Some(frame) = matched {
                return Err(anyhow!(
                    "unexpected MAM result matched body {:?} and contains {:?}: {frame}",
                    body,
                    contains
                ));
            }
        }
        Step::ExpectFrame {
            target,
            contains,
            absent,
            elements,
            absent_elements,
            captures,
        } => {
            let captures_snapshot = ctx.captures.clone();
            let frame = recv_matching(ctx, target, |frame| {
                contains.iter().all(|part| frame.contains(part))
                    && elements
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
            })
            .await?;
            assert_contains_all(&frame, contains, "frame expectation")?;
            assert_absent_all(&frame, absent, "frame expectation")?;
            assert_elements_present(&frame, elements, &captures_snapshot, "frame expectation")?;
            assert_elements_absent(
                &frame,
                absent_elements,
                &captures_snapshot,
                "frame expectation",
            )?;
            for capture in captures {
                let value = extract_attr_capture(&frame, capture)?;
                ctx.captures.insert(capture.capture_as.clone(), value);
            }
        }
        Step::DrainFrames {
            target,
            contains,
            elements,
            millis,
            min,
            max,
        } => {
            let captures_snapshot = ctx.captures.clone();
            let drain_millis = if *millis == 0 { 250 } else { *millis };
            let min_matches = min.unwrap_or(1);
            let max_matches = max.unwrap_or(min_matches);
            if max_matches < min_matches {
                return Err(anyhow!(
                    "drainFrames for {}.{} has max {} below min {}",
                    target.user,
                    target.device,
                    max_matches,
                    min_matches
                ));
            }
            let deadline = Instant::now() + Duration::from_millis(drain_millis);
            let mut non_matching_frames = Vec::new();
            let mut matched_frames = 0_u64;
            loop {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let Some(frame) = recv_timeout(ctx, target, deadline - now).await? else {
                    break;
                };
                let matches = contains.iter().all(|part| frame.contains(part))
                    && elements
                        .iter()
                        .all(|spec| frame_has_element(&frame, spec, &captures_snapshot));
                if !matches {
                    non_matching_frames.push(frame);
                } else {
                    matched_frames += 1;
                }
            }
            for frame in non_matching_frames.into_iter().rev() {
                push_pending_front(ctx, target, frame);
            }
            if matched_frames < min_matches {
                return Err(anyhow!(
                    "drainFrames for {}.{} matched {} frame(s), expected at least {} for {:?}",
                    target.user,
                    target.device,
                    matched_frames,
                    min_matches,
                    contains
                ));
            }
            if matched_frames > max_matches {
                return Err(anyhow!(
                    "drainFrames for {}.{} matched {} frame(s), expected at most {} for {:?}",
                    target.user,
                    target.device,
                    matched_frames,
                    max_matches,
                    contains
                ));
            }
        }
        Step::ExpectNoStanza {
            target,
            body,
            contains,
            millis,
        } => {
            let deadline = Instant::now() + Duration::from_millis(*millis);
            let mut non_matching_frames = Vec::new();
            loop {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let Some(frame) = recv_timeout(ctx, target, deadline - now).await? else {
                    break;
                };
                let matches = body
                    .as_ref()
                    .is_none_or(|body| frame_contains_body(&frame, body))
                    && contains.iter().all(|part| frame.contains(part));
                if matches {
                    return Err(anyhow!("unexpected matching stanza: {frame}"));
                }
                non_matching_frames.push(frame);
            }
            for frame in non_matching_frames.into_iter().rev() {
                push_pending_front(ctx, target, frame);
            }
        }
    }
    Ok(())
}

async fn recv_matching<F>(ctx: &mut ScenarioContext, actor: &Actor, predicate: F) -> Result<String>
where
    F: Fn(&str) -> bool,
{
    let mut non_matching_frames = Vec::new();
    loop {
        let Some(frame) = recv_timeout(ctx, actor, RECV_TIMEOUT).await? else {
            return Err(anyhow!(
                "Timeout waiting for matching frame; skipped frames: {:?}",
                non_matching_frames
            ));
        };
        if predicate(&frame) {
            for frame in non_matching_frames.into_iter().rev() {
                push_pending_front(ctx, actor, frame);
            }
            return Ok(frame);
        }
        non_matching_frames.push(frame);
    }
}

async fn recv_next(ctx: &mut ScenarioContext, actor: &Actor) -> Result<String> {
    recv_timeout(ctx, actor, RECV_TIMEOUT)
        .await?
        .ok_or_else(|| anyhow!("Timeout waiting for message"))
}

async fn recv_timeout(
    ctx: &mut ScenarioContext,
    actor: &Actor,
    timeout: Duration,
) -> Result<Option<String>> {
    let key = actor_key(actor);
    // Two transport-layer concerns that scenarios are written above:
    //
    // 1. XEP-0198 `<r/>` ack requests — the server now emits one per
    //    N countable outbound stanzas. A real client replies with
    //    `<a h='N'/>` after every `<r/>`; the cue harness models the
    //    same so scenarios don't have to mention SM mechanics.
    //    Eviction / resume-rejection behavior is covered by direct
    //    websocket-frame tests in `routes/websocket/tests/
    //    stream_management.rs` and by `state.rs` unit tests — the
    //    cue layer doesn't need to re-validate it.
    //
    // 2. `inbound_count` accounting: bumped whenever a countable
    //    stanza (message/presence/iq) crosses from the wire to the
    //    scenario. Frames replayed from `pending_frames` were already
    //    counted on their first wire arrival, so the pop branch does
    //    NOT bump again — that would double-count and desync `h`.
    loop {
        if let Some(frame) = ctx
            .pending_frames
            .get_mut(&key)
            .and_then(VecDeque::pop_front)
        {
            return Ok(Some(frame));
        }
        let frame = match client_mut(ctx, actor)?.recv_timeout(timeout).await {
            Ok(frame) => frame,
            Err(error) if error == "Timeout waiting for message" => return Ok(None),
            Err(error) => return Err(anyhow!(error)),
        };
        match classify_frame(&frame) {
            FrameKind::SmRequestAck => {
                let state = ctx.sm_state.entry(key.clone()).or_default();
                if state.enabled {
                    let ack = Element::builder("a", "urn:xmpp:sm:3")
                        .attr(
                            minidom::rxml::xml_ncname!("h").to_owned(),
                            state.inbound_count.to_string(),
                        )
                        .build();
                    let ack_xml = element_xml(&ack)?;
                    client_mut(ctx, actor)?
                        .send(&ack_xml)
                        .await
                        .map_err(|error| anyhow!(error))?;
                }
                continue;
            }
            FrameKind::CountableStanza => {
                let state = ctx.sm_state.entry(key.clone()).or_default();
                state.inbound_count = state.inbound_count.wrapping_add(1);
                return Ok(Some(frame));
            }
            FrameKind::Other => return Ok(Some(frame)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    /// `<r xmlns='urn:xmpp:sm:3'/>` — server asks us to ack.
    SmRequestAck,
    /// `<message>`, `<presence>`, `<iq>` — bumps the XEP-0198 inbound count.
    CountableStanza,
    /// Anything else (`<a/>`, `<enabled/>`, `<resumed/>`, `<failed/>`,
    /// stream features, raw text, etc). Returned as-is, not counted.
    Other,
}

fn classify_frame(xml: &str) -> FrameKind {
    let Ok(element) = Element::from_str(xml) else {
        return FrameKind::Other;
    };
    if element.name() == "r" && element.ns() == "urn:xmpp:sm:3" {
        return FrameKind::SmRequestAck;
    }
    match element.name() {
        "message" | "presence" | "iq" => FrameKind::CountableStanza,
        _ => FrameKind::Other,
    }
}

#[derive(Debug, Clone, Default)]
struct ActorSmState {
    /// True after the server's `<enabled/>` or `<resumed/>` lands.
    /// Gates auto-ack so the harness doesn't reply to `<r/>` on a
    /// connection that hasn't actually negotiated SM.
    enabled: bool,
    /// Count of countable inbound stanzas surfaced to the scenario,
    /// used as the `h` value when auto-acking.
    inbound_count: u32,
}

fn push_pending_front(ctx: &mut ScenarioContext, actor: &Actor, frame: String) {
    ctx.pending_frames
        .entry(actor_key(actor))
        .or_default()
        .push_front(frame);
}

fn client_mut<'a>(ctx: &'a mut ScenarioContext, actor: &Actor) -> Result<&'a mut WsXmppClient> {
    ctx.clients
        .get_mut(&actor_key(actor))
        .ok_or_else(|| anyhow!("unknown actor {}.{}", actor.user, actor.device))
}

fn actor_key(actor: &Actor) -> String {
    format!("{}.{}", actor.user, actor.device)
}

fn message_type(kind: &MessageKind) -> MessageType {
    match kind {
        MessageKind::Chat => MessageType::Chat,
        MessageKind::Normal => MessageType::Normal,
        MessageKind::Groupchat => MessageType::Groupchat,
    }
}

fn iq_response_kind_name(kind: &IqResponseKind) -> &'static str {
    match kind {
        IqResponseKind::Result => "result",
        IqResponseKind::Error => "error",
        IqResponseKind::Get => "get",
        IqResponseKind::Set => "set",
    }
}

enum IqKind {
    Get,
    Set,
}

fn mam_query_element(
    query_id: &str,
    max: u32,
    after: Option<&str>,
    before: Option<&str>,
    with_jid: Option<&str>,
    fulltext: Option<&str>,
    ids: &[String],
) -> Element {
    const MAM_NS: &str = "urn:xmpp:mam:2";
    const RSM_NS: &str = "http://jabber.org/protocol/rsm";
    const DATA_FORMS_NS: &str = "jabber:x:data";
    const FULLTEXT_MAM_FIELD: &str = "{urn:xmpp:fulltext:0}fulltext";

    let mut rsm = Element::builder("set", RSM_NS).append(
        Element::builder("max", RSM_NS)
            .append(max.to_string())
            .build(),
    );
    if let Some(after) = after {
        rsm = rsm.append(Element::builder("after", RSM_NS).append(after).build());
    }
    if let Some(before) = before {
        rsm = rsm.append(Element::builder("before", RSM_NS).append(before).build());
    }

    let has_form = with_jid.is_some() || fulltext.is_some() || !ids.is_empty();
    let mut query = Element::builder("query", MAM_NS)
        .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), query_id)
        .append(rsm.build());
    if has_form {
        let mut form = Element::builder("x", DATA_FORMS_NS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
            .append(data_form_field("FORM_TYPE", &[MAM_NS]));
        if let Some(with_jid) = with_jid {
            form = form.append(data_form_field("with", &[with_jid]));
        }
        if let Some(fulltext) = fulltext {
            form = form.append(data_form_field(FULLTEXT_MAM_FIELD, &[fulltext]));
        }
        if !ids.is_empty() {
            let values = ids.iter().map(String::as_str).collect::<Vec<_>>();
            form = form.append(data_form_field("ids", &values));
        }
        query = query.append(form.build());
    }
    query.build()
}

fn data_form_field(var: &str, values: &[&str]) -> Element {
    let mut field = Element::builder("field", "jabber:x:data")
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var);
    for value in values {
        field = field.append(
            Element::builder("value", "jabber:x:data")
                .append(*value)
                .build(),
        );
    }
    field.build()
}

async fn send_muc_admin_iq(
    ctx: &mut ScenarioContext,
    actor: &Actor,
    room: &str,
    jid: &str,
    affiliation: &str,
    id: &str,
    kind: IqKind,
) -> Result<()> {
    let item = match kind {
        IqKind::Get => Element::builder("item", "http://jabber.org/protocol/muc#admin")
            .attr(
                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                affiliation,
            )
            .build(),
        IqKind::Set => Element::builder("item", "http://jabber.org/protocol/muc#admin")
            .attr(minidom::rxml::xml_ncname!("jid").to_owned(), jid)
            .attr(
                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                affiliation,
            )
            .build(),
    };
    let query = Element::builder("query", "http://jabber.org/protocol/muc#admin")
        .append(item)
        .build();
    let payload = match kind {
        IqKind::Get => IqPayload::Get(query),
        IqKind::Set => IqPayload::Set(query),
    };
    let iq = payload.assemble(xmpp_parsers::iq::IqHeader {
        from: None,
        to: Some(room.parse()?),
        id: id.to_string(),
    });
    client_mut(ctx, actor)?
        .send(&stanza_xml(Stanza::Iq(Box::new(iq)))?)
        .await
        .map_err(|error| anyhow!(error))?;
    Ok(())
}

fn xml_element(spec: &XmlElementSpec, ctx: Option<&ScenarioContext>) -> Result<Element> {
    let mut builder = Element::builder(spec.name.as_str(), spec.ns.as_str());
    for (name, value) in &spec.attrs {
        let ncname = <minidom::rxml::NcName as TryFrom<&str>>::try_from(name.as_str())
            .map_err(|error| anyhow!("invalid attribute name {name:?}: {error}"))?;
        builder = builder.attr(ncname, value.as_str());
    }
    for (name, capture) in &spec.attrs_from {
        let value = ctx
            .and_then(|ctx| ctx.captures.get(capture))
            .ok_or_else(|| anyhow!("unknown captured attribute value {capture:?}"))?;
        let ncname = <minidom::rxml::NcName as TryFrom<&str>>::try_from(name.as_str())
            .map_err(|error| anyhow!("invalid attribute name {name:?}: {error}"))?;
        builder = builder.attr(ncname, value.as_str());
    }
    if let Some(text) = &spec.text {
        builder = builder.append(text.as_str());
    }
    for child in &spec.children {
        builder = builder.append(xml_element(child, ctx)?);
    }
    Ok(builder.build())
}

fn element_xml(element: &Element) -> Result<String> {
    let mut buf = Vec::new();
    element.write_to(&mut buf)?;
    Ok(String::from_utf8(buf)?)
}

fn payload_element(payload: &Payload, ctx: &ScenarioContext) -> Result<Element> {
    match payload {
        Payload::FileShare {
            disposition,
            name,
            media_type,
            size,
            url,
        } => Ok(Element::builder("file-sharing", "urn:xmpp:sfs:0")
            .attr(
                minidom::rxml::xml_ncname!("disposition").to_owned(),
                disposition.as_str(),
            )
            .append(
                Element::builder("file", "urn:xmpp:file:metadata:0")
                    .append(
                        Element::builder("media-type", "urn:xmpp:file:metadata:0")
                            .append(media_type.as_str())
                            .build(),
                    )
                    .append(
                        Element::builder("name", "urn:xmpp:file:metadata:0")
                            .append(name.as_str())
                            .build(),
                    )
                    .append(
                        Element::builder("size", "urn:xmpp:file:metadata:0")
                            .append(size.to_string())
                            .build(),
                    )
                    .build(),
            )
            .append(
                Element::builder("sources", "urn:xmpp:sfs:0")
                    .append(
                        Element::builder("url-data", "http://jabber.org/protocol/url-data")
                            .attr(
                                minidom::rxml::xml_ncname!("target").to_owned(),
                                url.as_str(),
                            )
                            .build(),
                    )
                    .build(),
            )
            .build()),
        Payload::LinkMetadata {
            about,
            title,
            description,
            url,
        } => Ok(
            Element::builder("Description", "http://www.w3.org/1999/02/22-rdf-syntax-ns#")
                .prefix(
                    Some("rdf".to_string()),
                    "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
                )
                .expect("static RDF prefix is unique")
                .attr_ns(
                    minidom::rxml::Namespace::from("http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
                    minidom::rxml::xml_ncname!("about").to_owned(),
                    about.as_str(),
                )
                .append(
                    Element::builder("title", "https://ogp.me/ns#")
                        .append(title.as_str())
                        .build(),
                )
                .append(
                    Element::builder("description", "https://ogp.me/ns#")
                        .append(description.as_str())
                        .build(),
                )
                .append(
                    Element::builder("url", "https://ogp.me/ns#")
                        .append(url.as_str())
                        .build(),
                )
                .build(),
        ),
        Payload::MessageCorrection { id } => {
            Ok(Element::builder("replace", "urn:xmpp:message-correct:0")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), id.as_str())
                .build())
        }
        Payload::Reactions {
            id,
            id_from,
            emojis,
        } => {
            let target_id = resolve_payload_id(ctx, id.as_deref(), id_from.as_deref())?;
            let emoji_refs = emojis.iter().map(String::as_str).collect::<Vec<_>>();
            Ok(xep0444::build_reactions_element(&target_id, &emoji_refs))
        }
        Payload::ProcessingHint { name } => Ok(xep0334::build_hint_element(Hint::from(name))),
        Payload::PinAttachment {
            id,
            id_from,
            action,
        } => {
            let target_id = resolve_payload_id(ctx, id.as_deref(), id_from.as_deref())?;
            let stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
                target_id,
                jid::Jid::from(
                    jid::BareJid::from_str("room@example.com")
                        .map_err(|e| anyhow!("invalid placeholder jid: {e}"))?,
                ),
            );
            let elem = match action {
                PinAction::Pinned => waddle_xmpp::xep::build_pinned_message_element(&stanza_id),
                PinAction::Unpinned => waddle_xmpp::xep::build_unpinned_message_element(&stanza_id),
            };
            Ok(elem)
        }
        Payload::PinEvent { .. } => Err(anyhow!(
            "PinEvent is an expected-only payload; cannot be sent"
        )),
        Payload::Xml { element, .. } => xml_element(element, Some(ctx)),
    }
}

fn payload_expectations(payloads: &[Payload], ctx: &ScenarioContext) -> Result<Vec<String>> {
    let mut expected = Vec::new();
    for payload in payloads {
        match payload {
            Payload::FileShare {
                disposition,
                name,
                media_type,
                size,
                url,
            } => {
                expected.extend([
                    "urn:xmpp:sfs:0".to_string(),
                    "urn:xmpp:file:metadata:0".to_string(),
                    "http://jabber.org/protocol/url-data".to_string(),
                    "disposition=".to_string(),
                    disposition.clone(),
                    text_node_marker(media_type),
                    text_node_marker(name),
                    text_node_marker(&size.to_string()),
                    "target=".to_string(),
                    url.clone(),
                ]);
            }
            Payload::LinkMetadata {
                about,
                title,
                description,
                url,
            } => {
                // minidom 0.18 serializes attribute prefixes based on the
                // generated namespace map; it may emit `rdf:about=` or
                // `tns0:about=` depending on how the writer assigns
                // prefixes. Look for `:about='…'` (any prefix) to stay
                // tolerant of either form.
                expected.extend([
                    "http://www.w3.org/1999/02/22-rdf-syntax-ns#".to_string(),
                    "https://ogp.me/ns#".to_string(),
                    format!(":about='{about}'"),
                    text_node_marker(title),
                    text_node_marker(description),
                    text_node_marker(url),
                ]);
            }
            Payload::MessageCorrection { id } => {
                expected.extend(["urn:xmpp:message-correct:0".to_string(), id.clone()]);
            }
            Payload::Reactions {
                id,
                id_from,
                emojis,
            } => {
                let target_id = resolve_payload_id(ctx, id.as_deref(), id_from.as_deref())?;
                expected.extend([
                    "urn:xmpp:reactions:0".to_string(),
                    format!("id='{target_id}'"),
                ]);
                expected.extend(normalized_reaction_text_markers(&target_id, emojis));
            }
            Payload::ProcessingHint { name } => {
                let hint = Hint::from(name);
                expected.extend([
                    "urn:xmpp:hints".to_string(),
                    format!("<{}", hint.element_name()),
                ]);
            }
            Payload::PinAttachment {
                id,
                id_from,
                action,
            } => {
                let target_id = resolve_payload_id(ctx, id.as_deref(), id_from.as_deref())?;
                let marker = match action {
                    PinAction::Pinned => "<pinned",
                    PinAction::Unpinned => "<unpinned",
                };
                expected.extend([
                    "urn:waddle:pin:0".to_string(),
                    marker.to_string(),
                    format!("target='{target_id}'"),
                ]);
            }
            Payload::PinEvent {
                id,
                id_from,
                action,
            } => {
                let target_id = resolve_payload_id(ctx, id.as_deref(), id_from.as_deref())?;
                let action_attr = match action {
                    PinAction::Pinned => "action='pinned'",
                    PinAction::Unpinned => "action='unpinned'",
                };
                expected.extend([
                    "urn:waddle:pin:0".to_string(),
                    "<pin-event".to_string(),
                    action_attr.to_string(),
                    format!("target='{target_id}'"),
                ]);
            }
            Payload::Xml { .. } => {}
        }
    }
    Ok(expected)
}

fn payload_element_expectations(payloads: &[Payload]) -> Vec<XmlElementSpec> {
    let mut expected = Vec::new();
    for payload in payloads {
        if let Payload::Xml {
            element,
            expect_elements,
        } = payload
        {
            expected.push(element.clone());
            expected.extend(expect_elements.clone());
        }
    }
    expected
}

fn resolve_payload_id(
    ctx: &ScenarioContext,
    id: Option<&str>,
    id_from: Option<&str>,
) -> Result<String> {
    match (id, id_from) {
        (Some(id), None) => Ok(id.to_string()),
        (None, Some(capture)) => ctx
            .captures
            .get(capture)
            .cloned()
            .ok_or_else(|| anyhow!("unknown captured id {capture:?}")),
        (Some(_), Some(_)) => Err(anyhow!("payload must specify id or idFrom, not both")),
        (None, None) => Err(anyhow!("payload requires id or idFrom")),
    }
}

impl From<&ProcessingHint> for Hint {
    fn from(value: &ProcessingHint) -> Self {
        match value {
            ProcessingHint::NoPermanentStore => Self::NoPermanentStore,
            ProcessingHint::NoStore => Self::NoStore,
            ProcessingHint::NoCopy => Self::NoCopy,
            ProcessingHint::Store => Self::Store,
        }
    }
}

fn validate_file_share_fallback_body(body: Option<&str>, payloads: &[Payload]) -> Result<()> {
    let Some(body) = body else {
        return Ok(());
    };
    let represented_by_payload = payloads.iter().any(|payload| match payload {
        Payload::FileShare { url, .. } => body == url,
        Payload::LinkMetadata { .. }
        | Payload::MessageCorrection { .. }
        | Payload::Reactions { .. }
        | Payload::ProcessingHint { .. }
        | Payload::PinAttachment { .. }
        | Payload::PinEvent { .. }
        | Payload::Xml { .. } => false,
    });
    if represented_by_payload {
        Ok(())
    } else {
        Err(anyhow!(
            "fileShare body is marked as XEP-0428 fallback, so it must be represented by the file-sharing payload"
        ))
    }
}

fn file_share_fallback_element() -> Element {
    Element::builder("fallback", "urn:xmpp:fallback:0")
        .attr(
            minidom::rxml::xml_ncname!("for").to_owned(),
            "urn:xmpp:sfs:0",
        )
        .append(Element::builder("body", "urn:xmpp:fallback:0").build())
        .build()
}

fn stanza_xml(stanza: Stanza) -> Result<String> {
    let mut buf = Vec::new();
    stanza.to_element().write_to(&mut buf)?;
    Ok(String::from_utf8(buf)?)
}

fn frame_contains_body(frame: &str, body: &str) -> bool {
    parse_frame(frame)
        .is_some_and(|element| element_contains_direct_message_body(&element, Some(body)))
}

fn frame_has_direct_message_body(frame: &str) -> bool {
    parse_frame(frame).is_some_and(|element| element_contains_direct_message_body(&element, None))
}

fn parse_frame(frame: &str) -> Option<Element> {
    Element::from_str(frame).ok()
}

fn frame_is_live_message(frame: &str) -> bool {
    parse_frame(frame).is_some_and(|element| {
        element.name() == "message"
            && !element
                .children()
                .any(|child| child.name() == "result" && child.ns() == "urn:xmpp:mam:2")
            && !element.children().any(|child| {
                matches!(child.name(), "sent" | "received") && child.ns() == "urn:xmpp:carbons:2"
            })
    })
}

fn frame_contains_mam_result(frame: &str) -> bool {
    parse_frame(frame).is_some_and(|element| {
        element.name() == "message"
            && element
                .children()
                .any(|child| child.name() == "result" && child.ns() == "urn:xmpp:mam:2")
            && find_named_element(&element, "forwarded", "urn:xmpp:forward:0")
    })
}

fn frame_mam_result_query_id(frame: &str) -> Option<String> {
    parse_frame(frame).and_then(|element| {
        element
            .children()
            .find(|child| child.name() == "result" && child.ns() == "urn:xmpp:mam:2")
            .and_then(|result| result.attr("queryid").map(ToOwned::to_owned))
    })
}

fn frame_is_mam_fin_for_query(frame: &str, query_id: &str) -> bool {
    parse_frame(frame).is_some_and(|element| {
        element.name() == "iq"
            && element.attr("id") == Some(query_id)
            && element
                .children()
                .any(|child| child.name() == "fin" && child.ns() == "urn:xmpp:mam:2")
    })
}

fn assert_mam_results_consumed(ctx: &ScenarioContext) -> Result<()> {
    if ctx.last_mam_frame_index == ctx.last_mam_frames.len() {
        return Ok(());
    }
    Err(anyhow!(
        "unconsumed MAM results after previous query: {:?}",
        &ctx.last_mam_frames[ctx.last_mam_frame_index..]
    ))
}

fn assert_no_pending_frames(ctx: &ScenarioContext) -> Result<()> {
    let pending = ctx
        .pending_frames
        .iter()
        .filter(|(_, frames)| !frames.is_empty())
        .map(|(actor, frames)| format!("{actor}: {frames:?}"))
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Ok(());
    }
    Err(anyhow!("unconsumed pending frames: {}", pending.join("; ")))
}

fn assert_actor_has_no_pending_frames(ctx: &ScenarioContext, actor: &Actor) -> Result<()> {
    let key = actor_key(actor);
    let Some(frames) = ctx.pending_frames.get(&key) else {
        return Ok(());
    };
    if frames.is_empty() {
        return Ok(());
    }
    Err(anyhow!(
        "unconsumed pending frames before disconnecting {key}: {frames:?}"
    ))
}

fn frame_root_message_has_body(frame: &str, expected: Option<&str>) -> bool {
    parse_frame(frame).is_some_and(|element| {
        element.name() == "message"
            && element.children().any(|child| {
                child.name() == "body"
                    && child.ns() == element.ns()
                    && expected.is_none_or(|body| child.text() == body)
            })
    })
}

fn frame_is_muc_join_self_presence(frame: &str, room: &str, nick: &str) -> bool {
    let expected_from = format!("{room}/{nick}");
    parse_frame(frame).is_some_and(|element| {
        element.name() == "presence"
            && element.attr("from") == Some(expected_from.as_str())
            && element.children().any(|child| {
                child.name() == "x"
                    && child.ns() == "http://jabber.org/protocol/muc#user"
                    && child.children().any(|grandchild| {
                        grandchild.name() == "status" && grandchild.attr("code") == Some("110")
                    })
            })
    })
}

fn find_named_element(element: &Element, name: &str, ns: &str) -> bool {
    (element.name() == name && element.ns() == ns)
        || element
            .children()
            .any(|child| find_named_element(child, name, ns))
}

fn frame_has_element(
    frame: &str,
    spec: &XmlElementSpec,
    captures: &HashMap<String, String>,
) -> bool {
    parse_frame(frame).is_some_and(|element| find_matching_element(&element, spec, captures))
}

fn find_matching_element(
    element: &Element,
    spec: &XmlElementSpec,
    captures: &HashMap<String, String>,
) -> bool {
    element_matches_spec(element, spec, captures)
        || element
            .children()
            .any(|child| find_matching_element(child, spec, captures))
}

fn element_matches_spec(
    element: &Element,
    spec: &XmlElementSpec,
    captures: &HashMap<String, String>,
) -> bool {
    if element.name() != spec.name.as_str() || element.ns() != spec.ns.as_str() {
        return false;
    }
    for (name, value) in &spec.attrs {
        if element.attr(name.as_str()) != Some(value.as_str()) {
            return false;
        }
    }
    for (name, capture) in &spec.attrs_from {
        let Some(value) = captures.get(capture) else {
            return false;
        };
        if element.attr(name.as_str()) != Some(value.as_str()) {
            return false;
        }
    }
    for name in &spec.attrs_present {
        if element.attr(name.as_str()).is_none() {
            return false;
        }
    }
    if spec
        .text
        .as_deref()
        .is_some_and(|text| element.text() != text)
    {
        return false;
    }
    spec.children.iter().all(|spec_child| {
        element
            .children()
            .any(|child| element_matches_spec(child, spec_child, captures))
    })
}

fn assert_elements_present(
    frame: &str,
    specs: &[XmlElementSpec],
    captures: &HashMap<String, String>,
    context: &str,
) -> Result<()> {
    for spec in specs {
        if !frame_has_element(frame, spec, captures) {
            return Err(anyhow!("{context} expected element {spec:?}, got: {frame}"));
        }
    }
    Ok(())
}

fn assert_elements_absent(
    frame: &str,
    specs: &[XmlElementSpec],
    captures: &HashMap<String, String>,
    context: &str,
) -> Result<()> {
    for spec in specs {
        if frame_has_element(frame, spec, captures) {
            return Err(anyhow!(
                "{context} expected element {spec:?} to be absent, got: {frame}"
            ));
        }
    }
    Ok(())
}

fn element_contains_direct_message_body(element: &Element, expected: Option<&str>) -> bool {
    let this_element_matches = element.name() == "message"
        && element.children().any(|child| {
            child.name() == "body"
                && child.ns() == element.ns()
                && expected.is_none_or(|body| child.text() == body)
        });

    this_element_matches
        || element
            .children()
            .any(|child| element_contains_direct_message_body(child, expected))
}

fn normalized_reaction_text_markers(target_id: &str, emojis: &[String]) -> Vec<String> {
    let emoji_refs = emojis.iter().map(String::as_str).collect::<Vec<_>>();
    xep0444::build_reactions_element(target_id, &emoji_refs)
        .children()
        .filter(|child| child.name() == "reaction" && child.ns() == xep0444::NS_REACTIONS)
        .map(|child| text_node_marker(&child.text()))
        .collect()
}

fn body_text_marker(body: &str) -> String {
    format!(">{body}</body>")
}

fn text_node_marker(value: &str) -> String {
    format!(">{value}</")
}

fn extract_stanza_id_from_frame(frame: &str, by: Option<&str>) -> Result<String> {
    let element =
        Element::from_str(frame).with_context(|| format!("parse message frame: {frame}"))?;
    find_stanza_id(&element, by)
        .ok_or_else(|| anyhow!("no stanza-id matched by {:?} in frame: {frame}", by))
}

fn find_stanza_id(element: &Element, by: Option<&str>) -> Option<String> {
    if element.name() == "stanza-id" && element.ns() == "urn:xmpp:sid:0" {
        let by_matches = by.is_none_or(|expected| element.attr("by") == Some(expected));
        if by_matches {
            if let Some(id) = element.attr("id").filter(|id| !id.is_empty()) {
                return Some(id.to_string());
            }
        }
    }
    element
        .children()
        .find_map(|child| find_stanza_id(child, by))
}

fn extract_attr_capture(frame: &str, capture: &AttributeCapture) -> Result<String> {
    let element = Element::from_str(frame).with_context(|| format!("parse frame: {frame}"))?;
    find_capture_element(&element, capture)
        .and_then(|element| element.attr(capture.name.as_str()))
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow!(
                "no attribute {:?} matched capture {:?}/{:?} in frame: {frame}",
                capture.name,
                capture.element,
                capture.ns
            )
        })
}

fn find_capture_element<'a>(
    element: &'a Element,
    capture: &AttributeCapture,
) -> Option<&'a Element> {
    let name_matches = capture
        .element
        .as_deref()
        .is_none_or(|name| element.name() == name);
    let ns_matches = capture.ns.as_deref().is_none_or(|ns| element.ns() == ns);
    let contains_matches = capture
        .contains
        .as_deref()
        .is_none_or(|needle| element_xml(element).is_ok_and(|xml| xml.contains(needle)));
    if name_matches && ns_matches && contains_matches {
        return Some(element);
    }
    element
        .children()
        .find_map(|child| find_capture_element(child, capture))
}

fn assert_contains_all<I, S>(frame: &str, expected: I, context: &str) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    for part in expected {
        let part = part.as_ref();
        if !frame.contains(part) {
            return Err(anyhow!("{context} expected {part:?}, got: {frame}"));
        }
    }
    Ok(())
}

fn parse_presence_show(value: &str) -> Result<PresenceShow> {
    match value {
        "away" => Ok(PresenceShow::Away),
        "chat" => Ok(PresenceShow::Chat),
        "dnd" => Ok(PresenceShow::Dnd),
        "xa" => Ok(PresenceShow::Xa),
        other => Err(anyhow!("unknown <show/> value: {other}")),
    }
}

fn assert_absent_all<I, S>(frame: &str, absent: I, context: &str) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    for part in absent {
        let part = part.as_ref();
        if frame.contains(part) {
            return Err(anyhow!(
                "{context} expected {part:?} to be absent, got: {frame}"
            ));
        }
    }
    Ok(())
}
