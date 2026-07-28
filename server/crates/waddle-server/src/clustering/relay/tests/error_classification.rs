use super::*;

#[test]
fn stale_ref_errors_trigger_relookup_and_others_do_not() {
    assert!(is_stale_ref_error::<std::convert::Infallible>(
        &RemoteSendError::ActorNotRunning
    ));
    assert!(is_stale_ref_error::<std::convert::Infallible>(
        &RemoteSendError::ActorStopped
    ));
    assert!(is_stale_ref_error::<std::convert::Infallible>(
        &RemoteSendError::BadActorType
    ));
    assert!(!is_stale_ref_error::<std::convert::Infallible>(
        &RemoteSendError::ReplyTimeout
    ));
    assert!(!is_stale_ref_error::<std::convert::Infallible>(
        &RemoteSendError::MailboxFull
    ));
}
#[test]
fn no_effect_relookup_excludes_maybe_enqueued_actor_stopped() {
    assert!(is_no_effect_stale_ref_relookup_error::<
        std::convert::Infallible,
    >(&RemoteSendError::ActorNotRunning));
    assert!(is_no_effect_stale_ref_relookup_error::<
        std::convert::Infallible,
    >(&RemoteSendError::BadActorType));
    assert!(!is_no_effect_stale_ref_relookup_error::<
        std::convert::Infallible,
    >(&RemoteSendError::ActorStopped));
    assert!(!is_no_effect_stale_ref_relookup_error::<
        std::convert::Infallible,
    >(&RemoteSendError::ReplyTimeout));
}
/// #1597: an old peer that does not know the versioned ordered-relay
/// message id fails with `UnknownMessage` — provably before any
/// handler ran. That must synthesize the typed `UnsupportedEnvelope`
/// NACK (not `ParseFailure`) so the sender rolls back the unconsumed
/// sequence and keeps the channel instead of installing a sticky
/// diversion shared with unrelated traffic.
#[test]
fn unknown_message_synthesizes_unsupported_envelope_nack() {
    let envelope = timeout_envelope();
    let reply = ordered_send_error::<std::convert::Infallible>(
        &envelope,
        RemoteSendError::UnknownMessage {
            actor_remote_id: "actor".into(),
            message_remote_id: "message".into(),
        },
    )
    .expect("UnknownMessage must synthesize a NACK, not an ask error");
    match reply {
        OrderedRelayReply::Nack(nack) => {
            assert_eq!(nack.reason, OrderedRelayNackReason::UnsupportedEnvelope);
            assert_eq!(nack.sequence, envelope.sequence);
            assert_eq!(nack.channel, envelope.channel);
        }
        OrderedRelayReply::Ack(_) => panic!("UnknownMessage must not ACK"),
    }
}
#[test]
fn unsupported_envelope_excludes_ambiguous_codec_errors() {
    assert!(is_ordered_unsupported_envelope_error::<
        std::convert::Infallible,
    >(&RemoteSendError::UnknownMessage {
        actor_remote_id: "actor".into(),
        message_remote_id: "message".into(),
    }));
    assert!(!is_ordered_unsupported_envelope_error::<
        std::convert::Infallible,
    >(&RemoteSendError::DeserializeMessage(String::new())));
    assert!(!is_ordered_unsupported_envelope_error::<
        std::convert::Infallible,
    >(&RemoteSendError::SerializeReply(String::new())));
    assert!(!is_ordered_unsupported_envelope_error::<
        std::convert::Infallible,
    >(&RemoteSendError::SerializeMessage(String::new())));
}
#[test]
fn ask_failures_classify_handler_effect_separately_from_failure_kind() {
    use std::convert::Infallible;
    use RelaySendEffect::{MaybeCommitted, NoEffect};

    for (error, expected) in [
        (RemoteSendError::ActorNotRunning, NoEffect),
        (
            RemoteSendError::UnknownActor {
                actor_remote_id: "actor".into(),
            },
            NoEffect,
        ),
        (RemoteSendError::BadActorType, NoEffect),
        (RemoteSendError::MailboxFull, NoEffect),
        (RemoteSendError::SerializeMessage(String::new()), NoEffect),
        (RemoteSendError::SwarmNotBootstrapped, NoEffect),
        (RemoteSendError::DialFailure, NoEffect),
        (RemoteSendError::UnsupportedProtocols, NoEffect),
        (RemoteSendError::ActorStopped, MaybeCommitted),
        (RemoteSendError::ReplyTimeout, MaybeCommitted),
        (
            RemoteSendError::DeserializeMessage(String::new()),
            MaybeCommitted,
        ),
        (
            RemoteSendError::SerializeReply(String::new()),
            MaybeCommitted,
        ),
        (RemoteSendError::NetworkTimeout, MaybeCommitted),
        (RemoteSendError::ConnectionClosed, MaybeCommitted),
    ] {
        assert_eq!(classify_effect::<Infallible>(&error), expected, "{error:?}");
    }
}
#[test]
fn ask_failures_classify_into_typed_kinds() {
    use std::convert::Infallible;
    for (error, expected) in [
        (RemoteSendError::ActorStopped, RelaySendFailure::StaleRef),
        (RemoteSendError::MailboxFull, RelaySendFailure::MailboxFull),
        (
            RemoteSendError::ReplyTimeout,
            RelaySendFailure::ReplyTimeout,
        ),
        (
            RemoteSendError::SerializeMessage(String::new()),
            RelaySendFailure::Codec,
        ),
        (RemoteSendError::DialFailure, RelaySendFailure::Transport),
        (RemoteSendError::NetworkTimeout, RelaySendFailure::Transport),
    ] {
        assert_eq!(classify::<Infallible>(&error), expected, "{error:?}");
    }
}
