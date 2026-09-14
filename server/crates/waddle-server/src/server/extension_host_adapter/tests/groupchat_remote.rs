use super::groupchat_ingress::GroupchatFixture;
use crate::{
    ingress::test_support::IngressFixture,
    server::{extension_host_adapter::ExtensionHostAdapterError, routes::interpret},
};
use std::sync::Arc;
use waddle_xmpp::ownership::{NodeIdentity, SharedNodeIdentity};

async fn remote_room_refused(f: IngressFixture) {
    let mut fixture = GroupchatFixture::new(&f).await;
    let state = Arc::get_mut(&mut fixture.adapter.state).expect("unique websocket state");
    let app = Arc::get_mut(&mut state.deps.app_state).expect("unique app state");
    app.clustering_claims = crate::clustering::ClusteringHandles {
        claim_store: Some(Arc::new(interpret::PlanningClaims::new(NodeIdentity::new(
            "remote", "epoch",
        )))),
        node_identity: Some(SharedNodeIdentity::new(NodeIdentity::new("local", "epoch"))),
        ..Default::default()
    };
    let result = fixture.send("remote-extension").await;
    assert!(
        matches!(
            result,
            Err(ExtensionHostAdapterError::Plan(
                interpret::effects::PlanFailure::ExtensionRemoteRoomUnsupported
            ))
        ),
        "remote-owned room must fail with a typed plan refusal: {result:?}"
    );
    assert_eq!(f.count("ingress_messages").await, 0);
    assert_eq!(f.count("mam_messages").await, 0);
    assert!(
        fixture.drain().is_empty(),
        "remote refusal precedes bot lifecycle effects"
    );
    fixture.close(f).await;
}

#[tokio::test]
async fn extension_groupchat_remote_refused_sqlite() {
    remote_room_refused(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_remote_refused_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_remote").await {
        remote_room_refused(f).await;
    }
}
