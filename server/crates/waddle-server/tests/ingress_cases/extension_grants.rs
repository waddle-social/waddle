use super::ingress_support::IngressFixture;
use waddle_extensions::PluginId;
use waddle_server::ingress_uow::{
    ConfiguredPluginGrants, ExtensionGrantRepository as Grants, GrantAssertion,
    GrantAssertionFailure, IngressUowError,
};
use waddle_xmpp::auth::{ExtensionGrantId, ExtensionGrantRef, ExtensionGrantScope};

async fn exercise(fixture: IngressFixture) {
    let plugin = PluginId::new("grant-test").expect("plugin");
    let other = PluginId::new("other-plugin").expect("plugin");
    let room: jid::BareJid = "room@conference.example.com".parse().expect("room");
    let replacement: jid::BareJid = "new@conference.example.com".parse().expect("room");
    let mut configured = ConfiguredPluginGrants {
        plugin: plugin.clone(),
        can_send: true,
        provider_rooms: vec![room.clone()],
    };
    let mut tx = fixture.uow.begin().await.expect("transaction");
    let sync = Grants::sync_configured(&mut tx, std::slice::from_ref(&configured))
        .await
        .expect("sync");
    assert_eq!((sync.inserted, sync.revoked), (2, 0));
    let send = Grants::active_send_grant(&mut tx, &plugin)
        .await
        .expect("lookup")
        .expect("send grant");
    let provider = Grants::active_room_grant(&mut tx, &plugin, &room)
        .await
        .expect("lookup")
        .expect("room grant");
    assert_eq!(
        Grants::assert_grant(&mut tx, &send).await.expect("assert"),
        GrantAssertion::Asserted
    );
    assert_eq!(
        Grants::assert_grant(&mut tx, &provider)
            .await
            .expect("assert"),
        GrantAssertion::Asserted
    );
    assert_eq!(
        Grants::assert_requester(&mut tx, fixture.principal.bare_jid())
            .await
            .expect("requester"),
        GrantAssertion::Asserted
    );
    let missing = ExtensionGrantRef {
        grant_id: ExtensionGrantId::new(uuid::Uuid::new_v4()),
        ..send.clone()
    };
    assert_failure(
        Grants::assert_grant(&mut tx, &missing).await,
        GrantAssertionFailure::Missing,
    );
    let mismatch = ExtensionGrantRef {
        plugin: other.clone(),
        ..send.clone()
    };
    assert_failure(
        Grants::assert_grant(&mut tx, &mismatch).await,
        GrantAssertionFailure::Mismatch,
    );
    let mismatch = ExtensionGrantRef {
        scope: ExtensionGrantScope::ProviderRoom(replacement.clone()),
        ..provider.clone()
    };
    assert_failure(
        Grants::assert_grant(&mut tx, &mismatch).await,
        GrantAssertionFailure::Mismatch,
    );
    assert_failure(
        Grants::assert_requester(&mut tx, &"deleted@example.com".parse().expect("requester")).await,
        GrantAssertionFailure::RequesterGone,
    );
    let sync = Grants::sync_configured(&mut tx, std::slice::from_ref(&configured))
        .await
        .expect("idempotent sync");
    assert_eq!((sync.inserted, sync.revoked), (0, 0));
    configured.provider_rooms = vec![replacement.clone()];
    let sync = Grants::sync_configured(&mut tx, std::slice::from_ref(&configured))
        .await
        .expect("room change");
    assert_eq!((sync.inserted, sync.revoked), (1, 1));
    assert_failure(
        Grants::assert_grant(&mut tx, &provider).await,
        GrantAssertionFailure::Revoked,
    );
    assert!(Grants::active_room_grant(&mut tx, &plugin, &room)
        .await
        .expect("lookup")
        .is_none());
    configured.can_send = false;
    let sync = Grants::sync_configured(&mut tx, std::slice::from_ref(&configured))
        .await
        .expect("capability loss");
    assert_eq!(sync.revoked, 2);
    assert_failure(
        Grants::assert_grant(&mut tx, &send).await,
        GrantAssertionFailure::Revoked,
    );
    assert!(Grants::active_send_grant(&mut tx, &plugin)
        .await
        .expect("lookup")
        .is_none());
    configured.can_send = true;
    Grants::sync_configured(&mut tx, std::slice::from_ref(&configured))
        .await
        .expect("restore config");
    let renewed = Grants::active_send_grant(&mut tx, &plugin)
        .await
        .expect("lookup")
        .expect("renewed");
    assert_ne!(renewed.grant_id, send.grant_id);
    let second = ConfiguredPluginGrants {
        plugin: other.clone(),
        can_send: true,
        provider_rooms: vec![],
    };
    Grants::sync_configured(&mut tx, &[configured.clone(), second.clone()])
        .await
        .expect("add plugin");
    let sync = Grants::sync_configured(&mut tx, &[second])
        .await
        .expect("remove plugin");
    assert_eq!((sync.inserted, sync.revoked), (0, 2));
    assert!(Grants::active_send_grant(&mut tx, &other)
        .await
        .expect("other remains")
        .is_some());
    tx.commit().await.expect("commit");
    let mut tx = fixture.uow.begin().await.expect("new transaction");
    assert!(Grants::active_send_grant(&mut tx, &plugin)
        .await
        .expect("durable revocation")
        .is_none());
    assert!(Grants::active_send_grant(&mut tx, &other)
        .await
        .expect("durable grant")
        .is_some());
    tx.commit().await.expect("commit");
    fixture.close().await;
}

fn assert_failure(
    result: Result<GrantAssertion, IngressUowError>,
    expected: GrantAssertionFailure,
) {
    assert!(
        matches!(result, Err(IngressUowError::ExtensionGrantAssertionFailed(actual)) if actual == expected)
    );
}

#[tokio::test]
async fn sqlite_extension_grants_lifecycle() {
    exercise(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_extension_grants_lifecycle() {
    let Some(fixture) = IngressFixture::postgres("extension_grants").await else {
        return;
    };
    exercise(fixture).await;
}
