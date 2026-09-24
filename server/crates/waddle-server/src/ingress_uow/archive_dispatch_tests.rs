use super::*;
use crate::{
    ingress::{commit::commit_submission, receipt_key, test_support::IngressFixture},
    ingress_uow::{CanonicalMessageRepository, DeliveryProgressRepository},
};
use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent};

async fn message(
    fixture: &IngressFixture,
    body: &str,
    resources: &[FullJid],
) -> (MessageKey, EffectReceiptKey) {
    let intent = IngressEffectIntent::RouteDirect {
        recipient: "juliet@example.com"
            .parse()
            .expect("valid archive owner JID"),
        fanout: resources.to_vec(),
        route_identity: EffectMessageIdentity::capture_ordinal(1),
    };
    let mut submission = fixture.submission(None, body);
    submission.plan.intents = vec![intent.clone()];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit route authority");
    (
        decision.message_key.expect("canonical key"),
        receipt_key(&intent).expect("direct route has a receipt key"),
    )
}

async fn register(
    fixture: &IngressFixture,
    key: MessageKey,
    receipt: &EffectReceiptKey,
    ordinal: i64,
    targets: &[DispatchTarget],
) {
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin ingress transaction");
    assert!(CanonicalMessageRepository::lock(&mut tx, key)
        .await
        .expect("lock canonical message"));
    let obligations = targets
        .iter()
        .cloned()
        .map(|target| ArchiveDispatchObligation {
            receipt: receipt.clone(),
            target,
        })
        .collect::<Vec<_>>();
    ArchiveDispatchRepository::record(
        &mut tx,
        key,
        &"juliet@example.com"
            .parse()
            .expect("valid archive owner JID"),
        ArchiveOrdinal::from_storage(ordinal).expect("valid archive ordinal fixture"),
        &obligations,
    )
    .await
    .expect("record archive dispatch obligations");
    tx.commit().await.expect("commit ingress transaction");
}

async fn readiness(
    fixture: &IngressFixture,
    key: MessageKey,
    receipt: &EffectReceiptKey,
    resource: Option<&FullJid>,
) -> DispatchReadiness {
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin ingress transaction");
    let result = ArchiveDispatchRepository::readiness(&mut tx, key, receipt, resource, None)
        .await
        .expect("read archive dispatch readiness");
    tx.commit().await.expect("commit ingress transaction");
    result
}

async fn finish(fixture: &IngressFixture, key: MessageKey, receipt: &EffectReceiptKey) {
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin ingress transaction");
    assert!(CanonicalMessageRepository::lock(&mut tx, key)
        .await
        .expect("lock canonical message"));
    EffectReceiptRepository::record_receipt(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash,
    )
    .await
    .expect("record completed effect receipt");
    tx.commit().await.expect("commit ingress transaction");
}

async fn resource_order(fixture: IngressFixture) {
    let first: FullJid = "juliet@example.com/one"
        .parse()
        .expect("valid first recipient resource JID");
    let second: FullJid = "juliet@example.com/two"
        .parse()
        .expect("valid second recipient resource JID");
    let targets = vec![
        DispatchTarget::Resource(first.clone()),
        DispatchTarget::Resource(second.clone()),
    ];
    let (a, ar) = message(&fixture, "earlier", &[first.clone(), second.clone()]).await;
    let (b, br) = message(&fixture, "later", &[first.clone(), second.clone()]).await;
    register(&fixture, a, &ar, 1, &targets).await;
    register(&fixture, b, &br, 42, &targets).await;
    assert_eq!(
        readiness(&fixture, a, &ar, None).await,
        DispatchReadiness::Ready
    );
    assert_eq!(
        readiness(&fixture, b, &br, None).await,
        DispatchReadiness::Blocked(vec![a])
    );
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin ingress transaction");
    assert!(CanonicalMessageRepository::lock(&mut tx, a)
        .await
        .expect("lock canonical message"));
    DeliveryProgressRepository::record(&mut tx, a, &ar, std::slice::from_ref(&first))
        .await
        .expect("record first resource delivery progress");
    tx.commit().await.expect("commit ingress transaction");
    assert_eq!(
        readiness(&fixture, b, &br, Some(&first)).await,
        DispatchReadiness::Ready
    );
    assert_eq!(
        readiness(&fixture, a, &ar, Some(&first)).await,
        DispatchReadiness::Completed
    );
    assert_eq!(
        readiness(&fixture, b, &br, Some(&second)).await,
        DispatchReadiness::Blocked(vec![a])
    );
    assert_eq!(
        readiness(&fixture, b, &br, None).await,
        DispatchReadiness::Blocked(vec![a])
    );
    // A different resource cannot discharge the remaining copy. Its exact
    // carbon progress can: the predecessor query shares both evidence stores.
    super::super::CarbonReceiptRepository::record(
        &fixture.uow,
        a,
        &ar,
        std::slice::from_ref(&second),
    )
    .await
    .expect("record second resource carbon receipt");
    assert_eq!(
        readiness(&fixture, b, &br, None).await,
        DispatchReadiness::Ready
    );
    // Repair and retries must retain the original position across fresh UoWs.
    register(&fixture, a, &ar, 1, &targets).await;
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin ingress transaction");
    let wrong = ArchiveDispatchRepository::record(
        &mut tx,
        a,
        &"juliet@example.com"
            .parse()
            .expect("valid archive owner JID"),
        ArchiveOrdinal::from_storage(43).expect("valid archive ordinal fixture"),
        &[ArchiveDispatchObligation {
            receipt: ar.clone(),
            target: targets[0].clone(),
        }],
    )
    .await;
    assert!(matches!(wrong, Err(IngressUowError::EffectIntentConflict)));
    drop(tx);
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin ingress transaction");
    let changed_target = ArchiveDispatchRepository::record(
        &mut tx,
        a,
        &"juliet@example.com"
            .parse()
            .expect("valid archive owner JID"),
        ArchiveOrdinal::FIRST,
        &[ArchiveDispatchObligation {
            receipt: ar.clone(),
            target: DispatchTarget::ArchiveWide,
        }],
    )
    .await;
    assert!(matches!(
        changed_target,
        Err(IngressUowError::EffectIntentConflict)
    ));
    drop(tx);
    finish(&fixture, a, &ar).await;
    assert_eq!(
        readiness(&fixture, a, &ar, None).await,
        DispatchReadiness::Completed
    );
    fixture.close().await;
}

async fn wildcard_and_pending(fixture: IngressFixture) {
    let resource: FullJid = "juliet@example.com/one"
        .parse()
        .expect("valid recipient resource JID");
    let (a, ar) = message(&fixture, "opaque predecessor", &[]).await;
    let (b, br) = message(&fixture, "offline predecessor", &[]).await;
    let (c, cr) = message(&fixture, "live successor", std::slice::from_ref(&resource)).await;
    let pending = PendingRowId::fresh();
    register(&fixture, a, &ar, 1, &[DispatchTarget::ArchiveWide]).await;
    register(
        &fixture,
        b,
        &br,
        2,
        &[DispatchTarget::Pending(pending.clone())],
    )
    .await;
    register(
        &fixture,
        c,
        &cr,
        3,
        &[DispatchTarget::Resource(resource.clone())],
    )
    .await;
    assert_eq!(
        readiness(&fixture, c, &cr, Some(&resource)).await,
        DispatchReadiness::Blocked(vec![a, b])
    );
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin ingress transaction");
    assert_eq!(
        ArchiveDispatchRepository::readiness_pending(&mut tx, &resource, &pending, None)
            .await
            .expect("read pending dispatch readiness"),
        DispatchReadiness::Blocked(vec![a])
    );
    tx.commit().await.expect("commit ingress transaction");
    finish(&fixture, a, &ar).await;
    // No row yet and no receipt means enqueue may still happen: fail closed.
    assert_eq!(
        readiness(&fixture, c, &cr, Some(&resource)).await,
        DispatchReadiness::Blocked(vec![b])
    );
    let pending_store = crate::pending_delivery::DatabasePendingDeliveryStorage::from_database(
        fixture.db.clone(),
        waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
    )
    .await
    .expect("open pending delivery storage");
    let conn = fixture
        .db
        .guard()
        .await
        .expect("acquire fixture database connection");
    conn.execute("INSERT INTO pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind, archive_stanza_by, archive_stanza_id) VALUES (?, ?, ?, ?, ?, ?)", crate::db_params![pending.as_str(), resource.to_bare().to_string(), 1_i64, "archived", resource.to_bare().to_string(), "archived-message"]).await.expect("insert archived pending delivery fixture");
    drop(conn);
    finish(&fixture, b, &br).await;
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin ingress transaction");
    CanonicalMessageRepository::terminalize(
        &mut tx,
        b,
        chrono::Utc::now() - chrono::Duration::days(9),
    )
    .await
    .expect("terminalize pending predecessor");
    tx.commit().await.expect("commit ingress transaction");
    assert_eq!(
        collect(&fixture).await,
        0,
        "pending row retains canonical ordering authority after receipt and retention"
    );
    assert_eq!(
        readiness(&fixture, c, &cr, Some(&resource)).await,
        DispatchReadiness::Blocked(vec![b])
    );
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin ingress transaction");
    assert_eq!(
        ArchiveDispatchRepository::readiness_pending(&mut tx, &resource, &pending, None)
            .await
            .expect("read pending dispatch readiness"),
        DispatchReadiness::Ready,
        "flush ignores its own completed enqueue obligation"
    );
    tx.commit().await.expect("commit ingress transaction");
    use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
    let stream = waddle_xmpp::pending_delivery::SmSessionId::new("pending-live");
    pending_store
        .claim_batch_for_session(&resource.to_bare(), &stream, None, 1)
        .await
        .expect("claim pending row for active stream");
    pending_store
        .record_pushed_at(&pending, 1)
        .await
        .expect("record pending row stream position");
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin ingress transaction");
    assert_eq!(
        ArchiveDispatchRepository::readiness(&mut tx, c, &cr, Some(&resource), Some(&stream))
            .await
            .expect("read archive dispatch readiness"),
        DispatchReadiness::Ready,
        "a counted copy already precedes this stream's successor without a client ack"
    );
    let other_stream = waddle_xmpp::pending_delivery::SmSessionId::new("replacement");
    assert_eq!(
        ArchiveDispatchRepository::readiness(&mut tx, c, &cr, Some(&resource), Some(&other_stream))
            .await
            .expect("read archive dispatch readiness"),
        DispatchReadiness::Blocked(vec![b]),
        "the exemption cannot cross sessions"
    );
    tx.commit().await.expect("commit ingress transaction");
    pending_store
        .delete_row(&pending)
        .await
        .expect("delete pending predecessor");
    assert_eq!(
        collect(&fixture).await,
        1,
        "deleting pending row makes retained authority collectible"
    );
    assert_eq!(
        readiness(&fixture, c, &cr, Some(&resource)).await,
        DispatchReadiness::Ready
    );
    fixture.close().await;
}

async fn pending_resource_order(fixture: IngressFixture) {
    let old: FullJid = "juliet@example.com/old"
        .parse()
        .expect("valid old recipient resource JID");
    let new: FullJid = "juliet@example.com/new"
        .parse()
        .expect("valid new recipient resource JID");
    let archive = new.to_bare();
    let (a, ar) = message(&fixture, "earlier old resource", std::slice::from_ref(&old)).await;
    let (b, br) = message(&fixture, "pending flush", &[]).await;
    let (c, cr) = message(&fixture, "later pending flush", &[]).await;
    let pending = PendingRowId::fresh();
    let later = PendingRowId::fresh();
    register(
        &fixture,
        a,
        &ar,
        1,
        &[DispatchTarget::Resource(old.clone())],
    )
    .await;
    register(
        &fixture,
        b,
        &br,
        2,
        &[DispatchTarget::Pending(pending.clone())],
    )
    .await;
    register(
        &fixture,
        c,
        &cr,
        3,
        &[DispatchTarget::Pending(later.clone())],
    )
    .await;
    let store = crate::pending_delivery::DatabasePendingDeliveryStorage::from_database(
        fixture.db.clone(),
        waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
    )
    .await
    .expect("open pending delivery storage");
    let conn = fixture
        .db
        .guard()
        .await
        .expect("acquire fixture database connection");
    for (row, stamp) in [(&pending, "pending-resource"), (&later, "later-resource")] {
        conn.execute("INSERT INTO pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind, archive_stanza_by, archive_stanza_id) VALUES (?, ?, ?, ?, ?, ?)", crate::db_params![row.as_str(), archive.to_string(), 1_i64, "archived", archive.to_string(), stamp]).await.expect("insert archived pending delivery fixture");
    }
    drop(conn);
    finish(&fixture, b, &br).await;
    finish(&fixture, c, &cr).await;
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin ingress transaction");
    assert_eq!(
        ArchiveDispatchRepository::readiness_pending(&mut tx, &new, &pending, None)
            .await
            .expect("read pending dispatch readiness"),
        DispatchReadiness::Ready,
        "an earlier live copy owed only to /old cannot block a pending flush to /new"
    );
    assert_eq!(
        ArchiveDispatchRepository::readiness_pending(&mut tx, &old, &pending, None)
            .await
            .expect("read pending dispatch readiness"),
        DispatchReadiness::Blocked(vec![a]),
        "the earlier live copy still blocks a pending flush to its own resource"
    );
    assert_eq!(
        ArchiveDispatchRepository::readiness_pending(&mut tx, &new, &later, None)
            .await
            .expect("read pending dispatch readiness"),
        DispatchReadiness::Blocked(vec![b]),
        "an earlier pending row remains a barrier for every resource"
    );
    tx.commit().await.expect("commit ingress transaction");
    use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
    store
        .delete_row(&pending)
        .await
        .expect("delete pending predecessor");
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin ingress transaction");
    assert_eq!(
        ArchiveDispatchRepository::readiness_pending(&mut tx, &new, &later, None)
            .await
            .expect("read pending dispatch readiness"),
        DispatchReadiness::Ready,
        "removing the pending predecessor releases /new while /old remains unfinished"
    );
    tx.commit().await.expect("commit ingress transaction");
    fixture.close().await;
}

async fn independent_pending(fixture: IngressFixture) {
    use waddle_xmpp::{
        mam::{ArchivedMessage, MamStorage, SqlxMamStorage},
        pending_delivery::{storage::PendingDeliveryStorage, QuotaPolicy},
    };
    let resource: FullJid = "juliet@example.com/one"
        .parse()
        .expect("valid recipient resource JID");
    let archive = resource.to_bare();
    let mam = SqlxMamStorage::open(fixture.db.database_url())
        .await
        .expect("open message archive storage");
    for id in ["promoted-first", "promoted-second"] {
        let mut archived = ArchivedMessage::for_test(
            "romeo@example.com/phone"
                .parse()
                .expect("valid sender resource JID"),
            archive.clone().into(),
        );
        archived.id = id.to_owned();
        archived.stanza_id = Some(waddle_xmpp_core::xep0359::StanzaId::new(
            id,
            archive.clone().into(),
        ));
        mam.store_message(&archive, &archived)
            .await
            .expect("store archived message fixture");
    }
    let store = crate::pending_delivery::DatabasePendingDeliveryStorage::from_database(
        fixture.db.clone(),
        QuotaPolicy::Unlimited,
    )
    .await
    .expect("open pending delivery storage");
    let earlier = PendingRowId::fresh();
    let later = PendingRowId::fresh();
    let conn = fixture
        .db
        .guard()
        .await
        .expect("acquire fixture database connection");
    for (row, stamp) in [(&earlier, "promoted-first"), (&later, "promoted-second")] {
        conn.execute("INSERT INTO pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind, archive_stanza_by, archive_stanza_id) VALUES (?, ?, ?, ?, ?, ?)", crate::db_params![row.as_str(), archive.to_string(), 1_i64, "archived", archive.to_string(), stamp]).await.expect("insert archived pending delivery fixture");
    }
    drop(conn);
    let (b, br) = message(
        &fixture,
        "live-after-promotion",
        std::slice::from_ref(&resource),
    )
    .await;
    register(
        &fixture,
        b,
        &br,
        2,
        &[DispatchTarget::Resource(resource.clone())],
    )
    .await;
    assert_eq!(
        readiness(&fixture, b, &br, Some(&resource)).await,
        DispatchReadiness::Blocked(vec![]),
        "a promoted copy has no canonical key but still blocks live delivery"
    );
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin ingress transaction");
    assert_eq!(
        ArchiveDispatchRepository::readiness_pending(&mut tx, &resource, &earlier, None)
            .await
            .expect("read pending dispatch readiness"),
        DispatchReadiness::Ready
    );
    assert_eq!(
        ArchiveDispatchRepository::readiness_pending(&mut tx, &resource, &later, None)
            .await
            .expect("read pending dispatch readiness"),
        DispatchReadiness::Blocked(vec![])
    );
    tx.commit().await.expect("commit ingress transaction");
    store
        .delete_row(&earlier)
        .await
        .expect("delete earlier promoted pending row");
    assert_eq!(
        readiness(&fixture, b, &br, Some(&resource)).await,
        DispatchReadiness::Ready,
        "a same-ordinal pending copy is not a predecessor"
    );
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin ingress transaction");
    assert_eq!(
        ArchiveDispatchRepository::readiness_pending(&mut tx, &resource, &later, None)
            .await
            .expect("read pending dispatch readiness"),
        DispatchReadiness::Ready
    );
    tx.commit().await.expect("commit ingress transaction");
    fixture.close().await;
}

async fn collect(fixture: &IngressFixture) -> usize {
    crate::ingress_substrate::gc_expired_aliases(
        &fixture.db,
        chrono::Utc::now(),
        crate::ingress_substrate::AliasGcBudget {
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(10),
            lock_timeout: std::time::Duration::from_secs(1),
            statement_timeout: std::time::Duration::from_secs(2),
            scan_timeout: std::time::Duration::from_secs(2),
            progress: Default::default(),
        },
    )
    .await
    .expect("collect expired ingress aliases")
    .deleted_messages
}

#[tokio::test]
async fn sqlite_archive_dispatch_resource_order() {
    resource_order(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_archive_dispatch_resource_order() {
    if let Some(fixture) = IngressFixture::postgres("dispatch").await {
        resource_order(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_archive_dispatch_pending_barrier() {
    wildcard_and_pending(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_archive_dispatch_pending_barrier() {
    if let Some(fixture) = IngressFixture::postgres("pending").await {
        wildcard_and_pending(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_archive_dispatch_promoted_pending() {
    independent_pending(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_archive_dispatch_promoted_pending() {
    if let Some(fixture) = IngressFixture::postgres("promoted").await {
        independent_pending(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_archive_dispatch_pending_resource_order() {
    pending_resource_order(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_archive_dispatch_pending_resource_order() {
    if let Some(fixture) = IngressFixture::postgres("pending_resource").await {
        pending_resource_order(fixture).await;
    }
}
