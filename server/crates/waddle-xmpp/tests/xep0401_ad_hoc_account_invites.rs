//! XEP-0401: Ad-hoc Account Invitation Generation dedicated suite.
//!
//! Pins the command node and pre-auth URI shapes against the spec and
//! exercises the invite store lifecycle: redeem-once semantics, typed
//! redemption errors, expiry, and cleanup.

use chrono::Duration;
use waddle_xmpp::xep::{AccountInvite, InviteRedeemError, InviteStore, COMMAND_NODE_INVITE};

#[test]
fn xep0401_command_node_matches_spec() {
    // XEP-0401 §4 uses the ad-hoc command node urn:xmpp:invite#invite.
    assert_eq!(COMMAND_NODE_INVITE, "urn:xmpp:invite#invite");
}

#[test]
fn xep0401_preauth_uri_has_register_and_preauth_query() {
    // §4.2 / XEP-0445: the URI carries a preauth token for IBR.
    let invite = AccountInvite::new("BpjjiSXhcJhngkDL", "inviter@example.com");
    assert_eq!(
        invite.to_xmpp_uri("example.com"),
        "xmpp:example.com?register;preauth=BpjjiSXhcJhngkDL"
    );
}

#[test]
fn xep0401_new_invite_is_valid_until_used() {
    let mut invite = AccountInvite::new("tok", "inviter@example.com");
    assert!(invite.is_valid());
    assert!(!invite.is_expired());

    invite.mark_used();
    assert!(!invite.is_valid());
}

#[test]
fn xep0401_expiry_invalidates_invite() {
    let expired = AccountInvite::new("tok", "a@b").with_expiry(Duration::seconds(-1));
    assert!(expired.is_expired());
    assert!(!expired.is_valid());

    let live = AccountInvite::new("tok", "a@b").with_expiry(Duration::hours(24));
    assert!(live.is_valid());

    let unexpiring = AccountInvite::new("tok", "a@b");
    assert!(!unexpiring.is_expired(), "no expiry means never expired");
}

#[test]
fn xep0401_redeem_is_single_use() {
    let mut store = InviteStore::new();
    store.add(AccountInvite::new("tok-1", "inviter@example.com"));

    let redeemed = store.redeem("tok-1").expect("first redemption succeeds");
    assert!(redeemed.used);

    assert_eq!(
        store.redeem("tok-1").expect_err("second redemption fails"),
        InviteRedeemError::AlreadyUsed
    );
}

#[test]
fn xep0401_redeem_unknown_token_is_not_found() {
    let mut store = InviteStore::new();
    assert_eq!(
        store.redeem("missing").expect_err("unknown token"),
        InviteRedeemError::NotFound
    );
}

#[test]
fn xep0401_redeem_expired_token_is_expired_error_and_stays_unused() {
    let mut store = InviteStore::new();
    store.add(AccountInvite::new("tok-exp", "a@b").with_expiry(Duration::seconds(-10)));

    assert_eq!(
        store.redeem("tok-exp").expect_err("expired token"),
        InviteRedeemError::Expired
    );
    // A failed redemption must not consume the token.
    assert!(!store.find_by_token("tok-exp").expect("still stored").used);
}

#[test]
fn xep0401_valid_invites_excludes_used_and_expired() {
    let mut store = InviteStore::new();
    store.add(AccountInvite::new("live", "a@b").with_expiry(Duration::hours(1)));
    store.add(AccountInvite::new("expired", "a@b").with_expiry(Duration::seconds(-1)));
    store.add(AccountInvite::new("burned", "a@b"));
    store.redeem("burned").expect("redeemable");

    let valid = store.valid_invites();
    assert_eq!(valid.len(), 1);
    assert_eq!(valid[0].token, "live");
}

#[test]
fn xep0401_cleanup_expired_removes_all_expired_invites() {
    let mut store = InviteStore::new();
    store.add(AccountInvite::new("live", "a@b").with_expiry(Duration::hours(1)));
    store.add(AccountInvite::new("expired-unused", "a@b").with_expiry(Duration::seconds(-1)));
    store.add(AccountInvite::new("expired-used", "a@b").with_expiry(Duration::hours(1)));
    // Redeem, then force expiry by rewriting the deadline in the past.
    store.redeem("expired-used").expect("redeemable");
    store.add(
        AccountInvite::new("expired-used-2", "a@b")
            .with_expires_at(chrono::Utc::now() - Duration::hours(1)),
    );

    store.cleanup_expired();

    assert!(store.find_by_token("live").is_some());
    assert!(
        store.find_by_token("expired-unused").is_none(),
        "expired invites must be removed even if never redeemed"
    );
    assert!(store.find_by_token("expired-used-2").is_none());
    assert!(
        store.find_by_token("expired-used").is_some(),
        "not expired yet"
    );
}

#[test]
fn xep0401_by_inviter_filters_tokens() {
    let mut store = InviteStore::new();
    store.add(AccountInvite::new("t1", "alice@example.com"));
    store.add(AccountInvite::new("t2", "alice@example.com"));
    store.add(AccountInvite::new("t3", "bob@example.com"));

    assert_eq!(store.by_inviter("alice@example.com").len(), 2);
    assert_eq!(store.by_inviter("bob@example.com").len(), 1);
    assert!(store.by_inviter("carol@example.com").is_empty());
    assert_eq!(store.total(), 3);
}

#[test]
fn xep0401_landing_url_is_carried_for_web_onboarding() {
    // §4.2: the server SHOULD provide a landing-url alongside the URI.
    let invite =
        AccountInvite::new("tok", "a@b").with_landing_url("https://example.com/invite/tok");
    assert_eq!(
        invite.landing_url.as_deref(),
        Some("https://example.com/invite/tok")
    );
}

#[test]
fn xep0401_redeem_errors_are_typed_and_displayable() {
    assert_eq!(InviteRedeemError::NotFound.to_string(), "invite not found");
    assert_eq!(
        InviteRedeemError::AlreadyUsed.to_string(),
        "invite already used"
    );
    assert_eq!(InviteRedeemError::Expired.to_string(), "invite expired");
}
