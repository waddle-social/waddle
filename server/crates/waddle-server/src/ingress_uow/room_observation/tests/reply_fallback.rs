use sha2::{Digest, Sha256};
use waddle_xmpp::xep::xep0428::{build_fallback_element, FallbackIndication};
use waddle_xmpp::xep::xep0461::{build_reply_element, ReplyReference, NS_REPLY};

use super::*;

fn with_reply(mut message: Message, quote_len: usize) -> Message {
    message.payloads.push(build_reply_element(
        &ReplyReference::new("quoted-parent")
            .with_to("quoted-author@example.org".parse().expect("JID")),
    ));
    message
        .payloads
        .push(build_fallback_element(&FallbackIndication::for_range(
            NS_REPLY, 0, quote_len,
        )));
    message
}

fn digest(body: &str) -> Sha256Digest {
    Sha256Digest::new(hex::encode(Sha256::digest(body.as_bytes()))).expect("digest")
}

async fn reply_inputs_preserve_raw_identity_and_correction_fences(fixture: IngressFixture) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let subscription = subscription(&observer);
    let root_key = MessageKey::new();
    let now = Utc::now();
    let quote = "> hostile quoted text 🙂\n\n";
    let raw_body = [quote, "a harmless question?"].concat();
    let root = with_reply(
        message("wire-root", "room-root", Some("origin-root"), &raw_body),
        quote.chars().count(),
    );
    let mut tx = fixture.uow.begin().await.expect("source");
    record_message(&mut tx, root_key).await;
    Repo::sync_configured(&mut tx, std::slice::from_ref(&observer))
        .await
        .expect("sync");
    capture(
        &mut tx,
        root_key,
        &room(),
        &root,
        &sender(),
        &[intent(&observer)],
        now,
    )
    .await
    .expect("capture");
    tx.commit().await.expect("source committed");

    let mut tx = fixture.uow.begin().await.expect("claim source");
    let first = Repo::claim(&mut tx, &subscription, now.timestamp_millis())
        .await
        .expect("claim")
        .expect("source work");
    assert_eq!(first.body.as_str(), "a harmless question?");
    assert_eq!(first.source.body_digest, digest(&raw_body));
    assert_eq!(first.source.stanza_id.as_str(), "room-root");
    assert_eq!(first.source.revision_stanza_id.as_str(), "room-root");
    assert_eq!(
        first.source.origin_id.as_ref().expect("origin").as_str(),
        "origin-root"
    );
    assert_eq!(
        root.get_best_body(vec![])
            .expect("unmodified source")
            .1
            .as_str(),
        raw_body.as_str()
    );
    tx.commit().await.expect("lease source");

    let corrected_quote = "> a different quote e\u{301}\n\n";
    let corrected_raw = [corrected_quote, "updated authored answer"].concat();
    let mut correction = with_reply(
        message(
            "wire-edit",
            "room-edit",
            Some("origin-edit"),
            &corrected_raw,
        ),
        corrected_quote.chars().count(),
    );
    correction
        .payloads
        .push(waddle_xmpp::xep::xep0308::build_replace_element(
            "wire-root",
        ));
    let edit_key = MessageKey::new();
    let mut tx = fixture.uow.begin().await.expect("correction");
    record_message(&mut tx, edit_key).await;
    capture(
        &mut tx,
        edit_key,
        &room(),
        &correction,
        &sender(),
        &[correction_intent(&observer, "room-root")],
        now,
    )
    .await
    .expect("capture correction");
    let second = Repo::claim(&mut tx, &subscription, now.timestamp_millis())
        .await
        .expect("claim correction")
        .expect("correction work");
    assert_eq!(second.body.as_str(), "updated authored answer");
    assert_eq!(second.source.body_digest, digest(&corrected_raw));
    assert_eq!(second.source.stanza_id, first.source.stanza_id);
    assert_eq!(second.source.origin_id, first.source.origin_id);
    assert_eq!(second.source.revision_stanza_id.as_str(), "room-edit");
    assert_eq!(second.source.revision.get(), 1);
    let result = RoomObservationOutcome::Completed(RoomObservationResult {
        payloads: vec![payload()],
        usage: None,
    });
    assert!(
        !Repo::finish(&mut tx, &first, &result, now.timestamp_millis())
            .await
            .expect("old revision rejected")
    );
    tx.commit().await.expect("correction committed");

    // A fully marked quote leaves no authored text, but it still advances
    // the source revision and prevents the preceding result from publishing.
    let only_quote = "> quote without an authored answer 🙂";
    let mut empty_edit = with_reply(
        message("wire-empty", "room-empty", Some("origin-empty"), only_quote),
        only_quote.chars().count(),
    );
    empty_edit
        .payloads
        .push(waddle_xmpp::xep::xep0308::build_replace_element(
            "wire-edit",
        ));
    let empty_key = MessageKey::new();
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("empty authored correction");
    record_message(&mut tx, empty_key).await;
    capture(
        &mut tx,
        empty_key,
        &room(),
        &empty_edit,
        &sender(),
        &[correction_intent(&observer, "room-edit")],
        now,
    )
    .await
    .expect("capture empty authored correction");
    assert!(Repo::claim(&mut tx, &subscription, now.timestamp_millis())
        .await
        .expect("claim")
        .is_none());
    assert!(
        !Repo::finish(&mut tx, &second, &result, now.timestamp_millis())
            .await
            .expect("superseded correction rejected")
    );
    let stored = super::super::sources::load_source(&mut tx, root_key)
        .await
        .expect("source")
        .expect("tracked source");
    assert_eq!(stored.source.revision.get(), 2);
    assert_eq!(stored.source.revision_stanza_id.as_str(), "room-empty");
    assert_eq!(stored.source.body_digest, digest(only_quote));
    assert_eq!(
        EffectReceiptRepository::keys(&mut tx, empty_key)
            .await
            .expect("empty receipt")
            .len(),
        1
    );
    tx.commit().await.expect("empty correction committed");
    assert_eq!(fixture.count("extension_room_publications").await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn reply_inputs_preserve_raw_identity_and_correction_fences_sqlite() {
    reply_inputs_preserve_raw_identity_and_correction_fences(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn reply_inputs_preserve_raw_identity_and_correction_fences_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observation_reply_input").await {
        reply_inputs_preserve_raw_identity_and_correction_fences(fixture).await;
    }
}

async fn invalid_reply_range_still_schedules_the_full_body(fixture: IngressFixture) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let key = MessageKey::new();
    let body = "> quote\n\nvisible authored text";
    let root = with_reply(message("wire", "room-stanza", Some("origin"), body), 999);
    let now = Utc::now();
    let mut tx = fixture.uow.begin().await.expect("capture");
    record_message(&mut tx, key).await;
    Repo::sync_configured(&mut tx, std::slice::from_ref(&observer))
        .await
        .expect("sync");
    capture(
        &mut tx,
        key,
        &room(),
        &root,
        &sender(),
        &[intent(&observer)],
        now,
    )
    .await
    .expect("capture malformed fallback");
    tx.commit().await.expect("commit");
    let mut tx = fixture.uow.begin().await.expect("claim");
    let work = Repo::claim(&mut tx, &subscription(&observer), now.timestamp_millis())
        .await
        .expect("claim")
        .expect("work is not bypassed");
    assert_eq!(work.body.as_str(), body);
    assert_eq!(work.source.body_digest, digest(body));
    tx.commit().await.expect("lease");
    fixture.close().await;
}

#[tokio::test]
async fn invalid_reply_range_still_schedules_the_full_body_sqlite() {
    invalid_reply_range_still_schedules_the_full_body(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn invalid_reply_range_still_schedules_the_full_body_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observation_reply_invalid").await {
        invalid_reply_range_still_schedules_the_full_body(fixture).await;
    }
}
