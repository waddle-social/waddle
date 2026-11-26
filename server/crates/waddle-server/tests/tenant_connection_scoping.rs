//! T1-F08-02: `ConnectionRegistry` isolates identical user localparts across tenants.

use jid::FullJid;
use tokio::sync::mpsc::error::TryRecvError;
use waddle_broker_core::identity::TenantId;
use waddle_xmpp::registry::{ConnectionRegistry, SendResult, SocketOutboundEndpoint};
use waddle_xmpp::Stanza;
use xmpp_parsers::message::{Lang, Message};

#[tokio::test]
async fn connection_registry_isolates_identical_localparts_across_tenants() {
    let tenant_acme = TenantId::parse("acme.example").expect("valid acme tenant");
    let tenant_globex = TenantId::parse("globex.example").expect("valid globex tenant");

    let reg_acme = ConnectionRegistry::for_tenant(tenant_acme.clone());
    let reg_globex = ConnectionRegistry::for_tenant(tenant_globex.clone());
    assert_eq!(reg_acme.tenant_id(), Some(&tenant_acme));
    assert_eq!(reg_globex.tenant_id(), Some(&tenant_globex));

    let shared_jid: FullJid = "alice@shared.example/web".parse().expect("valid full jid");
    let acme_only: FullJid = "alice@acme.example/web".parse().expect("valid acme jid");
    let globex_only: FullJid = "alice@globex.example/web"
        .parse()
        .expect("valid globex jid");

    let (ep_acme, mut rx_acme) = SocketOutboundEndpoint::channel(8);
    let (ep_globex, mut rx_globex) = SocketOutboundEndpoint::channel(8);
    let (ep_acme_domain, mut rx_acme_domain) = SocketOutboundEndpoint::channel(8);

    reg_acme.register(shared_jid.clone(), ep_acme);
    reg_acme.register(acme_only.clone(), ep_acme_domain);
    reg_globex.register(shared_jid.clone(), ep_globex);

    let mut msg = Message::new(None);
    msg.bodies.insert(Lang(String::new()), "acme secret".into());
    assert!(matches!(
        reg_acme
            .send_to(&shared_jid, Stanza::Message(msg.clone()))
            .await,
        SendResult::Sent
    ));
    assert!(rx_acme.try_recv().is_ok());
    assert!(matches!(rx_globex.try_recv(), Err(TryRecvError::Empty)));

    assert!(matches!(
        reg_globex
            .send_to(&acme_only, Stanza::Message(msg.clone()))
            .await,
        SendResult::NotConnected
    ));
    assert!(matches!(
        rx_acme_domain.try_recv(),
        Err(TryRecvError::Empty)
    ));
    assert!(matches!(
        reg_acme.send_to(&globex_only, Stanza::Message(msg)).await,
        SendResult::NotConnected
    ));
}
