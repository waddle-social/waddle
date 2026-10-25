use super::relay::{RelayRemoteResourceRegistrationReply, RelayRemoteResourceRegistrationStatus};
use super::route_bridge::retry_remote_resource_register_test;
use super::NodeId;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[tokio::test]
async fn remote_resource_register_retries_busy_long_enough_to_observe_eventual_registration() {
    let jid = "juliet@example.test/phone"
        .parse::<jid::FullJid>()
        .expect("valid full jid");
    let owner = NodeId::new("owner-node".to_string());
    let attempts = Arc::new(AtomicUsize::new(0));
    let release_after = Instant::now() + Duration::from_millis(500);

    let reply = retry_remote_resource_register_test(&jid, &owner, Duration::from_secs(1), {
        let attempts = Arc::clone(&attempts);
        move || {
            let attempts = Arc::clone(&attempts);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok(RelayRemoteResourceRegistrationReply {
                    status: if Instant::now() < release_after {
                        RelayRemoteResourceRegistrationStatus::Busy
                    } else {
                        RelayRemoteResourceRegistrationStatus::Registered
                    },
                })
            }
        }
    })
    .await
    .expect("busy retries should eventually observe the successor registration");

    assert_eq!(
        reply.status,
        RelayRemoteResourceRegistrationStatus::Registered
    );
    assert!(
        attempts.load(Ordering::SeqCst) > 3,
        "the retry shape must outlive the old 3-attempt busy cap"
    );
}
