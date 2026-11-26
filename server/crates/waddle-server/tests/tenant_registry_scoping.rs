//! T1-F08-01: `RoomRegistryActor` isolates identical room localparts across tenants.

use jid::BareJid;
use kameo::actor::Spawn;
use waddle_broker_core::identity::TenantId;
use waddle_xmpp::muc::room_registry_actor::{
    CreateRoom, GetTenantId, RoomExists, RoomRegistryActor,
};
use waddle_xmpp::muc::RoomConfig;
use waddle_xmpp::xep::xep0421::OccupantIdSecret;

#[tokio::test]
async fn room_registry_actor_isolates_identical_room_localparts_across_tenants() {
    let secret = OccupantIdSecret::try_new([0x33_u8; 32].to_vec()).expect("valid secret");
    let tenant_acme = TenantId::parse("acme.example").expect("valid acme tenant");
    let tenant_globex = TenantId::parse("globex.example").expect("valid globex tenant");

    let registry_acme = RoomRegistryActor::spawn(RoomRegistryActor::for_tenant(
        tenant_acme.clone(),
        "muc.shared.example".to_owned(),
        secret.clone(),
    ));
    let registry_globex = RoomRegistryActor::spawn(RoomRegistryActor::for_tenant(
        tenant_globex.clone(),
        "muc.shared.example".to_owned(),
        secret,
    ));

    let acme_id = registry_acme
        .ask(GetTenantId)
        .await
        .expect("acme tenant id");
    let globex_id = registry_globex
        .ask(GetTenantId)
        .await
        .expect("globex tenant id");
    assert_eq!(acme_id.0, tenant_acme);
    assert_eq!(globex_id.0, tenant_globex);

    let room_jid: BareJid = "general@muc.shared.example"
        .parse()
        .expect("valid room jid");
    let room_actor_acme = registry_acme
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "acme".to_owned(),
            channel_id: "general".to_owned(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create general room in acme");

    assert!(registry_acme
        .ask(RoomExists {
            room_jid: room_jid.clone(),
        })
        .await
        .expect("acme room exists"));
    assert!(!registry_globex
        .ask(RoomExists {
            room_jid: room_jid.clone(),
        })
        .await
        .expect("globex must not see acme room"));

    let room_actor_globex = registry_globex
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "globex".to_owned(),
            channel_id: "general".to_owned(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create general room in globex independently");

    assert_ne!(
        room_actor_acme.id(),
        room_actor_globex.id(),
        "identical room localparts in different tenants must map to separate RoomActors"
    );
}
