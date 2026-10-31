//! XEP-0045 §7.4: a blocked remote occupant must not stall a ready local
//! occupant's copy. Clustered production retains keyed legacy settlement, so
//! recovery completes the remote obligation and terminalizes the row.

use super::{
    cluster_e2e_serial_lock, free_tcp_port, generate_pool, open_control_db,
    partial_room_fanout_completes_on_retransmission_after_relay_recovery, register_test_swarm_node,
    relay_for_live_peer, reset_and_enroll, spawn_cluster_server, swarm, CancellationToken,
    ClusteringBootstrapConfig, ClusteringConfig, ClusteringLeaseConfig, Duration,
};

const CHILD_PROCESS: &str = "WADDLE_TEST_PARTIAL_FANOUT_CHILD";
const TEST_NAME: &str =
    "xep0045_partial_fanout::local_copy_precedes_blocked_remote_and_recovery_terminalizes";

#[tokio::test(flavor = "multi_thread")]
async fn local_copy_precedes_blocked_remote_and_recovery_terminalizes() {
    let Some(postgres_url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping XEP-0045 partial fanout: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let _serial = cluster_e2e_serial_lock().lock().await;
    if std::env::var_os(CHILD_PROCESS).is_none() {
        // kameo's global swarm can only initialize once per process. The
        // existing exit-criteria test already uses it, so isolate this mesh
        // while retaining the parent's shared-database serialization lock.
        let result =
            tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                .args([TEST_NAME, "--exact", "--nocapture"])
                .env(CHILD_PROCESS, "1")
                .kill_on_drop(true)
                .output()
                .await
                .expect("spawn isolated partial-fanout test");
        assert!(
            result.status.success(),
            "isolated partial-fanout test failed: {}\n{}\n{}",
            result.status,
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }

    let pool = generate_pool();
    let db = open_control_db(&postgres_url).await;
    reset_and_enroll(&db, &pool).await;
    let port_a = free_tcp_port();
    let port_b = free_tcp_port();
    let (server_a, _startup_node_a, peer_a) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_a, &[port_b]).await;
    let (server_b, _startup_node_b, peer_b) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_b, &[port_a]).await;

    let stop = CancellationToken::new();
    let config = ClusteringConfig {
        enabled: true,
        listen_addrs: vec!["/ip4/127.0.0.1/tcp/0".to_string()],
        bootstrap_peers: [port_a, port_b]
            .into_iter()
            .map(|port| ClusteringBootstrapConfig {
                dns_name: "localhost".to_string(),
                port,
            })
            .collect(),
        keypair_pool: pool.pool_env.split(',').map(str::to_string).collect(),
        lease: ClusteringLeaseConfig {
            heartbeat_interval: Duration::from_secs(1),
            lease_ttl: Duration::from_secs(10),
        },
        allowlist_refresh_interval: Duration::from_secs(1),
        dial_interval: Duration::from_secs(1),
        messaging: waddle_server::config::ClusteringMessagingConfig {
            request_timeout: Duration::from_secs(2),
            reply_timeout: Duration::from_millis(1_500),
            mailbox_timeout: Duration::from_millis(500),
            ..Default::default()
        },
        // Match the subprocess node-lease timings: their watchdog expires
        // every stale node using the same cluster-wide TTL.
        node_lease: waddle_server::config::ClusteringNodeLeaseConfig {
            heartbeat_interval: Duration::from_millis(300),
            lease_ttl: Duration::from_millis(1200),
            claim_release_budget: Duration::from_millis(300),
        },
        ..Default::default()
    };
    let handle = swarm::spawn(
        &config,
        &db,
        stop.clone(),
        swarm::RelayBridges {
            resume_bridge: waddle_server::clustering::resume_bridge::ResumeStealBridge::new(),
            room_local_claims: waddle_server::clustering::local_claims::RoomLocalClaims::new(),
            ordered_relay_delivery_bridge:
                waddle_server::clustering::route_bridge::OrderedRelayDeliveryBridge::new(
                    stop.clone(),
                    &config.messaging,
                ),
        },
    )
    .await
    .expect("isolated test swarm joins mesh");
    let node_lease = register_test_swarm_node(&db, &handle, &config, stop.clone()).await;
    let (node_a, _relay_a) =
        relay_for_live_peer(&db, &peer_a, stop.clone(), Duration::from_secs(45)).await;
    let (node_b, mut relay_b) =
        relay_for_live_peer(&db, &peer_b, stop.clone(), Duration::from_secs(45)).await;
    partial_room_fanout_completes_on_retransmission_after_relay_recovery(
        &db,
        &server_a,
        &server_b,
        &mut relay_b,
        &node_a,
        &node_b,
    )
    .await;
    stop.cancel();
    node_lease.await.expect("test-process node lease stops");
}
