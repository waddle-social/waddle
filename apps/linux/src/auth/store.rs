//! Signed-in sessions, kept in the Secret Service.
//!
//! One item per account, keyed by server origin and bare JID, so several
//! accounts (multi-tenancy: one per community) can be signed in at once.
//! `oo7` talks to the Secret Service directly, or through the secret portal
//! inside Flatpak.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::device::{ServerUrl, SignedInSession};

const APPLICATION: &str = "social.waddle.Waddle";

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("the system keyring is unavailable: {0}")]
    Keyring(Box<oo7::Error>),
    #[error("a stored session could not be read")]
    Corrupt,
}

impl From<oo7::Error> for StoreError {
    fn from(error: oo7::Error) -> Self {
        Self::Keyring(Box::new(error))
    }
}

/// An account this app holds a session for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAccount {
    pub server: ServerUrl,
    pub session: SignedInSession,
    /// This install's XMPP resource for the account, kept so every
    /// reconnect binds the same full JID (no ghost room occupants).
    pub resource: String,
}

impl StoredAccount {
    /// A freshly signed-in account with a new random resource.
    pub fn new(server: ServerUrl, session: SignedInSession) -> Self {
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        Self {
            server,
            session,
            resource: format!("waddle-linux-{}", &suffix[..8]),
        }
    }
}

/// What the keyring item's secret holds.
#[derive(Serialize, Deserialize)]
struct StoredSecret {
    session: SignedInSession,
    resource: String,
}

pub struct CredentialStore {
    keyring: oo7::Keyring,
}

impl CredentialStore {
    pub async fn open() -> Result<Self, StoreError> {
        Ok(Self {
            keyring: oo7::Keyring::new().await?,
        })
    }

    pub async fn save(&self, account: &StoredAccount) -> Result<(), StoreError> {
        let server_key = account.server.to_string();
        let jid = account.session.jid.to_string();
        let secret = serde_json::to_vec(&StoredSecret {
            session: account.session.clone(),
            resource: account.resource.clone(),
        })
        .map_err(|_| StoreError::Corrupt)?;
        self.keyring
            .create_item(
                &format!("Waddle: {jid}"),
                &attributes(&server_key, &jid),
                secret,
                true,
            )
            .await?;
        Ok(())
    }

    pub async fn accounts(&self) -> Result<Vec<StoredAccount>, StoreError> {
        let items = self
            .keyring
            .search_items(&HashMap::from([("application", APPLICATION)]))
            .await?;
        let mut accounts = Vec::with_capacity(items.len());
        for item in items {
            let attributes = item.attributes().await?;
            let server = attributes
                .get("server")
                .and_then(|raw| ServerUrl::parse(raw).ok())
                .ok_or(StoreError::Corrupt)?;
            let secret = item.secret().await?;
            let stored: StoredSecret =
                serde_json::from_slice(&secret).map_err(|_| StoreError::Corrupt)?;
            accounts.push(StoredAccount {
                server,
                session: stored.session,
                resource: stored.resource,
            });
        }
        Ok(accounts)
    }

    pub async fn remove(&self, account: &StoredAccount) -> Result<(), StoreError> {
        let server_key = account.server.to_string();
        let jid = account.session.jid.to_string();
        self.keyring.delete(&attributes(&server_key, &jid)).await?;
        Ok(())
    }
}

fn attributes<'a>(server: &'a str, jid: &'a str) -> HashMap<&'static str, &'a str> {
    HashMap::from([
        ("application", APPLICATION),
        ("server", server),
        ("jid", jid),
    ])
}
