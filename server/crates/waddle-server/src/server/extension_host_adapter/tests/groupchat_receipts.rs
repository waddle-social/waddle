use super::groupchat_ingress::GroupchatFixture;
use crate::{
    ingress::test_support::IngressFixture,
    ingress_uow::{CanonicalMessageRepository, EffectIntentRepository, EffectReceiptRepository},
};
use waddle_xmpp::{
    ingress::{IngressEffectIntent, MessageKey},
    Stanza,
};

pub(super) async fn key(f: &IngressFixture) -> MessageKey {
    MessageKey::from_storage(
        f.optional_text(
            "SELECT CAST(message_key AS TEXT) FROM ingress_messages ORDER BY created_at LIMIT 1",
        )
        .await
        .expect("canonical ingress key")
        .parse()
        .expect("message uuid"),
    )
}

pub(super) async fn intents(f: &IngressFixture) -> Vec<IngressEffectIntent> {
    let mut tx = f.uow.begin().await.expect("inspect transaction");
    let intents = EffectIntentRepository::load(&mut tx, key(f).await)
        .await
        .expect("recorded intents");
    tx.commit().await.expect("inspection commit");
    intents
}

async fn wire_and_receipts(f: IngressFixture) {
    let mut fixture = GroupchatFixture::new(&f).await;
    let mut request = fixture.request("groupchat-wire");
    request.thread_id = Some(waddle_extensions::ThreadId::new("root-message").expect("thread"));
    request.reply_to = Some(waddle_extensions::ReplyTarget {
        id: waddle_extensions::StanzaId::new("root-message").expect("reply"),
        to: Some(
            waddle_extensions::FullJidValue::new(format!("{}/romeo", fixture.room))
                .expect("occupant reply"),
        ),
    });
    request.markup = vec![waddle_extensions::MessageMarkupSpan {
        kind: waddle_extensions::MessageMarkupKind::Blockquote,
        start: 0,
        end: 9,
    }];
    let returned_id = fixture
        .adapter
        .send_message(&fixture.invocation(), request)
        .await
        .expect("groupchat send");
    let wire = fixture.drain();
    let presence = wire
        .iter()
        .find_map(|s| match s {
            Stanza::Presence(p) => Some(p),
            _ => None,
        })
        .expect("bot join presence");
    let hat = presence
        .payloads
        .iter()
        .find(|p| p.name() == "hats")
        .expect("bot hat on join presence");
    assert_eq!(hat.ns(), waddle_xmpp::xep::xep0317::NS_HATS);
    assert_eq!(
        waddle_xmpp::xep::xep0317::parse_hats_element(hat),
        waddle_xmpp::xep::xep0317::HatSet::new().with_hat(waddle_xmpp::xep::xep0317::Hat::bot()),
        "join presence preserves the existing bot hat"
    );
    let message = super::groupchat_ingress::groupchat_message(&wire);
    assert_eq!(message.type_, xmpp_parsers::message::MessageType::Groupchat);
    assert_eq!(
        waddle_xmpp_core::xep0359::extract_stanza_id_by(message, &fixture.room.clone().into())
            .as_deref(),
        Some(returned_id.as_str()),
        "host response is the canonical room stamp"
    );
    assert_eq!(
        message.from.as_ref().map(ToString::to_string),
        Some(format!("{}/direct-test", fixture.room))
    );
    assert_eq!(
        message.thread.as_ref().map(|t| t.id.as_str()),
        Some("root-message")
    );
    let reply = message
        .payloads
        .iter()
        .find(|p| p.name() == "reply")
        .expect("reply payload");
    assert_eq!(reply.ns(), waddle_xmpp::xep::NS_REPLY);
    assert_eq!(reply.attr("id"), Some("root-message"));
    assert_eq!(
        reply.attr("to"),
        Some(format!("{}/romeo", fixture.room).as_str())
    );
    let markup = message
        .payloads
        .iter()
        .find(|p| p.is("markup", waddle_xmpp::xep::NS_MESSAGE_MARKUP))
        .expect("markup");
    let quote = markup
        .get_child("bquote", waddle_xmpp::xep::NS_MESSAGE_MARKUP)
        .expect("blockquote");
    assert_eq!(quote.attr("start"), Some("0"));
    assert_eq!(quote.attr("end"), Some("9"));
    assert_eq!(
        waddle_xmpp_core::xep0359::extract_origin_id(message)
            .expect("origin")
            .as_str(),
        "groupchat-wire"
    );
    assert_eq!(
        f.count("ingress_messages").await,
        1,
        "real adapter must use ingress"
    );
    let key = key(&f).await;
    assert_eq!(
        f.optional_text("SELECT DISTINCT message_key FROM groupchat_notification_recovery")
            .await,
        Some(key.to_storage().to_string())
    );
    assert_eq!(
        f.count("groupchat_notification_recovery WHERE recipient_bare_jid = 'juliet@example.com'")
            .await,
        1
    );
    let mut tx = f.uow.begin().await.expect("authority inspection");
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("envelope load")
        .expect("envelope");
    assert_eq!(envelope.message().from, message.from);
    let recorded = EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("intents");
    assert!(recorded.iter().any(|i| matches!(i, IngressEffectIntent::ArchiveAuthoritative { archive, ordinal: Some(_), .. } if archive == &fixture.room)));
    assert!(recorded.iter().any(|i| matches!(i, IngressEffectIntent::RouteMucGroupchat { room, occupants, reflection, .. } if room == &fixture.room && occupants.contains(&"romeo@example.com/web".parse().expect("live")) && reflection == &fixture.invocation().actor_jid)));
    assert!(EffectReceiptRepository::receipts_complete(&mut tx, key)
        .await
        .expect("full receipts including host reflection"));
    assert!(CanonicalMessageRepository::is_terminal(&mut tx, key)
        .await
        .expect("terminal"));
    tx.commit().await.expect("inspection complete");
    fixture.close(f).await;
}

async fn replay_preserves_archive(f: IngressFixture) {
    let mut fixture = GroupchatFixture::new(&f).await;
    let first_id = fixture.send("stable-first").await.expect("first");
    assert_eq!(
        f.count("ingress_messages").await,
        1,
        "first bot send is canonical"
    );
    let recorded = intents(&f).await;
    let archive = recorded
        .iter()
        .find_map(|i| match i {
            IngressEffectIntent::ArchiveAuthoritative {
                stanza_id,
                ordinal: Some(ordinal),
                ..
            } => Some((stanza_id.clone(), *ordinal)),
            _ => None,
        })
        .expect("authoritative archive id and ordinal");
    fixture.drain();
    fixture.send("stable-second").await.expect("second");
    fixture.drain();
    let candidates = f.count("notification_candidates").await;
    let receipts = f.count("ingress_effect_receipts").await;
    let recovery = f.count("groupchat_notification_recovery").await;
    assert_eq!(
        fixture
            .send("stable-first")
            .await
            .expect("same-origin replay"),
        first_id
    );
    assert_eq!(f.count("ingress_messages").await, 2);
    assert_eq!(f.count("mam_messages").await, 2);
    assert_eq!(f.count("notification_candidates").await, candidates);
    assert_eq!(f.count("groupchat_notification_recovery").await, recovery);
    assert_eq!(f.count("ingress_effect_receipts").await, receipts);
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        2
    );
    let replayed = intents(&f).await;
    assert!(replayed.iter().any(|i| matches!(i, IngressEffectIntent::ArchiveAuthoritative { stanza_id, ordinal: Some(ordinal), .. } if stanza_id == &archive.0 && ordinal == &archive.1)));
    assert!(
        fixture.drain().is_empty(),
        "settled replay emits no occupant copy or join presence"
    );
    fixture.close(f).await;
}

#[tokio::test]
async fn extension_groupchat_wire_receipts_sqlite() {
    wire_and_receipts(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_wire_receipts_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_wire").await {
        wire_and_receipts(f).await;
    }
}
#[tokio::test]
async fn extension_groupchat_archive_replay_sqlite() {
    replay_preserves_archive(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_archive_replay_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_replay").await {
        replay_preserves_archive(f).await;
    }
}
