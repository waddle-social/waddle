use super::*;
use minidom::Element;
use tokio::sync::mpsc;

fn create_test_config() -> RouterConfig {
    RouterConfig::new("waddle.social".to_string())
}

fn create_test_jid(jid_str: &str) -> Jid {
    jid_str.parse().unwrap()
}

fn parse_iq(xml: &str) -> Iq {
    let elem: Element = xml.parse().expect("valid xml");
    Iq::try_from(elem).expect("valid iq")
}

#[test]
fn test_router_config() {
    let config = RouterConfig::new("example.com".to_string());
    assert_eq!(config.local_domain, "example.com");
    assert_eq!(config.muc_domain, "muc.example.com");

    let config = config.with_muc_domain("chat.example.com".to_string());
    assert_eq!(config.muc_domain, "chat.example.com");
}

#[test]
fn test_get_destination_local() {
    let config = create_test_config();
    let registry = Arc::new(ConnectionRegistry::new());
    let router = StanzaRouter::new(config, registry);

    let jid = create_test_jid("user@waddle.social");
    assert_eq!(router.get_destination(&jid), RoutingDestination::Local);

    let jid = create_test_jid("user@waddle.social/resource");
    assert_eq!(router.get_destination(&jid), RoutingDestination::Local);
}

#[test]
fn test_get_destination_muc() {
    let config = create_test_config();
    let registry = Arc::new(ConnectionRegistry::new());
    let router = StanzaRouter::new(config, registry);

    let jid = create_test_jid("room@muc.waddle.social");
    assert_eq!(router.get_destination(&jid), RoutingDestination::LocalMuc);

    let jid = create_test_jid("room@muc.waddle.social/nick");
    assert_eq!(router.get_destination(&jid), RoutingDestination::LocalMuc);
}

#[test]
fn test_get_destination_spaces() {
    let config = create_test_config();
    let registry = Arc::new(ConnectionRegistry::new());
    let router = StanzaRouter::new(config, registry);

    let jid = create_test_jid("spaces.waddle.social");
    assert_eq!(
        router.get_destination(&jid),
        RoutingDestination::LocalSpaces
    );
}

#[test]
fn test_get_destination_remote() {
    let config = create_test_config();
    let registry = Arc::new(ConnectionRegistry::new());
    let router = StanzaRouter::new(config, registry);

    let jid = create_test_jid("user@example.com");
    assert_eq!(
        router.get_destination(&jid),
        RoutingDestination::Remote {
            domain: "example.com".to_string()
        }
    );

    let jid = create_test_jid("user@other.social/resource");
    assert_eq!(
        router.get_destination(&jid),
        RoutingDestination::Remote {
            domain: "other.social".to_string()
        }
    );
}

#[test]
fn test_is_local_jid() {
    let config = create_test_config();
    let registry = Arc::new(ConnectionRegistry::new());
    let router = StanzaRouter::new(config, registry);

    assert!(router.is_local_jid(&create_test_jid("user@waddle.social")));
    assert!(router.is_local_jid(&create_test_jid("room@muc.waddle.social")));
    assert!(router.is_local_jid(&create_test_jid("spaces.waddle.social")));
    assert!(!router.is_local_jid(&create_test_jid("user@example.com")));
}

#[test]
fn test_is_muc_jid() {
    let config = create_test_config();
    let registry = Arc::new(ConnectionRegistry::new());
    let router = StanzaRouter::new(config, registry);

    assert!(!router.is_muc_jid(&create_test_jid("user@waddle.social")));
    assert!(router.is_muc_jid(&create_test_jid("room@muc.waddle.social")));
    assert!(!router.is_muc_jid(&create_test_jid("user@example.com")));
}

#[test]
fn test_is_remote_jid() {
    let config = create_test_config();
    let registry = Arc::new(ConnectionRegistry::new());
    let router = StanzaRouter::new(config, registry);

    assert!(!router.is_remote_jid(&create_test_jid("user@waddle.social")));
    assert!(!router.is_remote_jid(&create_test_jid("room@muc.waddle.social")));
    assert!(router.is_remote_jid(&create_test_jid("user@example.com")));
}

#[test]
fn test_federation_disabled_by_default() {
    let config = create_test_config();
    let registry = Arc::new(ConnectionRegistry::new());
    let router = StanzaRouter::new(config, registry);

    assert!(!router.is_remote_routing_enabled());
}

#[tokio::test]
async fn test_route_iq_local_muc_bare_non_jingle_routes_to_connected_resource() {
    let config = create_test_config();
    let registry = Arc::new(ConnectionRegistry::new());
    let (tx, mut rx) = mpsc::channel(16);
    let full_room_jid: FullJid = "room@muc.waddle.social/nick".parse().unwrap();
    registry.register(full_room_jid, tx);
    let router = StanzaRouter::new(config, registry);

    let sender_jid: FullJid = "sender@waddle.social/resource".parse().unwrap();
    let iq = parse_iq(
        r#"<iq xmlns='jabber:client' type='get' from='sender@waddle.social/resource' to='room@muc.waddle.social' id='iq-muc-get'>
            <query xmlns='jabber:iq:version'/>
        </iq>"#,
    );

    let result = router.route_iq(iq, &sender_jid).await.unwrap();
    assert!(matches!(
        result,
        RoutingResult::DeliveredLocal {
            delivered_count: 1,
            offline_count: 0
        }
    ));
    assert!(rx.recv().await.is_some());
}
