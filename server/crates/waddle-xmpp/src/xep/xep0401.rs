//! XEP-0401: Ad-hoc Account Invitation Generation
//!
//! Allows authorized users to generate invitation tokens for new account
//! registration. Invited users can register using the token without
//! needing open registration.
//!
//! ## XML Format
//!
//! Request invite via ad-hoc command:
//! ```xml
//! <iq type='set' to='example.com' id='inv-1'>
//!   <command xmlns='http://jabber.org/protocol/commands'
//!            node='urn:xmpp:invite#invite'
//!            action='execute'/>
//! </iq>
//! ```
//!
//! Server responds with invite URI:
//! ```xml
//! <iq type='result' from='example.com' id='inv-1'>
//!   <command xmlns='http://jabber.org/protocol/commands'
//!            node='urn:xmpp:invite#invite' status='completed'>
//!     <x xmlns='jabber:x:data' type='result'>
//!       <field var='landing-url' type='text-single'>
//!         <value>https://example.com/invite/TOKEN</value>
//!       </field>
//!       <field var='expire' type='text-single'>
//!         <value>2024-07-01T00:00:00Z</value>
//!       </field>
//!     </x>
//!   </command>
//! </iq>
//! ```
//!
//! ## Use Cases
//!
//! - "Invite a friend" feature in community apps
//! - Closed registration with invite-only access
//! - Track who invited whom

use chrono::{DateTime, Duration, Utc};

/// Command node for invite generation.
pub const COMMAND_NODE_INVITE: &str = "urn:xmpp:invite#invite";

/// An account invitation token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInvite {
    /// The opaque invitation token.
    pub token: String,
    /// Who generated this invite.
    pub inviter_jid: String,
    /// When the invite expires.
    pub expires: Option<DateTime<Utc>>,
    /// Optional landing page URL.
    pub landing_url: Option<String>,
    /// Whether this invite has been used.
    pub used: bool,
}

impl AccountInvite {
    /// Create a new invite.
    pub fn new(token: impl Into<String>, inviter: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            inviter_jid: inviter.into(),
            expires: None,
            landing_url: None,
            used: false,
        }
    }

    /// Set expiry (duration from now).
    pub fn with_expiry(mut self, duration: Duration) -> Self {
        self.expires = Some(Utc::now() + duration);
        self
    }

    /// Set expiry to a specific time.
    pub fn with_expires_at(mut self, at: DateTime<Utc>) -> Self {
        self.expires = Some(at);
        self
    }

    /// Set the landing URL.
    pub fn with_landing_url(mut self, url: impl Into<String>) -> Self {
        self.landing_url = Some(url.into());
        self
    }

    /// Returns `true` if the invite has expired.
    pub fn is_expired(&self) -> bool {
        self.expires.is_some_and(|e| e < Utc::now())
    }

    /// Returns `true` if the invite is still valid.
    pub fn is_valid(&self) -> bool {
        !self.used && !self.is_expired()
    }

    /// Mark as used.
    pub fn mark_used(&mut self) {
        self.used = true;
    }

    /// Generate an XMPP invite URI.
    pub fn to_xmpp_uri(&self, server: &str) -> String {
        format!("xmpp:{}?register;preauth={}", server, self.token)
    }
}

/// Manages account invitations.
#[derive(Debug, Default)]
pub struct InviteStore {
    invites: Vec<AccountInvite>,
}

impl InviteStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an invite.
    pub fn add(&mut self, invite: AccountInvite) {
        self.invites.push(invite);
    }

    /// Look up an invite by token.
    pub fn find_by_token(&self, token: &str) -> Option<&AccountInvite> {
        self.invites.iter().find(|i| i.token == token)
    }

    /// Validate and consume an invite token.
    pub fn redeem(&mut self, token: &str) -> Result<&AccountInvite, InviteRedeemError> {
        let invite = self
            .invites
            .iter_mut()
            .find(|i| i.token == token)
            .ok_or(InviteRedeemError::NotFound)?;

        if invite.used {
            return Err(InviteRedeemError::AlreadyUsed);
        }
        if invite.is_expired() {
            return Err(InviteRedeemError::Expired);
        }

        invite.used = true;
        Ok(invite)
    }

    /// Get all invites by a specific user.
    pub fn by_inviter(&self, jid: &str) -> Vec<&AccountInvite> {
        self.invites.iter().filter(|i| i.inviter_jid == jid).collect()
    }

    /// Get all valid (unused, not expired) invites.
    pub fn valid_invites(&self) -> Vec<&AccountInvite> {
        self.invites.iter().filter(|i| i.is_valid()).collect()
    }

    /// Remove expired invites.
    pub fn cleanup_expired(&mut self) {
        self.invites.retain(|i| !i.is_expired() || !i.used);
    }

    /// Total invites.
    pub fn total(&self) -> usize {
        self.invites.len()
    }
}

/// Errors when redeeming an invite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteRedeemError {
    /// Token not found.
    NotFound,
    /// Token already used.
    AlreadyUsed,
    /// Token has expired.
    Expired,
}

impl std::fmt::Display for InviteRedeemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("invite not found"),
            Self::AlreadyUsed => f.write_str("invite already used"),
            Self::Expired => f.write_str("invite expired"),
        }
    }
}

impl std::error::Error for InviteRedeemError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invite_new() {
        let inv = AccountInvite::new("tok-1", "alice@example.com");
        assert_eq!(inv.token, "tok-1");
        assert_eq!(inv.inviter_jid, "alice@example.com");
        assert!(!inv.used);
        assert!(inv.is_valid());
    }

    #[test]
    fn test_invite_expiry() {
        let expired = AccountInvite::new("tok", "a@b")
            .with_expiry(Duration::seconds(-1));
        assert!(expired.is_expired());
        assert!(!expired.is_valid());

        let valid = AccountInvite::new("tok", "a@b")
            .with_expiry(Duration::hours(24));
        assert!(!valid.is_expired());
        assert!(valid.is_valid());
    }

    #[test]
    fn test_invite_mark_used() {
        let mut inv = AccountInvite::new("tok", "a@b");
        assert!(inv.is_valid());
        inv.mark_used();
        assert!(!inv.is_valid());
    }

    #[test]
    fn test_invite_xmpp_uri() {
        let inv = AccountInvite::new("abc123", "a@b");
        assert_eq!(
            inv.to_xmpp_uri("example.com"),
            "xmpp:example.com?register;preauth=abc123"
        );
    }

    #[test]
    fn test_invite_landing_url() {
        let inv = AccountInvite::new("tok", "a@b")
            .with_landing_url("https://example.com/invite/tok");
        assert_eq!(
            inv.landing_url.as_deref(),
            Some("https://example.com/invite/tok")
        );
    }

    #[test]
    fn test_store_add_and_find() {
        let mut store = InviteStore::new();
        store.add(AccountInvite::new("tok-1", "alice@example.com"));
        store.add(AccountInvite::new("tok-2", "bob@example.com"));

        assert_eq!(store.total(), 2);
        assert!(store.find_by_token("tok-1").is_some());
        assert!(store.find_by_token("tok-3").is_none());
    }

    #[test]
    fn test_store_redeem() {
        let mut store = InviteStore::new();
        store.add(AccountInvite::new("tok-1", "alice@example.com"));

        let result = store.redeem("tok-1");
        assert!(result.is_ok());

        // Can't redeem twice
        let result2 = store.redeem("tok-1");
        assert_eq!(result2.unwrap_err(), InviteRedeemError::AlreadyUsed);

        // Can't redeem unknown
        let result3 = store.redeem("tok-unknown");
        assert_eq!(result3.unwrap_err(), InviteRedeemError::NotFound);
    }

    #[test]
    fn test_store_redeem_expired() {
        let mut store = InviteStore::new();
        store.add(
            AccountInvite::new("tok-exp", "a@b").with_expiry(Duration::seconds(-10)),
        );

        let result = store.redeem("tok-exp");
        assert_eq!(result.unwrap_err(), InviteRedeemError::Expired);
    }

    #[test]
    fn test_store_by_inviter() {
        let mut store = InviteStore::new();
        store.add(AccountInvite::new("t1", "alice@example.com"));
        store.add(AccountInvite::new("t2", "alice@example.com"));
        store.add(AccountInvite::new("t3", "bob@example.com"));

        assert_eq!(store.by_inviter("alice@example.com").len(), 2);
        assert_eq!(store.by_inviter("bob@example.com").len(), 1);
    }

    #[test]
    fn test_store_valid_invites() {
        let mut store = InviteStore::new();
        store.add(AccountInvite::new("valid", "a@b").with_expiry(Duration::hours(1)));
        store.add(AccountInvite::new("expired", "a@b").with_expiry(Duration::seconds(-1)));

        let valid = store.valid_invites();
        assert_eq!(valid.len(), 1);
        assert_eq!(valid[0].token, "valid");
    }

    #[test]
    fn test_invite_redeem_error_display() {
        assert_eq!(InviteRedeemError::NotFound.to_string(), "invite not found");
        assert_eq!(InviteRedeemError::AlreadyUsed.to_string(), "invite already used");
        assert_eq!(InviteRedeemError::Expired.to_string(), "invite expired");
    }

    #[test]
    fn test_command_node() {
        assert_eq!(COMMAND_NODE_INVITE, "urn:xmpp:invite#invite");
    }
}
