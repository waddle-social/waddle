use super::*;
use std::future::pending;
use std::time::Duration;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::presence::Presence;

fn iq_get_stanza(id: &str, from: &str, to: &str) -> Stanza {
    let payload = Element::builder("query", "http://jabber.org/protocol/disco#info").build();
    let iq = Iq::Get {
        id: id.to_string(),
        from: Some(from.parse().expect("from jid")),
        to: Some(to.parse().expect("to jid")),
        payload,
    };
    Stanza::Iq(Box::new(iq))
}

fn message_stanza() -> Stanza {
    let to: xmpp_parsers::jid::Jid = "bob@example.com".parse().expect("to jid");
    Stanza::Message(Message::new(Some(to)))
}

fn presence_stanza() -> Stanza {
    Stanza::Presence(Presence::new(xmpp_parsers::presence::Type::None))
}

#[tokio::test(start_paused = true)]
async fn iq_get_timeout_yields_conformant_resource_constraint() {
    let stanza = iq_get_stanza("disco-1", "alice@example.com/web", "upload.example.com");
    let backstop = StanzaBackstop::capture(&stanza);

    let mut fut = Box::pin(run_with_backstop(backstop, pending::<Vec<String>>()));
    assert!(
        futures::poll!(fut.as_mut()).is_pending(),
        "must not resolve before the wedge budget elapses"
    );
    tokio::time::advance(STANZA_HANDLER_WEDGE_TIMEOUT + Duration::from_millis(1)).await;
    let responses = fut.await;

    assert_eq!(
        responses.len(),
        1,
        "a timed-out IQ get owes exactly one reply"
    );
    let reply = &responses[0];
    // minidom serializes attributes with single quotes (house style; see the
    // existing iq.rs assertions).
    assert!(
        reply.contains("type='error'"),
        "must be an IQ error: {reply}"
    );
    assert!(
        reply.contains("resource-constraint"),
        "must carry resource-constraint: {reply}"
    );
    assert!(
        reply.contains("type='wait'"),
        "resource-constraint must be a retryable wait error: {reply}"
    );
    assert!(
        reply.contains("id='disco-1'"),
        "must echo the request id: {reply}"
    );
    // RFC 6120 §8.2.3: from/to are swapped on the response.
    assert!(
        reply.contains("from='upload.example.com'"),
        "response from = request to: {reply}"
    );
    assert!(
        reply.contains("to='alice@example.com/web'"),
        "response to = request from: {reply}"
    );
}

#[tokio::test(start_paused = true)]
async fn message_timeout_yields_no_response() {
    let stanza = message_stanza();
    let backstop = StanzaBackstop::capture(&stanza);

    let fut = run_with_backstop(backstop, pending::<Vec<String>>());
    tokio::time::advance(STANZA_HANDLER_WEDGE_TIMEOUT + Duration::from_millis(1)).await;
    let responses = fut.await;

    assert!(
        responses.is_empty(),
        "a timed-out message owes no reply: {responses:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn presence_timeout_yields_no_response() {
    let stanza = presence_stanza();
    let backstop = StanzaBackstop::capture(&stanza);

    let fut = run_with_backstop(backstop, pending::<Vec<String>>());
    tokio::time::advance(STANZA_HANDLER_WEDGE_TIMEOUT + Duration::from_millis(1)).await;
    let responses = fut.await;

    assert!(responses.is_empty(), "a timed-out presence owes no reply");
}

#[tokio::test(start_paused = true)]
async fn fast_dispatch_passes_through_untouched() {
    let stanza = iq_get_stanza("disco-2", "alice@example.com/web", "example.com");
    let backstop = StanzaBackstop::capture(&stanza);

    // A handler that completes immediately must pass its response through
    // unchanged, with no timeout reply.
    let responses = run_with_backstop(backstop, async {
        vec!["<iq id=\"disco-2\" type=\"result\"/>".to_string()]
    })
    .await;

    assert_eq!(
        responses,
        vec!["<iq id=\"disco-2\" type=\"result\"/>".to_string()]
    );
}
