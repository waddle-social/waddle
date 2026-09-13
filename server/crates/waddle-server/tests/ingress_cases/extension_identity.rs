use super::*;
use jid::BareJid;
use waddle_extensions::PluginId;
use waddle_server::{
    ingress::{AliasOutcomeClass, ExtensionPrincipal, IngressPrincipal, IngressStreamIdentity},
    ingress_uow::{ConfiguredPluginGrants, ExtensionGrantRepository},
};
use waddle_xmpp::{
    auth::ExtensionGrantRef,
    ingress::{DigestContext, DigestInput, NormalizedTarget, TransportGeneration},
};

async fn grant(f: &IngressFixture, plugin: &PluginId, room: Option<&BareJid>) -> ExtensionGrantRef {
    let mut tx = f.uow.begin().await.expect("transaction");
    ExtensionGrantRepository::sync_configured(
        &mut tx,
        &[ConfiguredPluginGrants {
            plugin: plugin.clone(),
            can_send: true,
            provider_rooms: room.cloned().into_iter().collect(),
        }],
    )
    .await
    .expect("sync");
    let grant = match room {
        Some(room) => ExtensionGrantRepository::active_room_grant(&mut tx, plugin, room).await,
        None => ExtensionGrantRepository::active_send_grant(&mut tx, plugin).await,
    }
    .expect("lookup")
    .expect("active grant");
    tx.commit().await.expect("commit");
    grant
}
fn extension(
    f: &IngressFixture,
    grant: ExtensionGrantRef,
    sender: BareJid,
    requester: Option<BareJid>,
    origin: &str,
) -> IngressSubmission {
    let mut s = f.submission(Some(origin), "hello");
    s.identity = IngressStreamIdentity::Extension {
        plugin: grant.plugin.clone(),
        requester: requester.clone(),
    };
    s.principal = IngressPrincipal::Extension(ExtensionPrincipal {
        grant,
        requester,
        sender: sender.clone(),
    });
    s.sender = sender.with_resource_str("extension-host").expect("sender");
    s.plan.sanitized_message.from = Some(s.sender.clone().into());
    s.connection_generation = TransportGeneration::Host;
    digest(&mut s);
    s
}
fn digest(s: &mut IngressSubmission) {
    s.digest_input = DigestInput::from_parsed(
        &s.plan.sanitized_message,
        &DigestContext {
            target: s.target.clone(),
            server_authorities: vec![s.principal.bare_jid().clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
}
async fn admission(f: IngressFixture) {
    let plugin = PluginId::new("test-plugin").expect("plugin");
    let sender: BareJid = "test-plugin@extensions.example.com"
        .parse()
        .expect("sender");
    let g = grant(&f, &plugin, None).await;
    let s = extension(&f, g, sender.clone(), None, "extension-origin");
    f.execute("DELETE FROM sessions", ()).await;
    let first = commit_submission(&f.uow, &s, 1)
        .await
        .expect("host admission");
    let replay = commit_submission(&f.uow, &s, 1).await.expect("replay");
    assert_eq!(replay.alias, AliasOutcomeClass::Existing);
    assert_eq!(first.message_key, replay.message_key);
    assert_eq!(f.count("ingress_messages").await, 1);
    assert_eq!(
        f.optional_text("SELECT sender_bare_jid FROM ingress_origin_aliases")
            .await,
        Some(sender.to_string())
    );
    let mut tx = f.uow.begin().await.expect("transaction");
    ExtensionGrantRepository::revoke_plugin(&mut tx, &plugin)
        .await
        .expect("revoke");
    tx.commit().await.expect("commit");
    assert_eq!(
        commit_submission(&f.uow, &s, 1)
            .await
            .expect_err("revoked replay")
            .class(),
        IngressDecisionClass::PrincipalMissing
    );
    let mut fresh = s.clone();
    fresh.plan.sanitized_message = f
        .submission(Some("revoked-new-origin"), "hello")
        .plan
        .sanitized_message;
    digest(&mut fresh);
    assert_eq!(
        commit_submission(&f.uow, &fresh, 1)
            .await
            .expect_err("revoked new submission")
            .class(),
        IngressDecisionClass::PrincipalMissing
    );
    assert_eq!(f.count("ingress_messages").await, 1);
    f.close().await;
}
async fn refusal(f: IngressFixture) {
    let plugin = PluginId::new("test-plugin").expect("plugin");
    let room: BareJid = "room@conference.example.com".parse().expect("room");
    let g = grant(&f, &plugin, Some(&room)).await;
    let mut s = extension(&f, g, f.principal.bare_jid().clone(), None, "refused");
    assert_eq!(
        commit_submission(&f.uow, &s, 1)
            .await
            .expect_err("wrong room")
            .class(),
        IngressDecisionClass::PrincipalMissing
    );
    s.target = NormalizedTarget::Bare(room.clone());
    s.plan.sanitized_message.to = Some(room.into());
    digest(&mut s);
    s.identity = IngressStreamIdentity::Extension {
        plugin: PluginId::new("other").expect("plugin"),
        requester: None,
    };
    assert_eq!(
        commit_submission(&f.uow, &s, 1)
            .await
            .expect_err("plugin mismatch")
            .class(),
        IngressDecisionClass::PrincipalMissing
    );
    s.identity = IngressStreamIdentity::Extension {
        plugin,
        requester: Some(f.principal.bare_jid().clone()),
    };
    assert_eq!(
        commit_submission(&f.uow, &s, 1)
            .await
            .expect_err("requester mismatch")
            .class(),
        IngressDecisionClass::PrincipalMissing
    );
    assert_eq!(f.count("ingress_messages").await, 0);
    assert_eq!(f.count("ingress_origin_aliases").await, 0);
    f.close().await;
}
async fn namespaces(f: IngressFixture) {
    for name in ["plugin-x", "plugin-y"] {
        let plugin = PluginId::new(name).expect("plugin");
        let g = grant(&f, &plugin, None).await;
        let sender: BareJid = format!("{name}@extensions.example.com")
            .parse()
            .expect("sender");
        let mut s = extension(&f, g, sender, None, "shared");
        let room: BareJid = "room@conference.example.com".parse().expect("room");
        s.target = NormalizedTarget::Bare(room.clone());
        s.plan.sanitized_message.to = Some(room.into());
        s.plan.sanitized_message.type_ = xmpp_parsers::message::MessageType::Groupchat;
        digest(&mut s);
        assert_eq!(
            commit_submission(&f.uow, &s, 1)
                .await
                .expect("sender namespace")
                .alias,
            AliasOutcomeClass::Inserted
        );
    }
    assert_eq!(f.count("ingress_messages").await, 2);
    let client = f.submission(Some("client"), "hello");
    let first = commit_submission(&f.uow, &client, 1).await.expect("client");
    let g = grant(&f, &PluginId::new("direct").expect("plugin"), None).await;
    let req = f.principal.bare_jid().clone();
    let mut s = extension(&f, g, req.clone(), Some(req), "client");
    digest(&mut s);
    let replay = commit_submission(&f.uow, &s, 1)
        .await
        .expect("requester namespace");
    assert_eq!(replay.alias, AliasOutcomeClass::Existing);
    assert_eq!(replay.message_key, first.message_key);
    assert_eq!(f.count("ingress_messages").await, 3);
    f.close().await;
}
#[tokio::test]
async fn ingress_extension_admission_sqlite() {
    admission(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_extension_admission_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_admission").await {
        admission(f).await;
    }
}
#[tokio::test]
async fn ingress_extension_refusal_sqlite() {
    refusal(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_extension_refusal_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_refusal").await {
        refusal(f).await;
    }
}
#[tokio::test]
async fn ingress_extension_namespaces_sqlite() {
    namespaces(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_extension_namespaces_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_namespaces").await {
        namespaces(f).await;
    }
}

async fn requester_fence(fixture: IngressFixture) {
    let plugin = PluginId::new("requester-plugin").expect("plugin");
    let grant = grant(&fixture, &plugin, None).await;
    let requester = fixture.principal.bare_jid().clone();
    let submission = extension(
        &fixture,
        grant,
        requester.clone(),
        Some(requester),
        "requester-fence",
    );
    let mut wrong_transport = submission.clone();
    wrong_transport.connection_generation =
        TransportGeneration::Connection(waddle_xmpp::ingress::ConnectionGeneration::INITIAL);
    assert_eq!(
        commit_submission(&fixture.uow, &wrong_transport, 1)
            .await
            .expect_err("host cannot claim a connection")
            .class(),
        IngressDecisionClass::PrincipalMissing
    );
    let mut wrong_identity = submission.clone();
    wrong_identity.identity = IngressStreamIdentity::Ephemeral {
        principal: fixture.principal.clone(),
    };
    assert_eq!(
        commit_submission(&fixture.uow, &wrong_identity, 1)
            .await
            .expect_err("extension cannot claim authenticated identity")
            .class(),
        IngressDecisionClass::PrincipalMissing
    );
    fixture.execute("DELETE FROM sessions", ()).await;
    fixture.execute("DELETE FROM users", ()).await;
    let failure = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect_err("deleted requester");
    assert_eq!(failure.class(), IngressDecisionClass::PrincipalMissing);
    assert!(matches!(
        failure.source,
        waddle_server::ingress_uow::IngressUowError::ExtensionGrantAssertionFailed(
            waddle_server::ingress_uow::GrantAssertionFailure::RequesterGone
        )
    ));
    assert_eq!(fixture.count("ingress_messages").await, 0);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 0);
    fixture.close().await;
}
#[tokio::test]
async fn ingress_extension_requester_fence_sqlite() {
    requester_fence(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_extension_requester_fence_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_requester").await {
        requester_fence(f).await;
    }
}
