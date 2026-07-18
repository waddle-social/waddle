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

fn iq_result_stanza(id: &str) -> Stanza {
    // A `result` IQ is a response, not a request — it owes no reply even if it
    // somehow reaches dispatch and times out.
    let iq = Iq::Result {
        id: id.to_string(),
        from: Some("upload.example.com".parse().expect("from jid")),
        to: Some("alice@example.com/web".parse().expect("to jid")),
        payload: None,
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

fn responses_and_disposition(
    result: Result<Vec<String>, StanzaTimeout>,
) -> (Vec<String>, InboundDisposition) {
    match result {
        Ok(responses) => (responses, InboundDisposition::Handled),
        Err(StanzaTimeout::HandledIq(reply)) => (
            vec![crate::server::routes::websocket::element_to_xml(reply)],
            InboundDisposition::Handled,
        ),
        Err(StanzaTimeout::Unhandled) => (Vec::new(), InboundDisposition::Unhandled),
    }
}

#[tokio::test(start_paused = true)]
async fn iq_get_timeout_yields_conformant_resource_constraint() {
    let stanza = iq_get_stanza("disco-1", "alice@example.com/web", "upload.example.com");
    let backstop = StanzaBackstop::capture(&stanza, None);

    let mut fut = Box::pin(run_with_backstop(backstop, pending::<Vec<String>>()));
    assert!(
        futures::poll!(fut.as_mut()).is_pending(),
        "must not resolve before the wedge budget elapses"
    );
    tokio::time::advance(STANZA_HANDLER_WEDGE_TIMEOUT + Duration::from_millis(1)).await;
    let (responses, disposition) = responses_and_disposition(fut.await);

    assert_eq!(
        responses.len(),
        1,
        "a timed-out IQ get owes exactly one reply"
    );
    assert_eq!(disposition, InboundDisposition::Handled);
    let mut sm_state = waddle_xmpp::stream_management::StreamManagementState::new();
    sm_state.enable("iq-timeout".to_string(), true, Some(300));
    let mut completion = crate::server::routes::interpret::SmInboundCompletionTracker::default();
    let sequence = completion.reserve(&sm_state);
    crate::server::routes::websocket::frame::settle_inbound_dispatch(
        disposition,
        false,
        Some(sequence),
        &mut completion,
        &mut sm_state,
    );
    assert_eq!(
        sm_state.get_inbound_count(),
        1,
        "the retryable IQ error accepts responsibility for the request"
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
    let backstop = StanzaBackstop::capture(&stanza, None);

    let fut = run_with_backstop(backstop, pending::<Vec<String>>());
    tokio::time::advance(STANZA_HANDLER_WEDGE_TIMEOUT + Duration::from_millis(1)).await;
    let (responses, disposition) = responses_and_disposition(fut.await);

    assert!(
        responses.is_empty(),
        "a timed-out message owes no reply: {:?}",
        responses
    );
    assert_eq!(disposition, InboundDisposition::Unhandled);
}

#[tokio::test(start_paused = true)]
async fn presence_timeout_yields_no_response() {
    let stanza = presence_stanza();
    let backstop = StanzaBackstop::capture(&stanza, None);

    let fut = run_with_backstop(backstop, pending::<Vec<String>>());
    tokio::time::advance(STANZA_HANDLER_WEDGE_TIMEOUT + Duration::from_millis(1)).await;
    let (responses, disposition) = responses_and_disposition(fut.await);

    assert!(responses.is_empty(), "a timed-out presence owes no reply");
    assert_eq!(disposition, InboundDisposition::Unhandled);
}

#[tokio::test(start_paused = true)]
async fn iq_result_timeout_yields_no_response() {
    // RFC 6120 §8.2.3: only `get`/`set` owe a response. A timed-out `result`
    // IQ must NOT synthesize an error reply (locks the `_ => None` arm in
    // `capture`).
    let stanza = iq_result_stanza("ack-1");
    let backstop = StanzaBackstop::capture(&stanza, None);

    let fut = run_with_backstop(backstop, pending::<Vec<String>>());
    tokio::time::advance(STANZA_HANDLER_WEDGE_TIMEOUT + Duration::from_millis(1)).await;
    let (responses, disposition) = responses_and_disposition(fut.await);

    assert!(
        responses.is_empty(),
        "a timed-out IQ result owes no reply: {:?}",
        responses
    );
    assert_eq!(disposition, InboundDisposition::Unhandled);
}

#[tokio::test(start_paused = true)]
async fn fast_dispatch_passes_through_untouched() {
    let stanza = iq_get_stanza("disco-2", "alice@example.com/web", "example.com");
    let backstop = StanzaBackstop::capture(&stanza, None);

    // A handler that completes immediately must pass its response through
    // unchanged, with no timeout reply.
    let result = run_with_backstop(backstop, async {
        vec!["<iq id=\"disco-2\" type=\"result\"/>".to_string()]
    })
    .await;
    let (responses, disposition) = responses_and_disposition(result);

    assert_eq!(
        responses,
        vec!["<iq id=\"disco-2\" type=\"result\"/>".to_string()]
    );
    assert_eq!(disposition, InboundDisposition::Handled);
}
