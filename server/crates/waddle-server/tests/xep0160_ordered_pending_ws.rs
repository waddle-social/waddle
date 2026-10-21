//! XEP-0160 §2 offline recovery preserves Waddle's archive order across resources.
//!
//! A live negative-priority phone retains A's SM custody. A tablet's first
//! available presence must defer B, then retry after A is acknowledged without
//! requiring another presence stanza.

pub mod pending_delivery_ws_support;

use pending_delivery_ws_support::{
    ack, barrier_without_message, enable, message, presence, recv, PendingFixture,
};
use std::time::Duration;
use waddle_xmpp::stream_management::SM_NS;

#[tokio::test]
async fn unacknowledged_predecessor_defers_then_retries_offline_delivery_on_another_resource() {
    let fixture = PendingFixture::start().await;
    let mut sender = fixture.connect("alice", "sender").await;
    fixture.offer(&mut sender, "pending-a").await;
    let first = fixture.wait_rows(|rows| rows.len() == 1).await.remove(0);
    assert!(first.payload.is_archived());

    let mut phone = fixture.connect("bob", "phone").await;
    let phone_session = enable(&mut phone).await;
    presence(&mut phone, 0).await;
    let mut phone_handled = 0;
    message(&mut phone, &mut phone_handled, "pending-a").await;
    assert!(recv(&mut phone).await.is("r", SM_NS));
    presence(&mut phone, -1).await;
    barrier_without_message(&mut phone, &mut phone_handled, 2).await;

    // XEP-0160: a connected resource with negative priority must not receive
    // new bare-JID messages. B therefore enters durable offline storage.
    fixture.offer(&mut sender, "pending-b").await;
    let queued = fixture.wait_rows(|rows| rows.len() == 2).await;
    let second = queued
        .iter()
        .find(|row| row.id != first.id)
        .expect("pending B");
    assert!(second.payload.is_archived());
    assert!(second.flushed_in_session.is_none());
    let second_id = second.id.clone();

    // An observation-only trigger provides a durable witness of the actual
    // claim/defer/release path, so a timer-based absence check cannot pass
    // merely because the tablet's flush has not started yet.
    let db = fixture.storage.database();
    {
        let guard = db.guard().await.expect("inspection database");
        guard
            .execute(
                "CREATE TABLE observed_pending_releases (row_id TEXT NOT NULL)",
                (),
            )
            .await
            .expect("release observation table");
        guard.execute(
            "CREATE TRIGGER observe_pending_release AFTER UPDATE OF flushed_in_session ON pending_delivery \
             WHEN OLD.flushed_in_session IS NOT NULL AND NEW.flushed_in_session IS NULL \
             BEGIN INSERT INTO observed_pending_releases (row_id) VALUES (NEW.row_id); END", (),
        ).await.expect("release observation trigger");
    }
    let mut tablet = fixture.connect("bob", "tablet").await;
    let tablet_session = enable(&mut tablet).await;
    presence(&mut tablet, 0).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let guard = db.guard().await.expect("inspection database");
            let mut result = guard
                .query(
                    "SELECT COUNT(*) FROM observed_pending_releases WHERE row_id = ?",
                    waddle_server::db_params![second_id.as_str()],
                )
                .await
                .expect("observed B deferrals");
            let count: i64 = result
                .next()
                .await
                .expect("count row")
                .expect("count")
                .get(0)
                .expect("release count");
            if count >= 2 {
                break;
            }
            drop(result);
            drop(guard);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("tablet's initial flush and retry both defer B behind A");
    let mut tablet_handled = 0;
    barrier_without_message(&mut tablet, &mut tablet_handled, 1).await;
    let blocked = fixture.rows().await;
    assert_eq!(blocked.len(), 2);
    let a = blocked
        .iter()
        .find(|row| row.id == first.id)
        .expect("retained A");
    assert_eq!(a.flushed_in_session.as_ref(), Some(&phone_session));
    assert!(a.outbound_sequence.is_some());
    assert!(blocked
        .iter()
        .find(|row| row.id == second_id)
        .expect("deferred B")
        .outbound_sequence
        .is_none());

    // No second tablet presence: the already-running retry must make progress.
    ack(&mut phone, phone_handled).await;
    message(&mut tablet, &mut tablet_handled, "pending-b").await;
    assert!(recv(&mut tablet).await.is("r", SM_NS));
    let delivered = fixture.rows().await;
    assert_eq!(delivered.len(), 1, "A's ACK removes A's durable row");
    assert_eq!(delivered[0].id, second_id);
    assert_eq!(
        delivered[0].flushed_in_session.as_ref(),
        Some(&tablet_session)
    );
    assert_eq!(delivered[0].outbound_sequence, Some(tablet_handled));
    ack(&mut tablet, tablet_handled).await;
    fixture
        .wait_rows(<[waddle_xmpp::pending_delivery::PendingRow]>::is_empty)
        .await;
}

/// XEP-0160 exemption (D13): a user bot never binds a resource, so a DM to it
/// is archived in the bot's MAM and is never stored offline. The sender is
/// not bounced.
#[tokio::test]
async fn bot_recipient_is_archived_not_queued_offline() {
    let fixture = PendingFixture::start().await;
    let url = format!(
        "sqlite://{}?mode=rwc",
        fixture.directory.path().join("pending.db").display()
    );
    let pool = sqlx::SqlitePool::connect(&url)
        .await
        .expect("seed database");
    let bot: jid::BareJid = "bob+standup@bots.localhost".parse().expect("bot JID");
    sqlx::query(
        "INSERT INTO users (jid, username, xmpp_localpart, localpart_key, kind, owner_jid, \
         created_at, updated_at) VALUES (?, ?, ?, ?, 'bot', ?, ?, ?)",
    )
    .bind(bot.as_str())
    .bind("bob+standup")
    .bind("bob+standup")
    .bind("bob+standup")
    .bind("bob@localhost")
    .bind("2026-01-01T00:00:00.000000Z")
    .bind("2026-01-01T00:00:00.000000Z")
    .execute(&pool)
    .await
    .expect("seed bot account");

    let mut sender = fixture.connect("alice", "sender").await;
    enable(&mut sender).await;
    let mut chat = xmpp_parsers::message::Message::new(Some(bot.clone().into()));
    chat.type_ = xmpp_parsers::message::MessageType::Chat;
    chat.id = Some(xmpp_parsers::message::Id("to-bot".to_owned()));
    chat.bodies
        .insert(xmpp_parsers::message::Lang::new(), "to-bot".to_owned());
    pending_delivery_ws_support::send(&mut sender, chat.into()).await;

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let archived: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ? AND body = 'to-bot'",
            )
            .bind(bot.as_str())
            .fetch_one(&pool)
            .await
            .expect("bot archive count");
            if archived == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the bot's archive holds the DM");

    let mut sender_handled = 0;
    // The DM is the only inbound stanza; a bounce would arrive as a message
    // before the acknowledgement and fail the barrier.
    barrier_without_message(&mut sender, &mut sender_handled, 1).await;
    {
        use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
        assert!(
            fixture
                .storage
                .list(&bot)
                .await
                .expect("bot pending rows")
                .is_empty(),
            "no offline row for a bot recipient"
        );
    }
    let pending: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pending_delivery WHERE recipient_jid = ?")
            .bind(bot.as_str())
            .fetch_one(&pool)
            .await
            .expect("pending count");
    assert_eq!(pending, 0);
    pool.close().await;
}
