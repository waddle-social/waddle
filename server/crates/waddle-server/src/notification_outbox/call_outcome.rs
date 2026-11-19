//! Bounded, fail-closed MAM evidence for unanswered-call notification decisions.

use chrono::{DateTime, Utc};
use jid::BareJid;
use minidom::Element;
use waddle_xmpp::mam::{MamArchiveKind, MamQuery, MamStorage, MamStorageError};
use waddle_xmpp::xep::xep0353::{jmi_call_outcome, jmi_retract};
use xmpp_parsers::jingle::SessionId;
use xmpp_parsers::message::Message;

#[derive(Debug, Clone, Copy)]
pub(crate) enum OutcomeAuthor {
    /// The callee answered, declined or ended the call.
    Callee,
    /// As `Callee`, plus the caller's signals other than a plain retract. A
    /// plain retract is a missed call: the retract path has alerted for it, or
    /// lost the race with the invitation and left the alert to the timer.
    CalleeOrCallerGlare,
}

/// The callee's personal archive contains both its outgoing responses and the
/// caller's incoming signals. Incomplete evidence must never trigger an alert.
pub(crate) async fn call_outcome_recorded(
    mam: &dyn MamStorage,
    callee: &BareJid,
    caller: &BareJid,
    sid: &SessionId,
    window: std::ops::Range<DateTime<Utc>>,
    author: OutcomeAuthor,
) -> Result<bool, MamStorageError> {
    let page = mam
        .query_messages(
            callee,
            MamArchiveKind::Personal,
            &MamQuery {
                start: Some(window.start),
                end: Some(window.end),
                with: Some(caller.clone().into()),
                max: Some(200),
                ..MamQuery::default()
            },
        )
        .await?;
    if !page.complete {
        return Ok(true);
    }
    for archived in page.messages {
        // Rows without the original stanza (displayed markers, moderation
        // tombstones) were never call signals.
        let Some(xml) = archived.stanza_xml else {
            continue;
        };
        let element = xml
            .parse::<Element>()
            .map_err(|error| MamStorageError::Serialization(error.to_string()))?;
        let message = Message::try_from(element)
            .map_err(|error| MamStorageError::Serialization(error.to_string()))?;
        if jmi_call_outcome(&message).as_ref() != Some(sid) {
            continue;
        }
        let from = message
            .from
            .as_ref()
            .map_or_else(|| archived.from.to_bare(), |from| from.to_bare());
        let counts = from == *callee
            || (matches!(author, OutcomeAuthor::CalleeOrCallerGlare)
                && from == *caller
                && jmi_retract(&message).is_none());
        if counts {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use jid::Jid;
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
    use waddle_xmpp::xep::xep0353::NS_JINGLE_MESSAGE;

    fn callee() -> BareJid {
        "bob@example.test".parse().unwrap()
    }

    fn caller() -> BareJid {
        "alice@example.test".parse().unwrap()
    }

    fn sid() -> SessionId {
        SessionId("session".into())
    }

    async fn store_signal(
        mam: &InMemoryMamStorage,
        archive: &BareJid,
        from: Jid,
        to: Jid,
        signal: &str,
        session: &SessionId,
        timestamp: DateTime<Utc>,
    ) {
        let mut message = Message::new(Some(to.clone()));
        message.from = Some(from.clone());
        message.payloads.push(
            Element::builder(signal, NS_JINGLE_MESSAGE)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), &session.0)
                .append(Element::builder("tie-break", NS_JINGLE_MESSAGE).build())
                .build(),
        );
        let element = Element::from(message);
        let mut bytes = Vec::new();
        element.write_to(&mut bytes).unwrap();
        mam.store_message(
            archive,
            &ArchivedMessage {
                timestamp,
                stanza_xml: Some(String::from_utf8(bytes).unwrap()),
                ..ArchivedMessage::for_test(from, to)
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn caller_retract_counts_only_for_either_author() {
        let mam = InMemoryMamStorage::new();
        let since = Utc::now() - Duration::seconds(1);
        store_signal(
            &mam,
            &callee(),
            "alice@example.test/phone".parse().unwrap(),
            callee().into(),
            "retract",
            &sid(),
            Utc::now(),
        )
        .await;
        assert!(!call_outcome_recorded(
            &mam,
            &callee(),
            &caller(),
            &sid(),
            since..(Utc::now() + Duration::seconds(60)),
            OutcomeAuthor::Callee,
        )
        .await
        .unwrap());
        assert!(call_outcome_recorded(
            &mam,
            &callee(),
            &caller(),
            &sid(),
            since..(Utc::now() + Duration::seconds(60)),
            OutcomeAuthor::CalleeOrCallerGlare,
        )
        .await
        .unwrap());
    }

    #[tokio::test]
    async fn callee_sent_copy_from_any_device_counts_for_both_author_filters() {
        for signal in ["proceed", "accept", "reject", "finish", "retract"] {
            let mam = InMemoryMamStorage::new();
            let since = Utc::now() - Duration::seconds(1);
            store_signal(
                &mam,
                &callee(),
                "bob@example.test/second-device".parse().unwrap(),
                "alice@example.test/caller-device".parse().unwrap(),
                signal,
                &sid(),
                Utc::now(),
            )
            .await;
            for author in [OutcomeAuthor::Callee, OutcomeAuthor::CalleeOrCallerGlare] {
                assert!(
                    call_outcome_recorded(
                        &mam,
                        &callee(),
                        &caller(),
                        &sid(),
                        since..(Utc::now() + Duration::seconds(60)),
                        author
                    )
                    .await
                    .unwrap(),
                    "{signal}: {author:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn query_is_scoped_to_callee_caller_time_and_session() {
        let mam = InMemoryMamStorage::new();
        let since = Utc::now();
        for (archive, from, session, timestamp) in [
            (
                callee(),
                "bob@example.test/phone",
                sid(),
                since - Duration::seconds(1),
            ),
            (caller(), "bob@example.test/phone", sid(), since),
            (
                callee(),
                "bob@example.test/phone",
                SessionId("other".into()),
                since,
            ),
            (callee(), "charlie@example.test/phone", sid(), since),
        ] {
            store_signal(
                &mam,
                &archive,
                from.parse().unwrap(),
                if from.starts_with("bob") {
                    caller().into()
                } else {
                    callee().into()
                },
                "reject",
                &session,
                timestamp,
            )
            .await;
        }
        assert!(!call_outcome_recorded(
            &mam,
            &callee(),
            &caller(),
            &sid(),
            since..(Utc::now() + Duration::seconds(60)),
            OutcomeAuthor::CalleeOrCallerGlare,
        )
        .await
        .unwrap());
    }

    #[tokio::test]
    async fn incomplete_page_suppresses_even_without_an_outcome() {
        let mam = InMemoryMamStorage::new();
        let since = Utc::now();
        for _ in 0..201 {
            store_signal(
                &mam,
                &callee(),
                caller().into(),
                callee().into(),
                "ringing",
                &sid(),
                since,
            )
            .await;
        }
        assert!(call_outcome_recorded(
            &mam,
            &callee(),
            &caller(),
            &sid(),
            since..(Utc::now() + Duration::seconds(60)),
            OutcomeAuthor::CalleeOrCallerGlare,
        )
        .await
        .unwrap());
    }

    #[tokio::test]
    async fn unreadable_stanza_is_an_error_and_a_row_without_stanza_is_ignored() {
        for (stanza_xml, readable) in [(Some("not xml".to_owned()), false), (None, true)] {
            let mam = InMemoryMamStorage::new();
            let since = Utc::now();
            mam.store_message(
                &callee(),
                &ArchivedMessage {
                    timestamp: since,
                    stanza_xml,
                    ..ArchivedMessage::for_test(caller().into(), callee().into())
                },
            )
            .await
            .unwrap();
            let recorded = call_outcome_recorded(
                &mam,
                &callee(),
                &caller(),
                &sid(),
                since..(Utc::now() + Duration::seconds(60)),
                OutcomeAuthor::CalleeOrCallerGlare,
            )
            .await;
            if readable {
                assert!(!recorded.unwrap());
            } else {
                assert!(recorded.is_err());
            }
        }
    }
}
