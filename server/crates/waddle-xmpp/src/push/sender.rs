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
        unread_count: u64,
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
        unread_count: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), PushError>> + Send + '_>> {
        let endpoint = subscription.endpoint.clone();
        // This sender targets an application relay/gateway endpoint. The relay is
        // responsible for performing standards-compliant Web Push (VAPID/encryption)
        // using the subscription keys supplied below.
        let payload = relay_payload(subscription, title, body, room_jid, unread_count);
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
    if let Some(channel_id) = crate::parse_managed_room_localpart(localpart) {
        return format!("/{}", channel_id);
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
    unread_count: u64,
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
                .send_notification(sub, &title, &preview, room_jid, unread_count)
                .await
            {
                debug!(user_jid = %jid, error = %e, "Push notification failed");
            }
        }
    }
}

fn relay_payload(
    subscription: &PushSubscription,
    title: &str,
    body: &str,
    room_jid: &str,
    unread_count: u64,
) -> serde_json::Value {
    serde_json::json!({
        "title": title,
        "body": body,
        "roomJid": room_jid,
        "url": room_jid_to_path(room_jid),
        "message-count": unread_count,
        "endpoint": subscription.endpoint.as_deref(),
        "p256dh": subscription.p256dh.as_deref(),
        "auth": subscription.auth_key.as_deref(),
    })
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
            _: u64,
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
            3,
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
            0,
        )
        .await;
        assert_eq!(sender.count(), 0);
    }

    #[test]
    fn relay_payload_includes_xep_0357_message_count() {
        let subscription = PushSubscription {
            user_jid: "alice@ex".into(),
            service_jid: "push.ex".into(),
            node: Some("n1".into()),
            endpoint: Some("https://ep".into()),
            p256dh: Some("BASE64KEY".into()),
            auth_key: Some("BASE64AUTH".into()),
        };
        let payload = relay_payload(
            &subscription,
            "Waddle",
            "New message",
            "c2@conference.example.com",
            6,
        );

        assert_eq!(payload["message-count"], 6);
        assert_eq!(payload["url"], "/c2");
        assert_eq!(payload["endpoint"], "https://ep");
        assert_eq!(payload["p256dh"], "BASE64KEY");
        assert_eq!(payload["auth"], "BASE64AUTH");
    }

    #[test]
    fn room_jid_to_path_uses_channel_id() {
        assert_eq!(room_jid_to_path("c2@conference.example.com"), "/c2");
        assert_eq!(room_jid_to_path("@conference.example.com"), "/");
    }
}
