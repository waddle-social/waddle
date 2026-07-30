//! Undeliverable-negotiation compensation and echo sanitization
//! (#1444/#1607): when a forwarded `session-initiate`/`session-accept`
//! bounces, exactly the issuance that stanza carried is revoked, and
//! the RFC 6120 §8.3.1 echo is the sender's own request minus only the
//! server-issued LiveKit transport.

use jid::FullJid;
use waddle_sfu::{Identity, SfuService};
use waddle_xmpp::protocol::event::OutboundEvent;
use waddle_xmpp::protocol::handlers::jingle::JingleHandler;
use waddle_xmpp::protocol::traits::IqHandler;
use waddle_xmpp::xep::xep0166::Action;
use waddle_xmpp::xep::xep_waddle_livekit_transport::NS_WADDLE_LIVEKIT_TRANSPORT;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;

use super::{ctx, dm_jingle_iq, fixture_livekit_sfu, NS_JINGLE_RTP};

/// Extract the forwarded IQ from a handler's `RouteToConnection` event.
fn routed_iq(events: &[OutboundEvent]) -> Iq {
    events
        .iter()
        .find_map(|event| match event {
            OutboundEvent::RouteToConnection { stanza, .. } => match stanza.as_ref() {
                Stanza::Iq(iq) => Some((**iq).clone()),
                _ => None,
            },
            _ => None,
        })
        .expect("handler forwards the negotiation IQ")
}

/// #1444/#1607: when the forwarded `session-initiate` turns out
/// undeliverable, the rollback derived from the bounced stanza itself
/// identifies EXACTLY the issuance that stanza carried. Revoking it
/// must not touch the pair's other tokens or its registration — the
/// `(call, identity)` pair may be live through an independent,
/// successful negotiation.
#[test]
fn undeliverable_initiate_rollback_targets_only_the_minted_issuance() {
    use waddle_xmpp::protocol::handlers::jingle::undeliverable_negotiation_rollback;

    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu.clone());
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("alice jid");
    let invite = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "bounce-1",
    );
    let forwarded = routed_iq(&handler.handle(&invite, &ctx(&alice)));

    let call = waddle_sfu::CallId::new("alice@waddle.test::bounce-1").expect("call id");
    let bob_identity = Identity::from_jid("bob@waddle.test/phone".parse().expect("bob jid"));
    // An independent, successful issuance for the same pair.
    let independent = sfu
        .issue_join_token(
            &call,
            &bob_identity,
            waddle_sfu::MediaCapabilities::direct_call_peer(),
        )
        .expect("independent mint");

    let rollback = undeliverable_negotiation_rollback(&forwarded).expect("rollback derivable");
    assert_eq!(rollback.call_id.as_str(), "alice@waddle.test::bounce-1");
    let minted = rollback
        .minted_jti
        .expect("the forwarded stanza carries its own minted jti");
    sfu.revoke_issued_token(&rollback.call_id, &rollback.identity, &minted);

    assert!(sfu.is_revoked(&minted), "the bounced issuance is revoked");
    assert!(
        !sfu.is_revoked(&independent.jti),
        "the independent issuance survives the bounce"
    );
    assert!(
        sfu.has_call_participant(&call, &bob_identity),
        "the bounce compensation never unregisters the participant"
    );
}

/// #1444/#1607: the RFC 6120 §8.3.1 echo for a bounced negotiation is
/// the sender's own request minus ONLY the server-issued LiveKit
/// transport — action, sid, contents, and descriptions all survive.
#[test]
fn credential_free_echo_strips_only_the_issued_transport() {
    use waddle_xmpp::protocol::handlers::jingle::credential_free_jingle_echo;

    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu.clone());
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("alice jid");
    let invite = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "bounce-2",
    );
    let forwarded = routed_iq(&handler.handle(&invite, &ctx(&alice)));
    let Iq::Set { payload, .. } = forwarded else {
        panic!("forwarded negotiation is an IQ set");
    };
    let carried_issued_transport = payload
        .children()
        .flat_map(|content| content.children())
        .any(|elem| {
            elem.is("transport", NS_WADDLE_LIVEKIT_TRANSPORT) && elem.attr("url").is_some()
        });
    assert!(
        carried_issued_transport,
        "guard: the forwarded stanza really carried an issued transport"
    );

    let echo = credential_free_jingle_echo(&payload);

    assert_eq!(echo.attr("action"), Some("session-initiate"));
    assert_eq!(echo.attr("sid"), Some("bounce-2"));
    let content = echo
        .children()
        .find(|child| child.name() == "content")
        .expect("echo keeps the sender's content");
    assert!(
        content
            .children()
            .any(|child| child.is("description", NS_JINGLE_RTP)),
        "echo keeps the sender's description"
    );
    assert!(
        !content
            .children()
            .any(|child| child.is("transport", NS_WADDLE_LIVEKIT_TRANSPORT)),
        "echo carries no LiveKit transport (and therefore no token)"
    );
}
