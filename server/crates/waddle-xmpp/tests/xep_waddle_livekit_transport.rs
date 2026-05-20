//! `urn:waddle:transports:livekit:0` — Waddle's Jingle transport for
//! LiveKit-backed calls. Dedicated test suite.
//!
//! This is a Waddle-custom XEP-0166 transport (the CLAUDE.md
//! conformance rule allows custom `urn:waddle:*` namespaces only
//! where no XEP shape fits — SFU-mediated join tokens are one such
//! case; XEP-0167 + XEP-0176 assume peer-to-peer ICE and don't model
//! a server-issued WebSocket signalling URL + JWT).
//!
//! Two wire forms exist:
//!
//! 1. **Request form** — empty element:
//!    `<transport xmlns='urn:waddle:transports:livekit:0'/>`.
//!    Sent by clients on session-initiate / session-accept to ask
//!    the server to issue a LiveKit join token.
//! 2. **Issued form** — fully-populated element carrying the
//!    LiveKit websocket URL, room name, participant identity, and
//!    JWT in a `<token/>` child. Inserted by the server's Jingle
//!    handler when it routes a session-initiate/accept onward.
//!
//! All construction goes through typed Rust values — the typed
//! [`WaddleLiveKitTransport`] sum, the typed [`IssuedTransport`]
//! record, and the typed [`WebsocketUrl`], [`CallId`], [`Identity`],
//! [`Jwt`] payload values. No `format!`, no string concatenation
//! (CLAUDE.md XML hard rule).

use minidom::Element;
use waddle_sfu::{CallId, Identity, Jti, Jwt, WebsocketUrl};
use waddle_xmpp::xep::xep_waddle_livekit_transport::{
    IssuedTransport, TransportParseError, WaddleLiveKitTransport, NS_WADDLE_LIVEKIT_TRANSPORT,
};

fn issued_fixture() -> IssuedTransport {
    let url = WebsocketUrl::new("wss://livekit.waddle.test".parse().expect("valid url"))
        .expect("valid ws url");
    let room = CallId::new("alice@waddle.test::c1").expect("valid call id");
    let jid: jid::FullJid = "bob@waddle.test/desktop".parse().expect("valid jid");
    let identity = Identity::from_jid(jid);
    let token = Jwt::from_wire("eyJhbGciOi.payload.sig".to_string());
    IssuedTransport {
        url,
        room,
        identity,
        token,
    }
}

// ── Namespace constant ────────────────────────────────────────────────

#[test]
fn waddle_livekit_transport_namespace_is_versioned_zero() {
    // `urn:waddle:*` per the CLAUDE.md XEP-conformance rule; the
    // `:0` suffix is the initial-draft version marker — future
    // revisions bump per the XSF Registrar convention.
    assert_eq!(
        NS_WADDLE_LIVEKIT_TRANSPORT,
        "urn:waddle:transports:livekit:0"
    );
}

// ── Request form (empty element) ─────────────────────────────────────

#[test]
fn waddle_livekit_transport_request_form_serialises_to_empty_element() {
    // The request form is the wire shape clients emit to ask the
    // server for a LiveKit token. It has NO attributes and NO
    // children — the server rewrites it with the issued form before
    // forwarding to the peer.
    let req = WaddleLiveKitTransport::request();
    let elem = req.to_element();
    assert_eq!(elem.name(), "transport");
    assert_eq!(elem.ns(), NS_WADDLE_LIVEKIT_TRANSPORT);
    assert!(
        elem.attrs().iter().next().is_none(),
        "request form carries no attributes"
    );
    assert!(
        elem.children().next().is_none(),
        "request form carries no children"
    );
}

#[test]
fn waddle_livekit_transport_request_form_round_trips() {
    let elem = Element::builder("transport", NS_WADDLE_LIVEKIT_TRANSPORT).build();
    let parsed = WaddleLiveKitTransport::try_from(&elem).expect("request form parses");
    assert!(matches!(parsed, WaddleLiveKitTransport::Request));

    // Re-serialise → reparse to prove the empty form is stable.
    let reser = parsed.to_element();
    let reparsed = WaddleLiveKitTransport::try_from(&reser).expect("reparses");
    assert!(matches!(reparsed, WaddleLiveKitTransport::Request));
}

// ── Issued form (populated element + <token/> child) ─────────────────

#[test]
fn waddle_livekit_transport_issued_form_carries_all_four_fields() {
    // The issued form carries: `url`, `room`, `identity` attributes
    // plus a `<token/>` child whose text is the LiveKit JWT.
    let issued = WaddleLiveKitTransport::Issued(issued_fixture());
    let elem = issued.to_element();
    assert_eq!(elem.name(), "transport");
    assert_eq!(elem.ns(), NS_WADDLE_LIVEKIT_TRANSPORT);

    assert_eq!(elem.attr("url"), Some("wss://livekit.waddle.test/"));
    assert_eq!(elem.attr("room"), Some("alice@waddle.test::c1"));
    assert_eq!(elem.attr("identity"), Some("bob@waddle.test/desktop"));

    let token = elem
        .children()
        .find(|c| c.name() == "token" && c.ns() == NS_WADDLE_LIVEKIT_TRANSPORT)
        .expect("issued form carries a <token/> child");
    assert_eq!(token.text(), "eyJhbGciOi.payload.sig");
}

#[test]
fn waddle_livekit_transport_issued_form_round_trips_through_typed_surface() {
    // Build via typed `IssuedTransport`, serialise, reparse, and
    // verify every typed field survives the wire encoding.
    let fixture = issued_fixture();
    let issued = WaddleLiveKitTransport::Issued(fixture.clone());
    let elem = issued.to_element();

    let parsed = WaddleLiveKitTransport::try_from(&elem).expect("issued form parses");
    match parsed {
        WaddleLiveKitTransport::Issued(t) => {
            assert_eq!(t.url.as_str(), fixture.url.as_str());
            assert_eq!(t.room, fixture.room);
            assert_eq!(t.identity, fixture.identity);
            assert_eq!(t.token.as_str(), fixture.token.as_str());
        }
        WaddleLiveKitTransport::Request => {
            panic!("populated transport must reparse as Issued, not Request")
        }
    }
}

// ── Negative cases — typed parse errors ──────────────────────────────

#[test]
fn waddle_livekit_transport_rejects_wrong_namespace() {
    // A `<transport/>` element in a non-Waddle namespace MUST NOT
    // parse as `WaddleLiveKitTransport`. The server's Jingle
    // handler uses this discriminator to reject foreign transports
    // (e.g. ICE-UDP) with `<unsupported-transports/>`.
    let elem = Element::builder("transport", "urn:xmpp:jingle:transports:ice-udp:1").build();
    let err = WaddleLiveKitTransport::try_from(&elem).expect_err("wrong ns");
    assert!(matches!(err, TransportParseError::WrongElement));
}

#[test]
fn waddle_livekit_transport_rejects_wrong_element_name() {
    let elem = Element::builder("candidate", NS_WADDLE_LIVEKIT_TRANSPORT).build();
    let err = WaddleLiveKitTransport::try_from(&elem).expect_err("wrong name");
    assert!(matches!(err, TransportParseError::WrongElement));
}

#[test]
fn waddle_livekit_transport_rejects_missing_url_attribute() {
    // The issued form MUST carry url + room + identity + <token/>.
    // Partial population is an unambiguous wire-format error
    // (typed parse fails) — the server's Jingle handler hides the
    // inner message and replies with a generic <bad-request/>.
    let elem = Element::builder("transport", NS_WADDLE_LIVEKIT_TRANSPORT)
        .attr(
            minidom::rxml::xml_ncname!("room").to_owned(),
            "alice@waddle.test::c1",
        )
        .attr(
            minidom::rxml::xml_ncname!("identity").to_owned(),
            "bob@waddle.test/desktop",
        )
        .append(
            Element::builder("token", NS_WADDLE_LIVEKIT_TRANSPORT)
                .append("jwt")
                .build(),
        )
        .build();
    let err = WaddleLiveKitTransport::try_from(&elem).expect_err("missing url");
    assert!(
        matches!(err, TransportParseError::MissingAttribute("url")),
        "missing-url error names the missing attribute, got {err:?}"
    );
}

#[test]
fn waddle_livekit_transport_rejects_missing_room_attribute() {
    let elem = Element::builder("transport", NS_WADDLE_LIVEKIT_TRANSPORT)
        .attr(
            minidom::rxml::xml_ncname!("url").to_owned(),
            "wss://livekit.waddle.test/",
        )
        .attr(
            minidom::rxml::xml_ncname!("identity").to_owned(),
            "bob@waddle.test/desktop",
        )
        .append(
            Element::builder("token", NS_WADDLE_LIVEKIT_TRANSPORT)
                .append("jwt")
                .build(),
        )
        .build();
    let err = WaddleLiveKitTransport::try_from(&elem).expect_err("missing room");
    assert!(matches!(err, TransportParseError::MissingAttribute("room")));
}

#[test]
fn waddle_livekit_transport_rejects_missing_identity_attribute() {
    let elem = Element::builder("transport", NS_WADDLE_LIVEKIT_TRANSPORT)
        .attr(
            minidom::rxml::xml_ncname!("url").to_owned(),
            "wss://livekit.waddle.test/",
        )
        .attr(
            minidom::rxml::xml_ncname!("room").to_owned(),
            "alice@waddle.test::c1",
        )
        .append(
            Element::builder("token", NS_WADDLE_LIVEKIT_TRANSPORT)
                .append("jwt")
                .build(),
        )
        .build();
    let err = WaddleLiveKitTransport::try_from(&elem).expect_err("missing identity");
    assert!(matches!(
        err,
        TransportParseError::MissingAttribute("identity")
    ));
}

#[test]
fn waddle_livekit_transport_rejects_missing_token_child() {
    let elem = Element::builder("transport", NS_WADDLE_LIVEKIT_TRANSPORT)
        .attr(
            minidom::rxml::xml_ncname!("url").to_owned(),
            "wss://livekit.waddle.test/",
        )
        .attr(
            minidom::rxml::xml_ncname!("room").to_owned(),
            "alice@waddle.test::c1",
        )
        .attr(
            minidom::rxml::xml_ncname!("identity").to_owned(),
            "bob@waddle.test/desktop",
        )
        .build();
    let err = WaddleLiveKitTransport::try_from(&elem).expect_err("missing token");
    assert!(matches!(err, TransportParseError::MissingToken));
}

#[test]
fn waddle_livekit_transport_rejects_invalid_url_scheme() {
    // The `WebsocketUrl::new` constructor restricts schemes to
    // `ws://` and `wss://`. An `http://` URL is rejected at the
    // typed-construction boundary.
    let elem = Element::builder("transport", NS_WADDLE_LIVEKIT_TRANSPORT)
        .attr(
            minidom::rxml::xml_ncname!("url").to_owned(),
            "http://livekit.waddle.test/",
        )
        .attr(
            minidom::rxml::xml_ncname!("room").to_owned(),
            "alice@waddle.test::c1",
        )
        .attr(
            minidom::rxml::xml_ncname!("identity").to_owned(),
            "bob@waddle.test/desktop",
        )
        .append(
            Element::builder("token", NS_WADDLE_LIVEKIT_TRANSPORT)
                .append("jwt")
                .build(),
        )
        .build();
    let err = WaddleLiveKitTransport::try_from(&elem).expect_err("invalid scheme");
    assert!(
        matches!(err, TransportParseError::InvalidUrl(_)),
        "http:// scheme must be rejected by the typed WebsocketUrl: {err:?}"
    );
}

#[test]
fn waddle_livekit_transport_rejects_invalid_identity_jid() {
    // Identity is constructed from a `FullJid`. A bare JID has no
    // resource and MUST be rejected — the LiveKit identity needs
    // a specific session-resource.
    let elem = Element::builder("transport", NS_WADDLE_LIVEKIT_TRANSPORT)
        .attr(
            minidom::rxml::xml_ncname!("url").to_owned(),
            "wss://livekit.waddle.test/",
        )
        .attr(
            minidom::rxml::xml_ncname!("room").to_owned(),
            "alice@waddle.test::c1",
        )
        .attr(
            minidom::rxml::xml_ncname!("identity").to_owned(),
            "bob@waddle.test",
        )
        .append(
            Element::builder("token", NS_WADDLE_LIVEKIT_TRANSPORT)
                .append("jwt")
                .build(),
        )
        .build();
    let err = WaddleLiveKitTransport::try_from(&elem).expect_err("bare identity");
    assert!(
        matches!(err, TransportParseError::InvalidIdentity(_)),
        "bare JID identity must be rejected, got {err:?}"
    );
}

// ── Builder helpers via from_join_token ──────────────────────────────

#[test]
fn waddle_livekit_transport_from_join_token_produces_issued_form() {
    // The server constructs the issued transport from a
    // `JoinToken` returned by the SFU service. The conversion
    // copies the typed fields verbatim — no string round-trip.
    let join = waddle_sfu::JoinToken {
        url: WebsocketUrl::new("wss://livekit.waddle.test".parse().expect("valid url"))
            .expect("valid ws url"),
        room: CallId::new("alice@waddle.test::c1").expect("valid call id"),
        identity: Identity::from_jid("bob@waddle.test/desktop".parse().expect("valid jid")),
        jwt: Jwt::from_wire("a.b.c".to_string()),
        jti: Jti::new(),
        expires_at: chrono::Utc::now() + chrono::Duration::seconds(3600),
    };
    let xport = WaddleLiveKitTransport::from_join_token(join);
    let elem = xport.to_element();
    let parsed = WaddleLiveKitTransport::try_from(&elem).expect("parses");
    match parsed {
        WaddleLiveKitTransport::Issued(t) => {
            assert_eq!(t.url.as_str(), "wss://livekit.waddle.test/");
            assert_eq!(t.room.as_str(), "alice@waddle.test::c1");
            assert_eq!(t.identity.as_livekit_identity(), "bob@waddle.test/desktop");
            assert_eq!(t.token.as_str(), "a.b.c");
        }
        WaddleLiveKitTransport::Request => panic!("from_join_token must produce Issued"),
    }
}

#[test]
fn waddle_livekit_transport_token_child_is_in_same_namespace_as_parent() {
    // Defensive: the `<token/>` child MUST be qualified by the
    // same `urn:waddle:transports:livekit:0` namespace as its
    // parent so a hand-built element using the default namespace
    // (which would put `<token/>` in `jabber:client`) parses
    // correctly.
    let issued = WaddleLiveKitTransport::Issued(issued_fixture());
    let elem = issued.to_element();
    let token = elem
        .children()
        .find(|c| c.name() == "token")
        .expect("token present");
    assert_eq!(
        token.ns(),
        NS_WADDLE_LIVEKIT_TRANSPORT,
        "<token/> MUST inherit the transport namespace, not default"
    );
}
