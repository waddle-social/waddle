//! Concurrent registry of live MIX channels.
//!
//! Mirrors the shape of `crate::muc::MucRoomRegistry` but indexes channels
//! at the MIX subdomain (`mix.<domain>` by default).

use std::sync::Arc;

use dashmap::DashMap;
use jid::BareJid;
use tokio::sync::RwLock;
use tracing::{debug, info, instrument, warn};

use super::channel::{MixChannel, MixChannelConfig};
use crate::XmppError;

/// Light handle returned when callers want to hold a reference to a channel.
#[derive(Debug, Clone)]
pub struct MixChannelHandle {
    pub channel_jid: BareJid,
    pub channel: Arc<RwLock<MixChannel>>,
}

/// Summary used in disco#items listings.
#[derive(Debug, Clone)]
pub struct MixChannelInfo {
    pub channel_jid: BareJid,
    pub name: String,
    pub participant_count: usize,
}

/// Registry of all MIX channels on this server.
pub struct MixChannelRegistry {
    channels: DashMap<BareJid, Arc<RwLock<MixChannel>>>,
    mix_domain: String,
}

impl MixChannelRegistry {
    pub fn new(mix_domain: String) -> Self {
        info!(domain = %mix_domain, "Creating MIX channel registry");
        Self {
            channels: DashMap::new(),
            mix_domain,
        }
    }

    pub fn mix_domain(&self) -> &str {
        &self.mix_domain
    }

    pub fn is_mix_jid(&self, jid: &BareJid) -> bool {
        jid.domain().as_str() == self.mix_domain
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub fn exists(&self, jid: &BareJid) -> bool {
        self.channels.contains_key(jid)
    }

    pub fn get(&self, jid: &BareJid) -> Option<MixChannelHandle> {
        self.channels.get(jid).map(|entry| MixChannelHandle {
            channel_jid: jid.clone(),
            channel: entry.value().clone(),
        })
    }

    #[instrument(skip(self, config), fields(channel = %channel_jid))]
    pub fn get_or_create(
        &self,
        channel_jid: BareJid,
        waddle_id: String,
        channel_id: String,
        config: MixChannelConfig,
    ) -> Result<MixChannelHandle, XmppError> {
        if let Some(existing) = self.channels.get(&channel_jid) {
            debug!("MIX channel already exists");
            return Ok(MixChannelHandle {
                channel_jid,
                channel: existing.value().clone(),
            });
        }
        let ch = MixChannel::new(channel_jid.clone(), waddle_id, channel_id, config);
        let wrapped = Arc::new(RwLock::new(ch));
        self.channels.insert(channel_jid.clone(), wrapped.clone());
        info!("Created MIX channel");
        Ok(MixChannelHandle {
            channel_jid,
            channel: wrapped,
        })
    }

    #[instrument(skip(self, config), fields(channel = %channel_jid))]
    pub fn create(
        &self,
        channel_jid: BareJid,
        waddle_id: String,
        channel_id: String,
        config: MixChannelConfig,
    ) -> Result<MixChannelHandle, XmppError> {
        if self.channels.contains_key(&channel_jid) {
            return Err(XmppError::muc(format!(
                "MIX channel {} already exists",
                channel_jid
            )));
        }
        self.get_or_create(channel_jid, waddle_id, channel_id, config)
    }

    #[instrument(skip(self), fields(channel = %channel_jid))]
    pub fn destroy(&self, channel_jid: &BareJid) -> bool {
        let removed = self.channels.remove(channel_jid).is_some();
        if removed {
            info!("Destroyed MIX channel");
        } else {
            warn!("Attempted to destroy unknown MIX channel");
        }
        removed
    }

    pub fn list(&self) -> Vec<BareJid> {
        self.channels.iter().map(|e| e.key().clone()).collect()
    }

    pub async fn list_info(&self) -> Vec<MixChannelInfo> {
        let mut out = Vec::new();
        for entry in self.channels.iter() {
            let ch = entry.value().read().await;
            out.push(MixChannelInfo {
                channel_jid: ch.channel_jid.clone(),
                name: ch.config.name.clone(),
                participant_count: ch.participant_count(),
            });
        }
        out
    }
}

impl std::fmt::Debug for MixChannelRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MixChannelRegistry")
            .field("mix_domain", &self.mix_domain)
            .field("channel_count", &self.channels.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(name: &str) -> BareJid {
        format!("{}@mix.example.com", name).parse().unwrap()
    }

    #[test]
    fn test_create_and_lookup() {
        let reg = MixChannelRegistry::new("mix.example.com".into());
        assert_eq!(reg.channel_count(), 0);
        let _ = reg
            .create(
                channel("general"),
                "w".into(),
                "c".into(),
                MixChannelConfig::default(),
            )
            .unwrap();
        assert!(reg.exists(&channel("general")));
        assert_eq!(reg.channel_count(), 1);
    }

    #[test]
    fn test_create_duplicate_errors() {
        let reg = MixChannelRegistry::new("mix.example.com".into());
        reg.create(
            channel("g"),
            "w".into(),
            "c".into(),
            MixChannelConfig::default(),
        )
        .unwrap();
        assert!(reg
            .create(
                channel("g"),
                "w".into(),
                "c".into(),
                MixChannelConfig::default()
            )
            .is_err());
    }

    #[test]
    fn test_get_or_create_idempotent() {
        let reg = MixChannelRegistry::new("mix.example.com".into());
        let h1 = reg
            .get_or_create(
                channel("g"),
                "w".into(),
                "c".into(),
                MixChannelConfig::default(),
            )
            .unwrap();
        let h2 = reg
            .get_or_create(
                channel("g"),
                "w".into(),
                "c".into(),
                MixChannelConfig::default(),
            )
            .unwrap();
        assert_eq!(h1.channel_jid, h2.channel_jid);
        assert_eq!(reg.channel_count(), 1);
    }

    #[test]
    fn test_destroy() {
        let reg = MixChannelRegistry::new("mix.example.com".into());
        reg.create(
            channel("g"),
            "w".into(),
            "c".into(),
            MixChannelConfig::default(),
        )
        .unwrap();
        assert!(reg.destroy(&channel("g")));
        assert!(!reg.exists(&channel("g")));
        assert!(!reg.destroy(&channel("g")));
    }

    #[test]
    fn test_is_mix_jid() {
        let reg = MixChannelRegistry::new("mix.example.com".into());
        assert!(reg.is_mix_jid(&channel("g")));
        assert!(!reg.is_mix_jid(&"user@example.com".parse().unwrap()));
    }
}
