use super::*;

#[test]
fn service_domains_use_xmpp_domain_for_all_components() {
    let domains = XmppServiceDomains::new("waddle.social");

    assert_eq!(domains.extensions, "extensions.waddle.social");
    assert_eq!(domains.muc, "muc.waddle.social");
    assert_eq!(domains.push, "push.waddle.social");
    assert_eq!(domains.spaces, "spaces.waddle.social");
    assert_eq!(domains.upload, "upload.waddle.social");
}

#[tokio::test]
async fn extension_route_channel_permission_allows_bootstrap_chat_member() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;

    assert!(
        !managed_channel_permission_allowed(
            state.as_ref(),
            Some(&session),
            "chat",
            Permission::View,
        )
        .await
        .expect("initial permission check"),
        "server membership should be required before default chat policy applies"
    );

    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
                Relation::new("member"),
                Subject::user(&session.user_id),
            ),
        })
        .await
        .expect("server member tuple");

    let owner_session = create_test_session(state.as_ref(), "owner").await;
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
                Relation::new("owner"),
                Subject::user(&owner_session.user_id),
            ),
        })
        .await
        .expect("server owner tuple");

    assert!(
        managed_channel_permission_allowed(
            state.as_ref(),
            Some(&session),
            "chat",
            Permission::View,
        )
        .await
        .expect("chat permission check"),
        "default chat routes should inherit deployment membership"
    );
    assert!(
        managed_channel_permission_allowed(
            state.as_ref(),
            Some(&session),
            "announcements",
            Permission::View,
        )
        .await
        .expect("announcements view permission check"),
        "default announcement route reads should inherit deployment membership"
    );
    assert!(
        managed_channel_permission_allowed(
            state.as_ref(),
            Some(&owner_session),
            "chat",
            Permission::View,
        )
        .await
        .expect("owner chat permission check"),
        "default room membership policy must include deployment owners"
    );
    assert!(
        managed_channel_permission_allowed(
            state.as_ref(),
            Some(&owner_session),
            "announcements",
            Permission::SendMessage,
        )
        .await
        .expect("owner announcements send permission check"),
        "deployment owners should be allowed to publish announcement extension state"
    );
    assert!(
        !managed_channel_permission_allowed(
            state.as_ref(),
            Some(&session),
            "announcements",
            Permission::SendMessage,
        )
        .await
        .expect("announcements send permission check"),
        "announcement extension publishes still require owner permissions"
    );
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Channel, "announcements"),
                Relation::new("writer"),
                Subject::user(&session.user_id),
            ),
        })
        .await
        .expect("announcement writer tuple");
    assert!(
        !managed_channel_permission_allowed(
            state.as_ref(),
            Some(&session),
            "announcements",
            Permission::SendMessage,
        )
        .await
        .expect("announcements writer permission check"),
        "announcement channel writer grants must not bypass server-owner write policy"
    );
    assert!(
        !managed_channel_permission_allowed(
            state.as_ref(),
            Some(&session),
            "random",
            Permission::View,
        )
        .await
        .expect("random channel permission check"),
        "non-default channels still require channel permissions"
    );
}
