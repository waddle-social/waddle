//! Sticker packs over the session's `WaddleClient` (XEP-0449).
//!
//! Like `session.rs` this only adapts: wire packs become the plain types
//! `stickers.rs` works with, and failures become a sentence for a toast.

use std::sync::Arc;

use waddle_xmpp_client_ffi::WaddleError;

use crate::runtime;
use crate::session::Session;
use crate::stickers::{self, Pack};

fn reason(error: &WaddleError) -> String {
    match error {
        WaddleError::NotConnected => "You are not connected.".to_string(),
        WaddleError::Stanza { condition, .. } if condition == "item-not-found" => {
            "That sticker pack no longer exists.".to_string()
        }
        WaddleError::InvalidArgument => "That sticker pack cannot be copied.".to_string(),
        _ => "The request failed. Try again.".to_string(),
    }
}

impl Session {
    /// The account's own sticker packs that have a sticker to send.
    pub async fn sticker_packs(&self) -> Result<Vec<Pack>, String> {
        let client = Arc::clone(&self.client);
        match runtime::spawn(async move { client.fetch_sticker_packs(None).await }).await {
            Ok(Ok(packs)) => Ok(stickers::usable(packs)),
            Ok(Err(error)) => {
                tracing::debug!(?error, "could not fetch the sticker packs");
                Err("Your stickers could not be loaded.".to_string())
            }
            Err(_) => Err("Your stickers could not be loaded.".to_string()),
        }
    }

    /// One pack from `owner`'s own sticker node, for viewing.
    pub async fn sticker_pack(&self, owner: &str, pack_id: &str) -> Result<Option<Pack>, String> {
        let client = Arc::clone(&self.client);
        let (owner, pack_id) = (owner.to_string(), pack_id.to_string());
        match runtime::spawn(async move { client.fetch_sticker_pack(owner, None, pack_id).await })
            .await
        {
            Ok(Ok(pack)) => Ok(pack.map(Pack::from_wire)),
            Ok(Err(error)) => {
                tracing::debug!(?error, "could not fetch a sticker pack");
                Err("The sticker pack could not be loaded.".to_string())
            }
            Err(_) => Err("The sticker pack could not be loaded.".to_string()),
        }
    }

    /// Copy `owner`'s pack to this account's own sticker node, under the
    /// same id.
    pub async fn import_sticker_pack(&self, owner: &str, pack_id: &str) -> Result<(), String> {
        let client = Arc::clone(&self.client);
        let (owner, pack_id) = (owner.to_string(), pack_id.to_string());
        match runtime::spawn(async move { client.import_sticker_pack(owner, None, pack_id).await })
            .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => {
                tracing::debug!(?error, "could not copy a sticker pack");
                Err(reason(&error))
            }
            Err(_) => Err(reason(&WaddleError::NotConnected)),
        }
    }
}
