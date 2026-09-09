use super::*;

// XEP-0045 §7.4: observers see the committed groupchat reflection, including
// the room occupant sender, once per plugin across an ordinary ingress replay.
async fn committed_groupchat_observers(fixture: IngressFixture) {
    use waddle_xmpp::ingress::{DigestContext, DigestInput, NormalizedTarget};
    use xmpp_parsers::message::{Id, MessageType};

    let a = plugin("observer-a");
    let b = plugin("observer-b");
    let manager = ExtensionManager::with_observer_test_plugins(vec![a.clone(), b.clone()]).await;
    let mut submission = fixture.submission(Some("xep0045-observers"), "committed room message");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    submission.target = NormalizedTarget::Bare(room.clone());
    submission.plan.sanitized_message.to = Some(room.clone().into());
    submission.plan.sanitized_message.type_ = MessageType::Groupchat;
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .expect("groupchat digest");
    let request = submission.plan.sanitized_message.clone();
    submission.plan.sanitized_message.from =
        Some("room@muc.example.com/romeo".parse().expect("occupant"));
    submission.plan.sanitized_message.id = Some(Id("committed-room-id".into()));
    select_observers(&mut submission, &manager);
    for planned in &mut submission.plan.plan {
        let Effect::External(ExternalEffect::Room(ExternalRoomEffect::ObserveRoomMessage {
            error_request,
            ..
        })) = &mut planned.effect
        else {
            panic!("observer")
        };
        **error_request = request.clone();
    }
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit groupchat");
    assert!(a.invocations().is_empty());
    assert!(
        b.invocations().is_empty(),
        "planning and committing never invoke observers"
    );
    let report = execute_with_manager(&fixture, &first, manager).await;
    assert!(report
        .outcomes
        .iter()
        .all(|(_, outcome)| *outcome == ExternalOutcome::Done));
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    for plugin in [&a, &b] {
        let invocations = plugin.invocations();
        assert_eq!(invocations.len(), 1);
        let invocation = &invocations[0];
        assert_eq!(invocation.body.as_str(), "committed room message");
        assert_eq!(
            invocation.context.room.as_ref().expect("room").as_str(),
            "room@muc.example.com"
        );
        assert_eq!(
            invocation
                .context
                .sender
                .as_ref()
                .expect("occupant")
                .as_str(),
            "room@muc.example.com/romeo"
        );
        assert_eq!(
            invocation.context.stanza_id.as_ref().expect("id").as_str(),
            "committed-room-id"
        );
    }
    let replay = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("replay groupchat");
    let restarted = ExtensionManager::with_observer_test_plugins(vec![a.clone(), b.clone()]).await;
    let report = execute_with_manager(&fixture, &replay, restarted).await;
    assert!(report
        .outcomes
        .iter()
        .all(|(_, outcome)| *outcome == ExternalOutcome::Done));
    assert_eq!(a.invocations().len(), 1);
    assert_eq!(b.invocations().len(), 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_xep0045_committed_groupchat_observed_once_per_plugin_across_replay() {
    committed_groupchat_observers(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_xep0045_committed_groupchat_observed_once_per_plugin_across_replay() {
    if let Some(fixture) = IngressFixture::postgres("xep0045_observers").await {
        committed_groupchat_observers(fixture).await;
    }
}
