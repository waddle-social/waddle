//! XEP-0237: Roster Versioning over WebSocket C2S.

mod ws_common;

use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";

async fn connect_alice() -> (TestServer, WsXmppClient) {
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_password)]);
    let alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("alice-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("alice connection");
    (server, alice)
}

async fn connect_named(
    server: &TestServer,
    user: &str,
    password: &str,
    resource: &str,
) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, user, password, resource)
        .await
        .unwrap_or_else(|e| panic!("connect {user}/{resource}: {e}"))
}

async fn roster_get(client: &mut WsXmppClient, id: &str, ver: Option<&str>) -> String {
    let ver_attr = ver
        .map(|ver| format!(r#" ver="{ver}""#))
        .unwrap_or_default();
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="{id}"><query xmlns="jabber:iq:roster"{ver_attr}/></iq>"#
        ))
        .await
        .expect("send roster get");
    let id_attr = format!(r#"id='{id}'"#);
    client
        .recv_matching(|frame| frame.contains(&id_attr))
        .await
        .expect("roster get result")
}

/// Send a roster-set adding `contact_jid`, then collect frames from the wire
/// up to and including the IQ result (which arrives after any roster push to
/// the originating resource — see `handle_roster_set`). Returns
/// (push_frames, result_frame).
async fn roster_set_add(
    client: &mut WsXmppClient,
    id: &str,
    contact_jid: &str,
    name: &str,
) -> (Vec<String>, String) {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="set" id="{id}"><query xmlns="jabber:iq:roster"><item jid="{contact_jid}" name="{name}"/></query></iq>"#
        ))
        .await
        .expect("send roster set");
    let id_attr = format!(r#"id='{id}'"#);
    let mut frames = client
        .recv_until(|f| f.contains(&id_attr) && f.contains("type='result'"))
        .await
        .expect("roster set result");
    let result = frames.pop().expect("at least the result frame");
    (frames, result)
}

fn roster_version(frame: &str) -> String {
    let (marker, terminator) = if frame.contains("ver=\"") {
        ("ver=\"", '"')
    } else if frame.contains("ver='") {
        ("ver='", '\'')
    } else {
        panic!("missing roster version: {frame}");
    };
    let start = frame.find(marker).expect("marker present") + marker.len();
    let end = frame[start..]
        .find(terminator)
        .unwrap_or_else(|| panic!("unterminated roster version: {frame}"));
    frame[start..start + end].to_string()
}

#[tokio::test]
async fn xep0237_first_sync_returns_full_roster_with_ver() {
    // T1: Absent inbound (no `ver` attribute). Server pre-condition: alice has at
    // least one contact in her roster, seeded by a prior roster-set on the same
    // session (we use roster-set rather than fixture because the server is
    // freshly provisioned per test). The first roster get with no `ver` MUST
    // come back as <iq type='result'> with a <query> payload that carries items
    // and a `ver` attribute (XEP-0237 §2.6).
    let (_server, mut alice) = connect_alice().await;

    // Seed the roster — `roster_set_add` collects the roster push that arrives
    // before the result frame, so subsequent reads start clean.
    let _ = roster_set_add(&mut alice, "xep237-t1-seed", "bob@localhost", "Bob").await;

    let first_sync = roster_get(&mut alice, "xep237-t1-first", None).await;
    assert!(
        first_sync.contains("type='result'"),
        "first sync response must be type=result: {first_sync}"
    );
    assert!(
        first_sync.contains("jabber:iq:roster"),
        "first sync must include the <query> payload: {first_sync}"
    );
    assert!(
        first_sync.contains("bob@localhost"),
        "first sync must include the seeded item: {first_sync}"
    );
    assert!(
        first_sync.contains("ver='"),
        "first sync result must carry a `ver` attribute: {first_sync}"
    );

    let _ = alice.close().await;
}

#[tokio::test]
async fn xep0237_matching_version_returns_empty_roster_result() {
    let (_server, mut alice) = connect_alice().await;

    let initial = roster_get(&mut alice, "xep237-initial", None).await;
    let version = roster_version(&initial);

    let unchanged = roster_get(&mut alice, "xep237-unchanged", Some(&version)).await;
    assert!(
        unchanged.contains("type='result'"),
        "expected empty roster result: {unchanged}"
    );
    assert!(
        !unchanged.contains("jabber:iq:roster") && !unchanged.contains("<item"),
        "matching ver must not include unchanged roster payload: {unchanged}"
    );

    let _ = alice.close().await;
}

#[tokio::test]
async fn xep0237_stale_version_returns_full_updated_roster() {
    // T3: Capture V1 from an empty-roster first sync, mutate the roster, query
    // with the now-stale V1 and assert the response carries the updated roster
    // and a fresh ver V2 ≠ V1.
    let (_server, mut alice) = connect_alice().await;

    let initial = roster_get(&mut alice, "xep237-t3-initial", None).await;
    let v1 = roster_version(&initial);

    let _ = roster_set_add(&mut alice, "xep237-t3-set", "carol@localhost", "Carol").await;
    let stale = roster_get(&mut alice, "xep237-t3-stale", Some(&v1)).await;
    assert!(
        stale.contains("jabber:iq:roster"),
        "stale-ver response must include the full roster: {stale}"
    );
    assert!(
        stale.contains("carol@localhost"),
        "stale-ver response must include the new item: {stale}"
    );
    let v2 = roster_version(&stale);
    assert_ne!(v1, v2, "stale-ver response must advance the version");

    let _ = alice.close().await;
}

#[tokio::test]
async fn xep0237_mutation_advances_version_and_push_carries_new_ver() {
    // T4: Two resources r1 + r2 both roster-interested. r1 issues a roster set;
    // both resources receive a roster push whose <query> carries the new ver,
    // and the IQ result back to r1 is bare <iq type='result'/> (RFC 6121 — no
    // ver on the result of a roster set). The pushed ver must differ from any
    // pre-mutation ver.
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_password)]);

    let mut r1 = connect_named(&server, "alice", &alice_password, "r1").await;
    let mut r2 = connect_named(&server, "alice", &alice_password, "r2").await;

    // Both resources must be roster-interested for pushes to fan out — that's
    // what the roster get triggers.
    let initial_r1 = roster_get(&mut r1, "xep237-t4-r1-initial", None).await;
    let initial_r2 = roster_get(&mut r2, "xep237-t4-r2-initial", None).await;
    let pre_ver = roster_version(&initial_r1);
    let _ = roster_version(&initial_r2);

    // r1 mutates. The push to r1 arrives before the IQ result on r1's stream.
    let (r1_pushes, r1_result) =
        roster_set_add(&mut r1, "xep237-t4-set", "dan@localhost", "Dan").await;
    assert!(
        !r1_result.contains("<query"),
        "RFC 6121: roster-set result must be bare <iq type='result'/>: {r1_result}"
    );
    let r1_push = r1_pushes
        .into_iter()
        .find(|f| f.contains("type='set'") && f.contains("jabber:iq:roster"))
        .expect("r1 must receive its own roster push");

    // r2 only ever sees the push (no IQ result is addressed to it).
    let r2_push = r2
        .recv_matching(|f| f.contains("type='set'") && f.contains("jabber:iq:roster"))
        .await
        .expect("r2 push");

    let v_push_r1 = roster_version(&r1_push);
    let v_push_r2 = roster_version(&r2_push);
    assert_eq!(
        v_push_r1, v_push_r2,
        "both pushes for the same mutation must carry the same ver: r1={v_push_r1} r2={v_push_r2}"
    );
    assert_ne!(
        v_push_r1, pre_ver,
        "post-mutation ver must differ from the pre-mutation ver"
    );
    assert!(
        r1_push.contains("dan@localhost") && r2_push.contains("dan@localhost"),
        "pushes must carry the mutated item"
    );

    let _ = r1.close().await;
    let _ = r2.close().await;
}

#[tokio::test]
async fn xep0237_version_persists_across_server_restart() {
    // T5: Issue's AC requires version state to survive server restart. We point
    // the server at a SQLite file in a temp dir, do a mutation, capture V1,
    // drop the server, restart against the same file, and assert that querying
    // with V1 returns an empty result — i.e. the server still recognises V1 as
    // current.
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("waddle-test-roster.sqlite");
    let database_url = format!("sqlite://{}?mode=rwc", db_path.display());

    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());

    let v1 = {
        let server = TestServer::start_persistent_with_extra_accounts(
            &database_url,
            &[("alice", &alice_password)],
        );
        let mut alice = connect_named(&server, "alice", &alice_password, "r1").await;

        let _ = roster_set_add(&mut alice, "xep237-t5-set", "ed@localhost", "Ed").await;
        let frame = roster_get(&mut alice, "xep237-t5-snapshot", None).await;
        let v = roster_version(&frame);
        let _ = alice.close().await;
        v
    };
    // server dropped here — subprocess killed, file remains.

    let server2 = TestServer::start_persistent_with_extra_accounts(
        &database_url,
        &[("alice", &alice_password)],
    );
    let mut alice2 = connect_named(&server2, "alice", &alice_password, "r2").await;

    let frame = roster_get(&mut alice2, "xep237-t5-after-restart", Some(&v1)).await;
    assert!(
        frame.contains("type='result'"),
        "restarted server must accept the persisted ver: {frame}"
    );
    assert!(
        !frame.contains("<item"),
        "matching persisted ver must yield empty result: {frame}"
    );

    let _ = alice2.close().await;
}
