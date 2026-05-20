use super::*;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use tokio::sync::oneshot;

/// Default mailbox capacity used by `Spawn::spawn` in kameo 0.20 — the test
/// exercises the bounded-mailbox backpressure path without depending on the
/// numeric default.
const DEFAULT_MAILBOX_CAPACITY: usize = 64;

struct HoldMailboxUntilReleased {
    release_rx: oneshot::Receiver<()>,
}

impl kameo::message::Message<HoldMailboxUntilReleased> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: HoldMailboxUntilReleased,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let _ = msg.release_rx.await;
    }
}

fn bare(user: &str) -> BareJid {
    format!("{user}@example.com").parse().expect("bare jid")
}

fn full(user: &str, resource: &str) -> FullJid {
    format!("{user}@example.com/{resource}")
        .parse()
        .expect("full jid")
}

async fn spawn_actor(user: &str) -> ActorRef<UserActor> {
    UserActor::spawn(UserActor::new(bare(user)))
}

#[tokio::test]
async fn test_register_and_resource_count() {
    let actor = spawn_actor("alice").await;

    actor
        .ask(RegisterConnection {
            jid: full("alice", "phone"),
        })
        .await
        .expect("register");

    let count: usize = actor.ask(ResourceCount).await.expect("count");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_unregister_cleans_up() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");

    actor
        .ask(RegisterConnection { jid: jid.clone() })
        .await
        .expect("register");

    actor
        .ask(UnregisterConnection { jid: jid.clone() })
        .await
        .expect("unregister");

    let count: usize = actor.ask(ResourceCount).await.expect("count");
    assert_eq!(count, 0);

    let connected: bool = actor.ask(IsConnected { jid }).await.expect("connected");
    assert!(!connected);
}

#[tokio::test]
async fn test_unregister_and_report_empty_is_atomic_per_user_actor() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");

    actor
        .ask(RegisterConnection { jid: jid.clone() })
        .await
        .expect("register");

    let is_empty: bool = actor
        .ask(UnregisterConnectionAndReportEmpty { jid })
        .await
        .expect("unregister+check");
    assert!(is_empty);
}

#[tokio::test]
async fn test_get_resources() {
    let actor = spawn_actor("alice").await;

    actor
        .ask(RegisterConnection {
            jid: full("alice", "phone"),
        })
        .await
        .expect("register");

    actor
        .ask(RegisterConnection {
            jid: full("alice", "laptop"),
        })
        .await
        .expect("register");

    let resources: Vec<FullJid> = actor.ask(GetResources).await.expect("resources");
    assert_eq!(resources.len(), 2);
}

#[tokio::test]
async fn test_get_other_resources() {
    let actor = spawn_actor("alice").await;
    let phone = full("alice", "phone");
    let laptop = full("alice", "laptop");

    actor
        .ask(RegisterConnection { jid: phone.clone() })
        .await
        .expect("register");

    actor
        .ask(RegisterConnection {
            jid: laptop.clone(),
        })
        .await
        .expect("register");

    let others: Vec<FullJid> = actor
        .ask(GetOtherResources {
            exclude: phone.clone(),
        })
        .await
        .expect("others");

    assert_eq!(others.len(), 1);
    assert_eq!(others[0], laptop);
}

#[tokio::test]
async fn test_presence_update_and_available_resources() {
    let actor = spawn_actor("alice").await;
    let phone = full("alice", "phone");
    let laptop = full("alice", "laptop");

    actor
        .ask(RegisterConnection { jid: phone.clone() })
        .await
        .expect("register");

    actor
        .ask(RegisterConnection {
            jid: laptop.clone(),
        })
        .await
        .expect("register");

    // Initially no resources are available
    let available: Vec<(FullJid, i8)> = actor.ask(GetAvailableResources).await.expect("available");
    assert!(available.is_empty());

    // Make phone available with priority 5
    let updated: bool = actor
        .ask(UpdatePresence {
            jid: phone.clone(),
            available: true,
            priority: 5,
        })
        .await
        .expect("update");
    assert!(updated);

    let available: Vec<(FullJid, i8)> = actor.ask(GetAvailableResources).await.expect("available");
    assert_eq!(available.len(), 1);
    assert_eq!(available[0].0, phone);
    assert_eq!(available[0].1, 5);
}

#[tokio::test]
async fn test_update_presence_missing_resource() {
    let actor = spawn_actor("alice").await;
    let missing = full("alice", "missing");

    let updated: bool = actor
        .ask(UpdatePresence {
            jid: missing,
            available: true,
            priority: 0,
        })
        .await
        .expect("update");
    assert!(!updated);
}

#[tokio::test]
async fn test_presence_state() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");

    // No state before setting
    let state: Option<PresenceState> = actor
        .ask(GetPresenceState { jid: jid.clone() })
        .await
        .expect("get");
    assert!(state.is_none());

    // Set state
    actor
        .ask(UpdatePresenceState {
            jid: jid.clone(),
            show: Some("away".to_string()),
            status: Some("Gone fishing".to_string()),
            priority: 3,
        })
        .await
        .expect("update");

    let state: Option<PresenceState> = actor
        .ask(GetPresenceState { jid: jid.clone() })
        .await
        .expect("get");
    let state = state.expect("should have state");
    assert_eq!(state.show.as_deref(), Some("away"));
    assert_eq!(state.status.as_deref(), Some("Gone fishing"));
    assert_eq!(state.priority, 3);

    // Clear state
    actor
        .ask(ClearPresenceState { jid: jid.clone() })
        .await
        .expect("clear");

    let state: Option<PresenceState> = actor.ask(GetPresenceState { jid }).await.expect("get");
    assert!(state.is_none());
}

#[tokio::test]
async fn test_pending_subscriptions() {
    let actor = spawn_actor("alice").await;

    let subscribe = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Subscribe);
    actor
        .ask(QueuePendingSubscription {
            stanza: Stanza::Presence(subscribe),
        })
        .await
        .expect("queue");

    let drained: Vec<Stanza> = actor.ask(DrainPendingSubscriptions).await.expect("drain");
    assert_eq!(drained.len(), 1);

    // Second drain should be empty
    let drained: Vec<Stanza> = actor.ask(DrainPendingSubscriptions).await.expect("drain");
    assert!(drained.is_empty());
}

#[tokio::test]
async fn test_carbons() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");

    actor
        .ask(RegisterConnection { jid: jid.clone() })
        .await
        .expect("register");

    // Default is disabled
    let enabled: bool = actor
        .ask(IsCarbonsEnabled { jid: jid.clone() })
        .await
        .expect("check");
    assert!(!enabled);

    // Enable
    actor
        .ask(SetCarbonsEnabled {
            jid: jid.clone(),
            enabled: true,
        })
        .await
        .expect("set");

    let enabled: bool = actor.ask(IsCarbonsEnabled { jid }).await.expect("check");
    assert!(enabled);
}

#[tokio::test]
async fn test_mailbox_backpressure_marks_best_effort_as_dropped() {
    let actor = spawn_actor("alice").await;

    let (release_tx, release_rx) = oneshot::channel();
    actor
        .tell(HoldMailboxUntilReleased { release_rx })
        .await
        .expect("hold message should enqueue");

    let jid = full("alice", "phone");
    let mut saw_mailbox_full = false;
    for _ in 0..(DEFAULT_MAILBOX_CAPACITY * 2) {
        let send_result = actor
            .tell(SetCarbonsEnabled {
                jid: jid.clone(),
                enabled: true,
            })
            .try_send();
        if matches!(send_result, Err(SendError::MailboxFull(_))) {
            saw_mailbox_full = true;
            break;
        }
        send_result.expect("best-effort tell should either enqueue or return MailboxFull");
    }
    assert!(
        saw_mailbox_full,
        "bounded mailbox should eventually apply backpressure"
    );

    let _ = release_tx.send(());
}
