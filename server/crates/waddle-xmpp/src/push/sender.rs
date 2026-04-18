//! Web Push notification sender.

use std::future::Future;
use std::pin::Pin;

use tracing::{debug, warn};

use super::store::PushSubscriptionStore;
use super::{PushError, PushSubscription};

/// Trait for sending Web Push notifications.
pub trait WebPushSender: Send + Sync + 'static {
    /// Send a push notification to the given subscription.
    fn send_notification(
        &self,
        subscription: &PushSubscription,
        title: &str,
        body: &str,
        room_jid: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PushError>> + Send + '_>>;
}

/// HTTP-based Web Push sender that POSTs JSON to a push relay/gateway.
#[derive(Debug, Clone)]
pub struct HttpWebPushSender {
    client: reqwest::Client,
}

impl HttpWebPushSender {
    /// Create a new HTTP Web Push sender with a 10-second request timeout.
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build HTTP Web Push client");
        Self { client }
    }
}

impl Default for HttpWebPushSender {
    fn default() -> Self {
        Self::new()
    }
}

impl WebPushSender for HttpWebPushSender {
    fn send_notification(
        &self,
        subscription: &PushSubscription,
        title: &str,
        body: &str,
        room_jid: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PushError>> + Send + '_>> {
        let endpoint = subscription.endpoint.clone();
        let subscription_endpoint = subscription.endpoint.clone();
        let subscription_p256dh = subscription.p256dh.clone();
        let subscription_auth = subscription.auth_key.clone();
        // This sender targets an application relay/gateway endpoint. The relay is
        // responsible for performing standards-compliant Web Push (VAPID/encryption)
        // using the subscription keys supplied below.
        let payload = serde_json::json!({
            "title": title,
            "body": body,
            "roomJid": room_jid,
            "url": room_jid_to_path(room_jid),
            "endpoint": subscription_endpoint,
            "p256dh": subscription_p256dh,
            "auth": subscription_auth,
        });
        Box::pin(async move {
            let ep = endpoint.as_deref().ok_or(PushError::MissingEndpoint)?;
            match self.client.post(ep).json(&payload).send().await {
                Ok(resp) if resp.status().is_success() => {
                    debug!(endpoint = %ep, "Push notification delivered");
                    Ok(())
                }
                Ok(resp) => {
                    let status = resp.status();
                    warn!(endpoint = %ep, status = %status, "Push rejected");
                    Err(PushError::SendFailed(format!("HTTP {}", status)))
                }
                Err(e) => {
                    warn!(endpoint = %ep, error = %e, "Push delivery failed");
                    Err(PushError::HttpError(e.to_string()))
                }
            }
        })
    }
}

fn room_jid_to_path(room_jid: &str) -> String {
    let localpart = room_jid.split('@').next().unwrap_or_default();
    if let Some((waddle_id, channel_id)) = crate::parse_managed_room_localpart(localpart) {
        return format!("/{}/{}", waddle_id, channel_id);
    }
    "/".to_string()
}

/// Send push notifications for mentioned users.
pub async fn notify_mentioned_users<S, W>(
    store: &S,
    sender: &W,
    mentioned_jids: &[String],
    sender_nick: &str,
    body: &str,
    room_jid: &str,
) where
    S: PushSubscriptionStore + ?Sized,
    W: WebPushSender + ?Sized,
{
    for jid in mentioned_jids {
        let subs = match store.get_for_user(jid).await {
            Ok(s) => s,
            Err(e) => {
                debug!(user_jid = %jid, error = %e, "Failed to get push subs");
                continue;
            }
        };
        if subs.is_empty() {
            continue;
        }
        let title = format!("@{} mentioned you", sender_nick);
        let preview = if body.len() > 100 {
            let end = body.char_indices().nth(100).map_or(body.len(), |(i, _)| i);
            format!("{}...", &body[..end])
        } else {
            body.to_string()
        };
        for sub in &subs {
            if let Err(e) = sender
                .send_notification(sub, &title, &preview, room_jid)
                .await
            {
                debug!(user_jid = %jid, error = %e, "Push notification failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::InMemoryPushStore;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockSender(AtomicUsize);
    impl MockSender {
        fn new() -> Self {
            Self(AtomicUsize::new(0))
        }
        fn count(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }
    impl WebPushSender for MockSender {
        fn send_notification(
            &self,
            _: &PushSubscription,
            _: &str,
            _: &str,
            _: &str,
        ) -> Pin<Box<dyn Future<Output = Result<(), PushError>> + Send + '_>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn test_notify_sends_to_subscribed() {
        let store = InMemoryPushStore::new();
        store
            .register(PushSubscription {
                user_jid: "alice@ex".into(),
                service_jid: "push.ex".into(),
                node: Some("n1".into()),
                endpoint: Some("https://ep".into()),
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect("ok");

        let sender = MockSender::new();
        notify_mentioned_users(
            &store,
            &sender,
            &["alice@ex".into(), "bob@ex".into()],
            "charlie",
            "Hey!",
            "room@muc",
        )
        .await;
        assert_eq!(sender.count(), 1); // only alice has a sub
    }

    #[tokio::test]
    async fn test_notify_no_subs() {
        let store = InMemoryPushStore::new();
        let sender = MockSender::new();
        notify_mentioned_users(
            &store,
            &sender,
            &["alice@ex".into()],
            "bob",
            "Hi",
            "room@muc",
        )
        .await;
        assert_eq!(sender.count(), 0);
    }

    #[test]
    fn room_jid_to_path_uses_waddle_and_channel_ids() {
        assert_eq!(room_jid_to_path("w1_c2@conference.example.com"), "/w1/c2");
        assert_eq!(room_jid_to_path("room@conference.example.com"), "/");
    }
}
