//! XEP-0298 (Coin) dedicated suite: the `calls.<domain>` mixer is the
//! conference focus that emits `<conference-info isfocus='true'/>`, so
//! it (and only it) advertises `urn:xmpp:coin:1` in disco#info
//! ("If an entity supports Coin, it SHOULD advertise that fact by
//! returning a feature of \"urn:xmpp:coin:1\"").

use waddle_xmpp::disco::info::{call_features, calls_mixer_features, server_features};
use waddle_xmpp::protocol::handlers::jingle::NS_COIN;
use waddle_xmpp_core::disco::info::Feature;

#[test]
fn xep0298_mixer_advertises_coin_feature() {
    assert_eq!(NS_COIN, "urn:xmpp:coin:1");
    assert!(calls_mixer_features().contains(&Feature::new("urn:xmpp:coin:1")));
}

#[test]
fn xep0298_coin_is_not_advertised_on_the_server_domain() {
    let coin = Feature::new("urn:xmpp:coin:1");
    assert!(!server_features().contains(&coin));
    assert!(!call_features().contains(&coin));
}
