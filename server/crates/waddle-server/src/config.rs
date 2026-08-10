//! Server configuration.

use crate::auth::providers::AuthProviderConfig;
use crate::db::DatabaseDriver;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use std::{fmt, str::FromStr};
use tracing::info;
use waddle_extensions::ExtensionConfig;
use waddle_xmpp::xep::xep0421::{OccupantIdSecret, OCCUPANT_ID_SECRET_MIN_BYTES};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ServerMode {
    /// Full server mode with HTTP auth broker + XMPP.
    #[default]
    HomeServer,
    /// Standalone XMPP-focused mode.
    Standalone,
}

impl fmt::Display for ServerMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerMode::HomeServer => write!(f, "HomeServer"),
            ServerMode::Standalone => write!(f, "Standalone"),
        }
    }
}

impl ServerMode {
    pub fn auth_broker_allowed(&self) -> bool {
        matches!(self, ServerMode::HomeServer)
    }
}

impl FromStr for ServerMode {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "standalone" | "xmpp" | "xmpp-only" => ServerMode::Standalone,
            _ => ServerMode::HomeServer,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct AuthConfig {
    pub providers: Vec<AuthProviderConfig>,
}

impl AuthConfig {
    pub fn from_env() -> Result<Self, String> {
        let raw = std::env::var("WADDLE_AUTH_PROVIDERS_JSON").unwrap_or_else(|_| "[]".to_string());
        let trimmed = raw.trim();

        let providers = if trimmed.starts_with('[') {
            serde_json::from_str::<Vec<AuthProviderConfig>>(trimmed)
                .map_err(|e| format!("invalid WADDLE_AUTH_PROVIDERS_JSON array: {}", e))?
        } else {
            #[derive(Deserialize)]
            struct Wrapper {
                providers: Vec<AuthProviderConfig>,
            }
            serde_json::from_str::<Wrapper>(trimmed)
                .map_err(|e| format!("invalid WADDLE_AUTH_PROVIDERS_JSON object: {}", e))?
                .providers
        };

        // Validation is strict and fails startup.
        let registry = crate::auth::ProviderRegistry::new(providers.clone())
            .map_err(|e| format!("invalid provider config: {}", e))?;

        if registry.is_empty() {
            info!("No auth providers configured");
        }

        Ok(Self { providers })
    }
}

/// SpiceDB backend configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpiceDbConfig {
    pub endpoint: String,
    pub preshared_key: String,
    pub insecure: bool,
}

impl SpiceDbConfig {
    pub fn from_env() -> Result<Option<Self>, String> {
        let endpoint = std::env::var("WADDLE_SPICEDB_ENDPOINT")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let preshared_key = std::env::var("WADDLE_SPICEDB_PRESHARED_KEY")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        match (endpoint, preshared_key) {
            (None, None) => Ok(None),
            (Some(_), None) => Err(
                "WADDLE_SPICEDB_PRESHARED_KEY is required when WADDLE_SPICEDB_ENDPOINT is set"
                    .to_string(),
            ),
            (None, Some(_)) => Err(
                "WADDLE_SPICEDB_ENDPOINT is required when WADDLE_SPICEDB_PRESHARED_KEY is set"
                    .to_string(),
            ),
            (Some(endpoint), Some(preshared_key)) => {
                let insecure = std::env::var("WADDLE_SPICEDB_INSECURE")
                    .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
                    .unwrap_or(false);

                Ok(Some(Self {
                    endpoint,
                    preshared_key,
                    insecure,
                }))
            }
        }
    }
}

/// ADR-0017 Phase 2 clustering (owned libp2p swarm) configuration.
///
/// Parsed from `WADDLE_CLUSTERING_*`. With `enabled` false (the default) the
/// swarm subsystem never starts and server behaviour is byte-for-byte
/// identical to the single-replica path. Clustering additionally requires the
/// `clustering` build feature and the Postgres control plane; see
/// [`crate::clustering`]. This struct carries no libp2p types (multiaddrs are
/// strings, parsed inside the feature-gated swarm module) so it compiles into
/// every build.
#[derive(Clone, PartialEq, Eq)]
pub struct ClusteringConfig {
    pub enabled: bool,
    /// libp2p listen multiaddrs for the swarm transport. Default: one
    /// ephemeral TCP address (`/ip4/0.0.0.0/tcp/0`).
    pub listen_addrs: Vec<String>,
    /// Peer discovery seeds (`WADDLE_CLUSTERING_BOOTSTRAP_PEERS`, comma-
    /// separated `host:port`; bracket IPv6 literals as `[::1]:7900`). In
    /// production this is a single entry — the
    /// Kubernetes headless Service name, which resolves to every ready pod;
    /// the multi-process harness lists one loopback entry per node. Empty =
    /// cold start with no bootstrap peers (dialing retries continuously; an
    /// empty peer set is tolerated, avoiding cold-start deadlock).
    pub bootstrap_peers: Vec<ClusteringBootstrapConfig>,
    /// kameo `messaging::Config` limits and the ADR element-5 timeout
    /// hierarchy.
    pub messaging: ClusteringMessagingConfig,
    /// Pre-enrolled per-pod keypair pool: base64-encoded 32-byte ed25519 secret
    /// keys (`WADDLE_CLUSTERING_KEYPAIR_POOL`, comma-separated). At startup a
    /// pod leases exactly one pool slot via a Postgres CAS and uses that
    /// keypair as its libp2p identity (ADR element 3). Empty (the default)
    /// falls back to an ephemeral per-process keypair — fine for the discovery
    /// spike and tests, but no stable/revocable identity.
    pub keypair_pool: Vec<String>,
    /// Keypair-slot lease timing (heartbeat interval + lease TTL).
    pub lease: ClusteringLeaseConfig,
    /// How often the swarm re-reads the peer allowlist and revokes live
    /// connections whose PeerId is no longer enrolled
    /// (`WADDLE_CLUSTERING_ALLOWLIST_REFRESH_MS`). This interval is the
    /// containment bound for a revoked peer (ADR element 3).
    pub allowlist_refresh_interval: Duration,
    /// How often the swarm re-resolves the headless-DNS bootstrap name and
    /// dials seed peers, picking up pod churn
    /// (`WADDLE_CLUSTERING_DIAL_INTERVAL_MS`).
    pub dial_interval: Duration,
    /// Cadence for the orphan reaper's `sm_session` claim sweep (ADR-0017
    /// Phase 3 Slice 5, element 9) — `WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS`.
    /// The only cluster timer that, prior to ADR-0017 Phase 3 Slice 11's
    /// corrigenda (deviation 111), had no env override at all (every
    /// sibling timer above and below this field does); added so the
    /// multi-process harness's kill-one hydration capstone
    /// (`clustering_cluster_e2e.rs::orphan_reaper_kills_one_node_and_hydrates_only_its_orphaned_sessions`)
    /// does not have to wait out the full production default in real
    /// wall-clock time.
    pub orphan_reaper_interval: Duration,
    /// Enable the relay's fault-injection message set (crash/sleep) for the
    /// multi-process cluster harness (`WADDLE_CLUSTERING_FAULT_INJECTION`).
    /// Never enable in production: it lets any enrolled peer stop this node's
    /// relay or hold its mailbox.
    pub fault_injection: bool,
    /// Write `"<node_id> <peer_id>\n"` to this path once the swarm is up
    /// (`WADDLE_CLUSTERING_NODE_ID_FILE`) so the harness can resolve this
    /// node's relay name — mirrors the `WADDLE_HTTP_PORT_FILE` convention.
    pub node_id_file: Option<std::path::PathBuf>,
    /// Node-lease (`clustering_nodes`) heartbeat/TTL timing (ADR-0017 Phase 3
    /// Slice 2, Q6): a **second**, conceptually distinct lease from
    /// `lease` above — that one guards a leased keypair-pool slot (libp2p
    /// identity); this one guards this node's entity-ownership claims. Q6
    /// rejected a `nodes`-table config row (bootstrap chicken-and-egg) in
    /// favor of the same env-var mechanism Phase 2 already established.
    pub node_lease: ClusteringNodeLeaseConfig,
    /// Isolation-fencing + re-registration hysteresis timing (ADR-0017 Phase
    /// 3 Slice 2, element 4's "N=2 lone-survivor carve-out" and
    /// re-registration hysteresis text).
    pub self_fence: ClusteringSelfFenceConfig,
    /// Steal-intent unwedge/owner-veto timing (ADR-0017 Phase 3 Slice 3,
    /// element 4's "Unwedge" text): how long an uncleared steal-intent row
    /// must age before `StalePredicate::StealIntentExpired` treats the
    /// entity as stealable — "a small multiple of the heartbeat interval"
    /// per the ADR. Named `steal_intent` (not bundled into `node_lease` or
    /// `self_fence`) because it is a third, conceptually distinct timing
    /// value from either: it gates the steal-intent CAS, not node-lease
    /// renewal or isolation fencing. No production call site issues
    /// `StalePredicate::StealIntentExpired` this slice (Slice 3 lands the
    /// mechanism; the cross-node reporter that would call it is Slice 5+
    /// scope) — parsed and validated now so the mechanism and its config
    /// surface land together, mirroring `node_lease`/`self_fence`'s own
    /// Slice 2 precedent.
    pub steal_intent: ClusteringStealIntentConfig,
    /// ADR-0017 Phase 3 Slice 6: bound on the cross-node XEP-0198 resume
    /// live-handshake's held-response retry window (element 8's
    /// owner-unreachable branch — "hold the `<resume/>` response ... retry
    /// the handshake with backoff, capped at `min(remaining lease TTL,
    /// resume-handshake timeout)`"). See
    /// [`ClusteringResumeHandshakeConfig`]'s own doc for the `min(...)`
    /// simplification this config validates at parse time rather than
    /// computing per-request.
    pub resume_handshake: ClusteringResumeHandshakeConfig,
    /// This pod's `pod-template-hash` label, read once at startup from the
    /// Kubernetes downward API (`WADDLE_CLUSTERING_POD_TEMPLATE_HASH`) and
    /// stamped onto this node's `clustering_nodes` row (Q5/element 4/Slice
    /// 2): identifies which deployment generation this node belongs to, for
    /// the rollout-aware claim-acquisition backoff rule (Slice 10). `None`
    /// outside Kubernetes (e.g. the multi-process harness, or a Deployment
    /// that omits the downward-API env var) — every acquire-backoff site
    /// treats a missing hash as "no generation to compare," never a parse
    /// failure. Parsed the same way as every other `ClusteringConfig` string
    /// field: trimmed, empty ⇒ `None`.
    pub pod_template_hash: Option<String>,
}

/// Manual `Debug`: `keypair_pool` holds base64-encoded ed25519 secret-key
/// seeds, and `ClusteringConfig` is embedded in `ServerConfig`, so a derived
/// impl would print raw private key material into any `{:?}` sink (panic
/// handlers, error logs, `--nocapture` test output). The pool is redacted to
/// a count; every other field formats as the derive would.
impl std::fmt::Debug for ClusteringConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusteringConfig")
            .field("enabled", &self.enabled)
            .field("listen_addrs", &self.listen_addrs)
            .field("bootstrap_peers", &self.bootstrap_peers)
            .field("messaging", &self.messaging)
            .field(
                "keypair_pool",
                &format_args!("[{} keys redacted]", self.keypair_pool.len()),
            )
            .field("lease", &self.lease)
            .field(
                "allowlist_refresh_interval",
                &self.allowlist_refresh_interval,
            )
            .field("dial_interval", &self.dial_interval)
            .field("orphan_reaper_interval", &self.orphan_reaper_interval)
            .field("fault_injection", &self.fault_injection)
            .field("node_id_file", &self.node_id_file)
            .field("node_lease", &self.node_lease)
            .field("self_fence", &self.self_fence)
            .field("steal_intent", &self.steal_intent)
            .field("resume_handshake", &self.resume_handshake)
            .field("pod_template_hash", &self.pod_template_hash)
            .finish()
    }
}

/// Timing for the keypair-slot lease heartbeat and expiry (ADR element 4
/// shape). `lease_ttl` must exceed `heartbeat_interval` with margin so a
/// briefly-delayed renewal does not lose the slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusteringLeaseConfig {
    pub heartbeat_interval: Duration,
    pub lease_ttl: Duration,
}

impl Default for ClusteringLeaseConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(10),
            lease_ttl: Duration::from_secs(30),
        }
    }
}

/// Timing for the node-lease (`clustering_nodes`) heartbeat/expiry CAS
/// (ADR-0017 element 4). Same shape and same `lease_ttl >=
/// heartbeat_interval * 2` invariant as [`ClusteringLeaseConfig`], but a
/// distinct value: this TTL bounds the node's entity-ownership claims, not
/// its keypair-slot identity (Q6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusteringNodeLeaseConfig {
    pub heartbeat_interval: Duration,
    pub lease_ttl: Duration,
    /// ADR-0017 Phase 3 Slice 10: the graceful-drain time budget for this
    /// node's per-entity claim release sequence
    /// (`clustering::drain::run_shutdown_drain`) — named `claimReleaseBudget`
    /// in the ADR's own Implementation Plan text ("already named as a
    /// chart value consumed from Phase 3 on"). An entity whose final fenced
    /// write has not committed within this budget is left claimed
    /// (abandoned, not released) rather than blocking shutdown indefinitely
    /// — fenced-safe, since an un-released claim is simply reclaimed later
    /// by another node's orphan reaper. Composes with (does not replace)
    /// the existing `WADDLE_DRAIN_TIMEOUT_SECS` SM-session Q6 drain budget
    /// (`session_janitors::max_drain_duration_from_env`): the two run on
    /// independent tasks racing the same shutdown token, each bounding a
    /// disjoint entity-type slice of the overall drain. Surfaced through
    /// Helm's `terminationGracePeriodSeconds` formula alongside
    /// `preStopSleepSeconds`/`config.drainTimeoutSeconds`, per the ADR text.
    ///
    /// **FIX 2 (council-adjudicated)**: `clustering::self_fence::
    /// run_shutdown_drain_with_heartbeat` keeps renewing this node's own
    /// `lease_ttl` heartbeat for this entire budget window rather than
    /// freezing it the instant shutdown fires — element 4's "stay live in
    /// `nodes` until finished draining" requirement. `from_vars`'s parse
    /// -time validation requires `lease_ttl` to comfortably exceed this
    /// value (a conservative 3x floor, mirroring `lease_ttl`'s own 2x
    /// floor against `heartbeat_interval`) so a slow/overrunning drain
    /// cannot outrun the margin that renewal buys. Operators should ALSO
    /// size `lease_ttl` with `WADDLE_DRAIN_TIMEOUT_SECS` (the separate,
    /// potentially much longer Q6 SM-session drain budget racing the same
    /// shutdown token) in mind — that value lives outside this typed
    /// config entirely, so it cannot be cross-validated here; see the
    /// Helm `terminationGracePeriodSeconds` formula, which already
    /// composes both budgets.
    pub claim_release_budget: Duration,
}

impl Default for ClusteringNodeLeaseConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(10),
            lease_ttl: Duration::from_secs(30),
            claim_release_budget: Duration::from_secs(5),
        }
    }
}

/// Isolation-fencing + re-registration hysteresis timing (ADR-0017 element
/// 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusteringSelfFenceConfig {
    /// M: consecutive heartbeat intervals of total swarm unreachability
    /// (with `clustering_nodes` showing >= 2 other live nodes — the N=2
    /// lone-survivor carve-out never fences on isolation alone) required
    /// before this node refuses to renew its node lease.
    pub isolation_intervals: u32,
    /// Initial delay before the first post-fence re-registration attempt;
    /// doubles on each subsequent failed/gated attempt up to
    /// `reregister_backoff_max`.
    pub reregister_backoff_base: Duration,
    /// Ceiling on the re-registration backoff delay.
    pub reregister_backoff_max: Duration,
}

impl Default for ClusteringSelfFenceConfig {
    fn default() -> Self {
        Self {
            isolation_intervals: 3,
            reregister_backoff_base: Duration::from_secs(1),
            reregister_backoff_max: Duration::from_secs(60),
        }
    }
}

/// Steal-intent unwedge/owner-veto timing (ADR-0017 Phase 3 Slice 3,
/// element 4). `intent_ttl` bounds a sick-but-heartbeating owner's hostage
/// window: an uncleared steal-intent row must age past this before it makes
/// the entity stealable via `StalePredicate::StealIntentExpired`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusteringStealIntentConfig {
    pub intent_ttl: Duration,
}

impl Default for ClusteringStealIntentConfig {
    fn default() -> Self {
        Self {
            // "A small multiple of the heartbeat interval" (ADR element 4)
            // — six times the default node-lease heartbeat interval (10s),
            // giving the owner's veto scan several chances to clear a
            // reported intent before it ages out.
            intent_ttl: Duration::from_secs(60),
        }
    }
}

/// ADR-0017 Phase 3 Slice 6: the cross-node XEP-0198 resume live-handshake's
/// held-response retry budget (element 8's owner-unreachable branch).
///
/// The ADR text caps the held-response retry window at
/// `min(remaining lease TTL, resume-handshake timeout)` — this deployment's
/// configured `timeout` here, and the *remaining* time before the owning
/// node's `clustering_nodes` lease naturally expires. This config
/// deliberately does not compute that `min(...)` per request (which would
/// need a live per-owner remaining-TTL read with no existing query to
/// serve it): instead, parse-time validation requires `timeout <=
/// node_lease.lease_ttl`, which makes the flat configured `timeout` an
/// upper bound that is never longer than a **fresh** lease's remaining TTL
/// — the only imprecision this simplification accepts is holding the
/// response slightly longer than the *shrinking* remaining TTL of an
/// owner whose lease is already partway expired, which is harmless: the
/// janitor-vs-resume ordering invariant (this module's own doc comment)
/// already guarantees a concurrent orphan-reaper steal is observed as a
/// clean CAS loss, never a corrupted read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusteringResumeHandshakeConfig {
    pub timeout: Duration,
}

impl Default for ClusteringResumeHandshakeConfig {
    fn default() -> Self {
        Self {
            // Comfortably inside a default 30s node-lease TTL, leaving
            // headroom for the reaper to win a genuinely dead owner's
            // claim before this window would have expired anyway.
            timeout: Duration::from_secs(20),
        }
    }
}

/// One peer-discovery seed: a DNS name or IP literal (A/AAAA-resolved to one
/// or more peer IPs — the headless Service resolves to every ready pod) plus
/// the TCP port those peers listen on for the swarm transport. IPv6 literals
/// are stored unbracketed (`::1`); the resolver re-brackets them for the
/// `host:port` lookup form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusteringBootstrapConfig {
    pub dns_name: String,
    pub port: u16,
}

/// Receiver-side cap for ordered-relay mailbox admission.
pub(crate) const ORDERED_RELAY_MAILBOX_TIMEOUT: Duration = Duration::from_secs(2);
/// Receiver-side cap for ordinary ordered-relay replies.
pub(crate) const ORDERED_RELAY_REPLY_TIMEOUT: Duration = Duration::from_secs(8);
/// A remote-owner registration drains at most one current-affecting pending
/// unregister before issuing its child register ask.
const REMOTE_OWNER_REGISTER_CHILD_OPERATION_COUNT: u32 = 2;
/// After the owner-local register succeeds, the handler still re-reads the
/// `UserActor` and validates the owner-gated `ConnectionEntry` before it can
/// safely reply `Registered`.
const REMOTE_OWNER_REGISTER_POST_REGISTRATION_ASK_COUNT: u32 = 2;
const REMOTE_OWNER_REGISTER_REPLY_TIMEOUT_MARGIN: Duration = Duration::from_millis(250);
/// Reply budget for the owner-local `UserRegistryActor` admission ask issued
/// from `register_remote_user_resource_on_owner_locked`.
/// Each nested child ask spends TWO bounded phases on a congested actor —
/// mailbox admission and the reply — each capped by `CHILD_ACTOR_TIMEOUT`.
const REMOTE_OWNER_REGISTER_CHILD_PHASES: u32 = 2;
pub(crate) const REMOTE_OWNER_REGISTER_USER_REGISTRY_REPLY_TIMEOUT: Duration =
    waddle_xmpp::registry::user_registry::CHILD_ACTOR_TIMEOUT
        .saturating_mul(REMOTE_OWNER_REGISTER_CHILD_OPERATION_COUNT)
        .saturating_mul(REMOTE_OWNER_REGISTER_CHILD_PHASES)
        .saturating_add(REMOTE_OWNER_REGISTER_REPLY_TIMEOUT_MARGIN);
/// Reply budget for the owner-local post-registration currentness checks:
/// `GetUser` on the registry plus `GetConnectionEntry` on the child actor.
pub(crate) const REMOTE_OWNER_REGISTER_POST_REGISTRATION_REPLY_TIMEOUT: Duration =
    ORDERED_RELAY_MAILBOX_TIMEOUT
        .saturating_mul(REMOTE_OWNER_REGISTER_POST_REGISTRATION_ASK_COUNT)
        .saturating_mul(2);
/// Reply floor for a remote-resource register relay ask. It must cover the
/// nested owner-local registry admission mailbox window (spent BEFORE that
/// ask's reply budget starts — the explicit first term below) plus that
/// admission ask's bounded reply budget AND the two bounded post-registration
/// asks that prove the mirrored resource is now current before replying
/// `Registered`. Dropping the admission term makes the sum expire before an
/// otherwise-bounded handler finishes whenever the registry mailbox consumes
/// part of its window, and the socket's idempotent retry budget collapses
/// with it.
///
/// This floor intentionally excludes displaced-mirror retirement work. That
/// pre-registration path runs under the same per-JID lock, but it cannot keep
/// stretching the fixed reply floor every time another bounded cleanup step is
/// discovered; owner-managed retirement must instead surface a prompt typed
/// transient (the same "Busy immediately, retry the idempotent register"
/// design the socket side already uses elsewhere).
pub(crate) const REMOTE_OWNER_REGISTER_REPLY_TIMEOUT: Duration = ORDERED_RELAY_MAILBOX_TIMEOUT
    .saturating_add(REMOTE_OWNER_REGISTER_USER_REGISTRY_REPLY_TIMEOUT)
    .saturating_add(REMOTE_OWNER_REGISTER_POST_REGISTRATION_REPLY_TIMEOUT);

/// kameo `messaging::Config` limits plus the ADR element-5 timeout hierarchy.
///
/// Invariants enforced at parse time: `reply_timeout <= request_timeout`,
/// `mailbox_timeout <= request_timeout`, and — because the mailbox and reply
/// phases run sequentially on the receiver — `mailbox_timeout + reply_timeout
/// <= request_timeout`. `request_timeout` is the sender-side
/// libp2p transport cap (`with_request_timeout`, applied at swarm build) and
/// is the binding bound; any `reply_timeout` above it is dead configuration.
/// `mailbox_timeout`/`reply_timeout` are per-ask parameters, applied by every
/// relay ask (`clustering::relay::RelayHandle`); Phase 4's delivery asks
/// inherit the same wiring. Remote-resource registration raises its relay
/// reply wait to [`REMOTE_OWNER_REGISTER_REPLY_TIMEOUT`] because the owner
/// handler performs a nested `UserRegistryActor` ask whose mailbox admission
/// alone can spend [`ORDERED_RELAY_MAILBOX_TIMEOUT`] before its own reply
/// budget starts, so that effective mailbox and reply combination must fit
/// under `request_timeout` as well. Displaced-mirror retirement is not part of
/// this fixed floor; that path must return a prompt retryable typed status
/// instead of holding the relay reply open across extra cleanup work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusteringMessagingConfig {
    pub request_timeout: Duration,
    pub reply_timeout: Duration,
    pub mailbox_timeout: Duration,
    /// Cap on concurrent asks per peer connection (kameo default 100).
    pub max_concurrent_streams: usize,
    /// Max serialized request envelope bytes (kameo default 1 MiB).
    pub max_request_bytes: u64,
    /// Max serialized response envelope bytes (kameo default 10 MiB).
    pub max_response_bytes: u64,
}

impl Default for ClusteringMessagingConfig {
    fn default() -> Self {
        Self {
            // Sized above the worst-case fenced-write / resume-handshake budget
            // (the 10s kameo default is too low per ADR element 5).
            request_timeout: Duration::from_secs(30),
            reply_timeout: Duration::from_secs(20),
            mailbox_timeout: Duration::from_secs(5),
            max_concurrent_streams: 256,
            max_request_bytes: 1024 * 1024,
            max_response_bytes: 10 * 1024 * 1024,
        }
    }
}

impl Default for ClusteringConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen_addrs: vec!["/ip4/0.0.0.0/tcp/0".to_string()],
            bootstrap_peers: Vec::new(),
            messaging: ClusteringMessagingConfig::default(),
            keypair_pool: Vec::new(),
            lease: ClusteringLeaseConfig::default(),
            allowlist_refresh_interval: Duration::from_secs(30),
            dial_interval: Duration::from_secs(15),
            orphan_reaper_interval: Duration::from_secs(120),
            fault_injection: false,
            node_id_file: None,
            node_lease: ClusteringNodeLeaseConfig::default(),
            self_fence: ClusteringSelfFenceConfig::default(),
            steal_intent: ClusteringStealIntentConfig::default(),
            resume_handshake: ClusteringResumeHandshakeConfig::default(),
            pod_template_hash: None,
        }
    }
}

impl ClusteringConfig {
    pub fn from_env() -> Result<Self, String> {
        Self::from_vars(std::env::vars())
    }

    pub fn from_vars<I, K, V>(vars: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let vars = vars
            .into_iter()
            .map(|(key, value)| (key.as_ref().to_string(), value.as_ref().to_string()))
            .collect::<std::collections::HashMap<_, _>>();

        let defaults = Self::default();
        let enabled = parse_bool_var(&vars, "WADDLE_CLUSTERING_ENABLED", false)?;

        let listen_addrs = match vars
            .get("WADDLE_CLUSTERING_LISTEN_ADDRS")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        {
            None => defaults.listen_addrs,
            Some(raw) => {
                let addrs: Vec<String> = raw
                    .split(',')
                    .map(|entry| entry.trim().to_string())
                    .filter(|entry| !entry.is_empty())
                    .collect();
                if addrs.is_empty() {
                    return Err(
                        "WADDLE_CLUSTERING_LISTEN_ADDRS must contain at least one multiaddr"
                            .to_string(),
                    );
                }
                addrs
            }
        };

        let bootstrap_peers = match vars
            .get("WADDLE_CLUSTERING_BOOTSTRAP_PEERS")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        {
            None => Vec::new(),
            Some(raw) => raw
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(|entry| {
                    // IPv6 literals must be bracketed (`[::1]:7900`): the
                    // resolver takes `host:port` strings, and an unbracketed
                    // IPv6 host is ambiguous with the port separator — it
                    // would parse "successfully" here and then never resolve,
                    // retrying forever. Fail fast instead. Brackets are
                    // stripped for storage; the resolver re-adds them.
                    let (dns_name, port) = if let Some(rest) = entry.strip_prefix('[') {
                        let (host, port) = rest.split_once("]:").ok_or_else(|| {
                            format!(
                                "WADDLE_CLUSTERING_BOOTSTRAP_PEERS entry '{entry}' must be \
                                 [ipv6]:port"
                            )
                        })?;
                        if host.parse::<std::net::Ipv6Addr>().is_err() {
                            return Err(format!(
                                "WADDLE_CLUSTERING_BOOTSTRAP_PEERS entry '{entry}' has an \
                                 invalid bracketed IPv6 literal"
                            ));
                        }
                        (host, port)
                    } else {
                        let (host, port) = entry.rsplit_once(':').ok_or_else(|| {
                            format!(
                                "WADDLE_CLUSTERING_BOOTSTRAP_PEERS entry '{entry}' must be \
                                 host:port"
                            )
                        })?;
                        if host.contains(':') {
                            return Err(format!(
                                "WADDLE_CLUSTERING_BOOTSTRAP_PEERS entry '{entry}': IPv6 \
                                 literals must be bracketed, e.g. [::1]:7900 — the \
                                 unbracketed form never resolves"
                            ));
                        }
                        (host, port)
                    };
                    let port = port
                        .parse::<u16>()
                        .ok()
                        .filter(|value| *value != 0)
                        .ok_or_else(|| {
                            format!(
                                "WADDLE_CLUSTERING_BOOTSTRAP_PEERS entry '{entry}' has an \
                                 invalid TCP port (1-65535)"
                            )
                        })?;
                    if dns_name.is_empty() {
                        return Err(format!(
                            "WADDLE_CLUSTERING_BOOTSTRAP_PEERS entry '{entry}' has an empty host"
                        ));
                    }
                    Ok(ClusteringBootstrapConfig {
                        dns_name: dns_name.to_string(),
                        port,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?,
        };

        let messaging_defaults = ClusteringMessagingConfig::default();
        let request_timeout = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS",
            millis_u64(messaging_defaults.request_timeout),
        )?);
        let reply_timeout = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_REPLY_TIMEOUT_MS",
            millis_u64(messaging_defaults.reply_timeout),
        )?);
        let mailbox_timeout = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_MAILBOX_TIMEOUT_MS",
            millis_u64(messaging_defaults.mailbox_timeout),
        )?);
        let max_concurrent_streams = parse_usize_var(
            &vars,
            "WADDLE_CLUSTERING_MAX_CONCURRENT_STREAMS",
            messaging_defaults.max_concurrent_streams,
        )?;
        let max_request_bytes = parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_MAX_REQUEST_BYTES",
            messaging_defaults.max_request_bytes,
        )?;
        let max_response_bytes = parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_MAX_RESPONSE_BYTES",
            messaging_defaults.max_response_bytes,
        )?;

        // ADR element-5 timeout hierarchy. `request_timeout` is the sender-side
        // transport cap; a `reply_timeout` above it is dead configuration (the
        // sender always observes `OutboundFailure(Timeout)` at the cap), and
        // the receiver-side `mailbox_timeout` must also fit under the cap.
        // All three must be non-zero: a zero anywhere times out every ask
        // instantly, and an all-zero triple would otherwise satisfy every
        // ordering check below.
        if request_timeout.is_zero() || reply_timeout.is_zero() || mailbox_timeout.is_zero() {
            return Err(
                "WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS, WADDLE_CLUSTERING_REPLY_TIMEOUT_MS, \
                 and WADDLE_CLUSTERING_MAILBOX_TIMEOUT_MS must all be greater than 0"
                    .to_string(),
            );
        }
        if reply_timeout > request_timeout {
            return Err(format!(
                "WADDLE_CLUSTERING_REPLY_TIMEOUT_MS ({}) must be <= \
                 WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS ({}): a reply timeout above the \
                 transport request timeout is dead configuration",
                reply_timeout.as_millis(),
                request_timeout.as_millis()
            ));
        }
        if mailbox_timeout > request_timeout {
            return Err(format!(
                "WADDLE_CLUSTERING_MAILBOX_TIMEOUT_MS ({}) must be <= \
                 WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS ({})",
                mailbox_timeout.as_millis(),
                request_timeout.as_millis()
            ));
        }
        // The mailbox and reply phases of an ask are sequential on the
        // receiver, so their combined worst case must also fit under the
        // sender-side transport cap — otherwise the tail of the reply budget
        // is dead configuration the transport timeout always preempts.
        if mailbox_timeout + reply_timeout > request_timeout {
            return Err(format!(
                "WADDLE_CLUSTERING_MAILBOX_TIMEOUT_MS ({}) + \
                 WADDLE_CLUSTERING_REPLY_TIMEOUT_MS ({}) must be <= \
                 WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS ({}): the phases are \
                 sequential, so any combined budget above the transport cap \
                 can never be observed",
                mailbox_timeout.as_millis(),
                reply_timeout.as_millis(),
                request_timeout.as_millis()
            ));
        }
        let remote_register_mailbox_timeout = mailbox_timeout.min(ORDERED_RELAY_MAILBOX_TIMEOUT);
        let remote_register_reply_timeout = reply_timeout
            .min(ORDERED_RELAY_REPLY_TIMEOUT)
            .max(REMOTE_OWNER_REGISTER_REPLY_TIMEOUT);
        if remote_register_mailbox_timeout + remote_register_reply_timeout > request_timeout {
            return Err(format!(
                "WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS ({}) must cover the remote-resource \
                 registration mailbox ({}) + reply ({}) timeout budget",
                request_timeout.as_millis(),
                remote_register_mailbox_timeout.as_millis(),
                remote_register_reply_timeout.as_millis(),
            ));
        }
        if max_concurrent_streams == 0 {
            return Err("WADDLE_CLUSTERING_MAX_CONCURRENT_STREAMS must be at least 1".to_string());
        }
        if max_request_bytes == 0 || max_response_bytes == 0 {
            return Err(
                "WADDLE_CLUSTERING_MAX_REQUEST_BYTES and WADDLE_CLUSTERING_MAX_RESPONSE_BYTES \
                 must both be non-zero"
                    .to_string(),
            );
        }

        let keypair_pool = match vars
            .get("WADDLE_CLUSTERING_KEYPAIR_POOL")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        {
            None => Vec::new(),
            Some(raw) => {
                let entries: Vec<String> = raw
                    .split(',')
                    .map(|entry| entry.trim().to_string())
                    .filter(|entry| !entry.is_empty())
                    .collect();
                // Two slots holding the same keypair would let two live nodes
                // share a PeerId — exactly what the slot lease exists to
                // prevent. The entries are secrets, so the error names slot
                // positions, never values.
                let mut seen = std::collections::HashMap::new();
                for (index, entry) in entries.iter().enumerate() {
                    if let Some(first) = seen.insert(entry.as_str(), index) {
                        return Err(format!(
                            "WADDLE_CLUSTERING_KEYPAIR_POOL entries at positions {first} and \
                             {index} are identical: duplicate keypairs would let two live \
                             nodes share a PeerId"
                        ));
                    }
                }
                entries
            }
        };

        let lease_defaults = ClusteringLeaseConfig::default();
        let heartbeat_interval = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_HEARTBEAT_INTERVAL_MS",
            millis_u64(lease_defaults.heartbeat_interval),
        )?);
        let lease_ttl = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_LEASE_TTL_MS",
            millis_u64(lease_defaults.lease_ttl),
        )?);
        if heartbeat_interval.is_zero() {
            return Err(
                "WADDLE_CLUSTERING_HEARTBEAT_INTERVAL_MS must be greater than 0".to_string(),
            );
        }
        // TTL must survive at least one missed renewal plus margin, else a
        // single delayed heartbeat forfeits the slot.
        if lease_ttl < heartbeat_interval * 2 {
            return Err(format!(
                "WADDLE_CLUSTERING_LEASE_TTL_MS ({}) must be at least 2x \
                 WADDLE_CLUSTERING_HEARTBEAT_INTERVAL_MS ({})",
                lease_ttl.as_millis(),
                heartbeat_interval.as_millis()
            ));
        }

        let allowlist_refresh_interval = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_ALLOWLIST_REFRESH_MS",
            millis_u64(Self::default().allowlist_refresh_interval),
        )?);
        if allowlist_refresh_interval.is_zero() {
            return Err(
                "WADDLE_CLUSTERING_ALLOWLIST_REFRESH_MS must be greater than 0".to_string(),
            );
        }
        let dial_interval = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_DIAL_INTERVAL_MS",
            millis_u64(Self::default().dial_interval),
        )?);
        if dial_interval.is_zero() {
            return Err("WADDLE_CLUSTERING_DIAL_INTERVAL_MS must be greater than 0".to_string());
        }
        // ADR-0017 Phase 3 Slice 11 corrigenda (deviation 111): the orphan
        // reaper's own cadence, parsed and validated the same way every
        // sibling cluster timer is — previously hardcoded with no override
        // anywhere (`session_janitors::ORPHAN_REAPER_INTERVAL`).
        let orphan_reaper_interval = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS",
            millis_u64(Self::default().orphan_reaper_interval),
        )?);
        if orphan_reaper_interval.is_zero() {
            return Err(
                "WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS must be greater than 0".to_string(),
            );
        }
        let fault_injection = parse_bool_var(&vars, "WADDLE_CLUSTERING_FAULT_INJECTION", false)?;
        let node_id_file = vars
            .get("WADDLE_CLUSTERING_NODE_ID_FILE")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from);
        // FIX 6: moved from a raw `std::env::var` read at the `mod.rs`
        // clustering-bringup call site into the typed config pipeline, like
        // every sibling var — trimmed, empty ⇒ `None`, never a parse
        // failure (this is deployment-generation metadata, not something
        // that gates startup).
        let pod_template_hash = vars
            .get("WADDLE_CLUSTERING_POD_TEMPLATE_HASH")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        let node_lease_defaults = ClusteringNodeLeaseConfig::default();
        let node_lease_heartbeat_interval = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_NODE_LEASE_HEARTBEAT_INTERVAL_MS",
            millis_u64(node_lease_defaults.heartbeat_interval),
        )?);
        let node_lease_ttl = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_NODE_LEASE_TTL_MS",
            millis_u64(node_lease_defaults.lease_ttl),
        )?);
        if node_lease_heartbeat_interval.is_zero() {
            return Err(
                "WADDLE_CLUSTERING_NODE_LEASE_HEARTBEAT_INTERVAL_MS must be greater than 0"
                    .to_string(),
            );
        }
        if node_lease_ttl < node_lease_heartbeat_interval * 2 {
            return Err(format!(
                "WADDLE_CLUSTERING_NODE_LEASE_TTL_MS ({}) must be at least 2x \
                 WADDLE_CLUSTERING_NODE_LEASE_HEARTBEAT_INTERVAL_MS ({})",
                node_lease_ttl.as_millis(),
                node_lease_heartbeat_interval.as_millis()
            ));
        }
        // ADR-0017 Phase 3 Slice 10: the per-entity claim-release drain
        // budget. Same typed-`config.rs`-pipeline pattern every other
        // clustering timing value uses (never `WADDLE_DRAIN_TIMEOUT_SECS`'s
        // raw-`std::env::var` shortcut, which is not clustering-specific).
        let claim_release_budget = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS",
            millis_u64(node_lease_defaults.claim_release_budget),
        )?);
        if claim_release_budget.is_zero() {
            return Err(
                "WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS must be greater than 0".to_string(),
            );
        }
        // ADR-0017 Phase 3 Slice 10 FIX 2 (council-adjudicated,
        // defense-in-depth): `run_shutdown_drain_with_heartbeat`
        // (`self_fence.rs`) keeps renewing this node's own heartbeat for
        // the full duration of ITS OWN drain (bounded by
        // `claim_release_budget`), but `node_lease_ttl` is also the floor
        // every OTHER node's orphan reaper honors before treating this
        // node's `clustering_nodes` row as stale enough to steal from
        // (`NodeLeaseStore::expire`'s own `heartbeat < now() - lease_ttl`
        // check) — a `claim_release_budget` too close to `node_lease_ttl`
        // leaves no margin: a single missed/slow renewal mid-drain (a
        // transient Postgres hiccup, GC pause, etc.) could then let the
        // row go stale WHILE this node still legitimately holds and is
        // sealing claims, reopening the exact split-brain window the
        // runtime restructure closes. `CLAIM_RELEASE_BUDGET_SAFETY_FACTOR`
        // mirrors this function's other "conservative multiple of the
        // faster timer" floors (`node_lease_ttl >= heartbeat_interval *
        // 2`, `intent_ttl >= heartbeat_interval * 2`) one level up.
        //
        // This is deliberately the ONLY programmatic cross-check: the
        // pre-existing, separate Q6 SM-session drain budget
        // (`WADDLE_DRAIN_TIMEOUT_SECS`, default 30s, operator-clampable to
        // 600s — `session_janitors::max_drain_duration_from_env`) is a raw
        // `std::env::var` read outside this typed `ClusteringConfig`
        // struct entirely (deviation 99's own "never
        // WADDLE_DRAIN_TIMEOUT_SECS's raw-env-var shortcut" note), so it
        // cannot be structurally cross-validated here. Operators sizing
        // `node_lease_ttl` should ALSO account for that budget — this
        // node's clustering-lease heartbeat only keeps renewing through
        // its OWN `claim_release_budget` window, not through the
        // independent, potentially much longer Q6 SM-drain window racing
        // the same shutdown token — documented on
        // [`ClusteringNodeLeaseConfig::claim_release_budget`] and in the
        // Helm `terminationGracePeriodSeconds` formula, which already
        // composes both budgets.
        const CLAIM_RELEASE_BUDGET_SAFETY_FACTOR: u32 = 3;
        if node_lease_ttl < claim_release_budget * CLAIM_RELEASE_BUDGET_SAFETY_FACTOR {
            return Err(format!(
                "WADDLE_CLUSTERING_NODE_LEASE_TTL_MS ({}) must be at least {}x \
                 WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS ({}): a draining node's heartbeat \
                 renewal only runs for the duration of its own claim-release drain, and \
                 node_lease_ttl is the floor other nodes' orphan reaper honors before treating \
                 this node's row as stale — it must leave comfortable margin over \
                 claim_release_budget (and, operationally, over the separate \
                 WADDLE_DRAIN_TIMEOUT_SECS SM-session Q6 drain budget this node's SM-session \
                 drain task races under the same shutdown token, default 30s / clampable to \
                 600s — not itself part of this typed config, so size node_lease_ttl with that \
                 in mind too)",
                node_lease_ttl.as_millis(),
                CLAIM_RELEASE_BUDGET_SAFETY_FACTOR,
                claim_release_budget.as_millis()
            ));
        }

        let self_fence_defaults = ClusteringSelfFenceConfig::default();
        let isolation_intervals = parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_ISOLATION_INTERVALS",
            u64::from(self_fence_defaults.isolation_intervals),
        )?;
        let isolation_intervals = u32::try_from(isolation_intervals).map_err(|_| {
            format!(
                "WADDLE_CLUSTERING_ISOLATION_INTERVALS ({isolation_intervals}) does not fit in u32"
            )
        })?;
        if isolation_intervals == 0 {
            return Err("WADDLE_CLUSTERING_ISOLATION_INTERVALS must be at least 1".to_string());
        }
        let reregister_backoff_base = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_REREGISTER_BACKOFF_BASE_MS",
            millis_u64(self_fence_defaults.reregister_backoff_base),
        )?);
        let reregister_backoff_max = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_REREGISTER_BACKOFF_MAX_MS",
            millis_u64(self_fence_defaults.reregister_backoff_max),
        )?);
        if reregister_backoff_base.is_zero() {
            return Err(
                "WADDLE_CLUSTERING_REREGISTER_BACKOFF_BASE_MS must be greater than 0".to_string(),
            );
        }
        if reregister_backoff_max < reregister_backoff_base {
            return Err(format!(
                "WADDLE_CLUSTERING_REREGISTER_BACKOFF_MAX_MS ({}) must be >= \
                 WADDLE_CLUSTERING_REREGISTER_BACKOFF_BASE_MS ({})",
                reregister_backoff_max.as_millis(),
                reregister_backoff_base.as_millis()
            ));
        }

        let steal_intent_defaults = ClusteringStealIntentConfig::default();
        let intent_ttl = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_STEAL_INTENT_TTL_MS",
            millis_u64(steal_intent_defaults.intent_ttl),
        )?);
        if intent_ttl.is_zero() {
            return Err("WADDLE_CLUSTERING_STEAL_INTENT_TTL_MS must be greater than 0".to_string());
        }
        if intent_ttl < node_lease_heartbeat_interval * 2 {
            return Err(format!(
                "WADDLE_CLUSTERING_STEAL_INTENT_TTL_MS ({}) must be at least 2x \
                 WADDLE_CLUSTERING_NODE_LEASE_HEARTBEAT_INTERVAL_MS ({}): the owner-veto scan \
                 only runs once per heartbeat tick, so worst-case phase alignment between an \
                 intent's report time and the scan's cadence can consume most of one interval \
                 before the owner's first chance to observe it — 2x is the conservative floor \
                 guaranteeing at least one full scan interval survives inside intent_ttl \
                 regardless of phase (mirroring node_lease_ttl's own 2x floor against its \
                 heartbeat interval)",
                intent_ttl.as_millis(),
                node_lease_heartbeat_interval.as_millis()
            ));
        }

        let resume_handshake_defaults = ClusteringResumeHandshakeConfig::default();
        let resume_handshake_timeout = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_RESUME_HANDSHAKE_TIMEOUT_MS",
            millis_u64(resume_handshake_defaults.timeout),
        )?);
        if resume_handshake_timeout.is_zero() {
            return Err(
                "WADDLE_CLUSTERING_RESUME_HANDSHAKE_TIMEOUT_MS must be greater than 0".to_string(),
            );
        }
        if resume_handshake_timeout > node_lease_ttl {
            return Err(format!(
                "WADDLE_CLUSTERING_RESUME_HANDSHAKE_TIMEOUT_MS ({}) must be <= \
                 WADDLE_CLUSTERING_NODE_LEASE_TTL_MS ({}): the held-response window is capped \
                 at min(remaining lease TTL, resume-handshake timeout) per ADR-0017 element 8, \
                 which this config makes an upper bound by construction rather than computing \
                 a live per-owner remaining-TTL read (see this config's own doc comment)",
                resume_handshake_timeout.as_millis(),
                node_lease_ttl.as_millis()
            ));
        }

        Ok(Self {
            enabled,
            listen_addrs,
            bootstrap_peers,
            messaging: ClusteringMessagingConfig {
                request_timeout,
                reply_timeout,
                mailbox_timeout,
                max_concurrent_streams,
                max_request_bytes,
                max_response_bytes,
            },
            keypair_pool,
            lease: ClusteringLeaseConfig {
                heartbeat_interval,
                lease_ttl,
            },
            allowlist_refresh_interval,
            dial_interval,
            orphan_reaper_interval,
            fault_injection,
            node_id_file,
            node_lease: ClusteringNodeLeaseConfig {
                heartbeat_interval: node_lease_heartbeat_interval,
                lease_ttl: node_lease_ttl,
                claim_release_budget,
            },
            self_fence: ClusteringSelfFenceConfig {
                isolation_intervals,
                reregister_backoff_base,
                reregister_backoff_max,
            },
            steal_intent: ClusteringStealIntentConfig { intent_ttl },
            resume_handshake: ClusteringResumeHandshakeConfig {
                timeout: resume_handshake_timeout,
            },
            pod_template_hash,
        })
    }
}

/// Milliseconds of a `Duration` as `u64`, saturating (used only to derive
/// env-var defaults from the compiled-in `Duration` defaults).
fn millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub mode: ServerMode,
    pub base_url: String,
    pub session_key: String,
    pub auth: AuthConfig,
    /// Runtime extension configuration.
    pub extensions: ExtensionConfig,
    /// Operator controls for server-side link-preview enrichment.
    pub link_preview: LinkPreviewConfig,
    /// RFC 7395 §3.8 WebSocket keepalive knobs (issue #1090), parsed
    /// from `WADDLE_WS_KEEPALIVE_*` by [`ws_keepalive_from_vars`].
    pub ws_keepalive: waddle_xmpp::protocol::KeepaliveConfig,
    /// SpiceDB backend configuration.
    /// Runtime startup requires this to be set.
    pub spicedb: Option<SpiceDbConfig>,
    /// Per-deployment HMAC key used to derive XEP-0421 occupant
    /// identifiers. Loaded from `WADDLE_OCCUPANT_ID_SECRET` and shared
    /// across the WebSocket dependencies and `RoomRegistryActor` so
    /// every stamping site reads the same key. Required at startup;
    /// see [`parse_occupant_id_secret`] for the validation rules.
    pub occupant_id_secret: OccupantIdSecret,
    /// ADR-0017 Phase 2 clustering (owned libp2p swarm) configuration. With
    /// `enabled` false (the default) the swarm subsystem never starts.
    pub clustering: ClusteringConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPreviewConfig {
    pub enabled: bool,
    pub allowed_hosts: Vec<LinkPreviewHostPattern>,
    pub blocked_hosts: Vec<LinkPreviewHostPattern>,
    /// Maximum bytes fetched while scanning an HTML document for OpenGraph
    /// metadata. The resolver stops shortly after locating `</head>` — it reads
    /// a bounded window past the head (so streaming-SSR frameworks that emit og
    /// tags into the `<body>` are still captured), then stops; well-formed pages
    /// typically read only the head plus that small window. The cap bounds large
    /// pages (e.g. YouTube emits its og tags ~640 KB deep) and acts as a DoS
    /// limit. Does not affect cached-image fetch limits.
    pub max_html_head_bytes: usize,
    pub max_cached_image_bytes: usize,
    pub max_redirects: usize,
    pub fetch_timeout: Duration,
    pub video_enabled: bool,
}

impl Default for LinkPreviewConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allowed_hosts: Vec::new(),
            blocked_hosts: Vec::new(),
            max_html_head_bytes: 1024 * 1024,
            max_cached_image_bytes: 2 * 1024 * 1024,
            max_redirects: 3,
            fetch_timeout: Duration::from_millis(1_500),
            video_enabled: true,
        }
    }
}

impl LinkPreviewConfig {
    const MAX_FETCH_TIMEOUT: Duration = Duration::from_secs(60);

    pub fn from_env() -> Result<Self, String> {
        Self::from_vars(std::env::vars())
    }

    pub fn from_vars<I, K, V>(vars: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let vars = vars
            .into_iter()
            .map(|(key, value)| (key.as_ref().to_string(), value.as_ref().to_string()))
            .collect::<std::collections::HashMap<_, _>>();
        let fetch_timeout = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_LINK_PREVIEW_FETCH_TIMEOUT_MS",
            1_500,
        )?);
        if fetch_timeout > Self::MAX_FETCH_TIMEOUT {
            return Err(format!(
                "WADDLE_LINK_PREVIEW_FETCH_TIMEOUT_MS must be at most {}ms",
                Self::MAX_FETCH_TIMEOUT.as_millis()
            ));
        }

        Ok(Self {
            enabled: parse_bool_var(&vars, "WADDLE_LINK_PREVIEW_ENABLED", true)?,
            allowed_hosts: parse_host_patterns_var(&vars, "WADDLE_LINK_PREVIEW_ALLOWED_HOSTS")?,
            blocked_hosts: parse_host_patterns_var(&vars, "WADDLE_LINK_PREVIEW_BLOCKED_HOSTS")?,
            max_html_head_bytes: parse_usize_var(
                &vars,
                "WADDLE_LINK_PREVIEW_MAX_HTML_HEAD_BYTES",
                1024 * 1024,
            )?,
            max_cached_image_bytes: parse_usize_var(
                &vars,
                "WADDLE_LINK_PREVIEW_MAX_CACHED_IMAGE_BYTES",
                2 * 1024 * 1024,
            )?,
            max_redirects: parse_usize_var(&vars, "WADDLE_LINK_PREVIEW_MAX_REDIRECTS", 3)?,
            fetch_timeout,
            video_enabled: parse_bool_var(&vars, "WADDLE_LINK_PREVIEW_VIDEO_ENABLED", true)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkPreviewHostPattern {
    Exact(String),
    DomainSuffix(String),
}

impl LinkPreviewHostPattern {
    pub fn matches(&self, host: &str) -> bool {
        let host = normalize_host_pattern_value(host);
        match self {
            Self::Exact(pattern) => host == *pattern,
            Self::DomainSuffix(suffix) => {
                host == *suffix
                    || host
                        .strip_suffix(suffix)
                        .is_some_and(|prefix| prefix.ends_with('.'))
            }
        }
    }
}

impl FromStr for LinkPreviewHostPattern {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("host pattern must not be empty".to_string());
        }
        let suffix = trimmed
            .strip_prefix("*.")
            .or_else(|| trimmed.strip_prefix('.'));
        let (suffix_match, value) = match suffix {
            Some(value) => (true, value),
            None => (false, trimmed),
        };
        let normalized = normalize_host_pattern_value(value);
        if normalized.is_empty()
            || normalized.contains('/')
            || normalized.contains(':')
            || normalized.contains('*')
            || normalized.contains(char::is_whitespace)
        {
            return Err(format!("invalid host pattern '{raw}'"));
        }
        if suffix_match {
            Ok(Self::DomainSuffix(normalized))
        } else {
            Ok(Self::Exact(normalized))
        }
    }
}

fn normalize_host_pattern_value(value: &str) -> String {
    value.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Validate the `WADDLE_OCCUPANT_ID_SECRET` env var into a typed secret.
///
/// Pure function so the validation logic is unit-testable without
/// mutating process-global env state. Called by [`ServerConfig::from_env`]
/// with the result of `std::env::var(...).ok().as_deref()`.
fn parse_occupant_id_secret(raw: Option<&str>) -> Result<OccupantIdSecret, String> {
    let value = raw.ok_or_else(|| {
        format!(
            "WADDLE_OCCUPANT_ID_SECRET is required (≥{OCCUPANT_ID_SECRET_MIN_BYTES} bytes; \
             generate with: openssl rand -base64 48)"
        )
    })?;
    OccupantIdSecret::new(value.as_bytes().to_vec()).map_err(|e| {
        format!(
            "WADDLE_OCCUPANT_ID_SECRET invalid: {e} \
             (generate with: openssl rand -base64 48)"
        )
    })
}

const SESSION_KEY_MIN_BYTES: usize = 32;

fn parse_session_key(raw: Option<&str>) -> Result<String, String> {
    let value = raw.filter(|value| !value.is_empty()).ok_or_else(|| {
        "WADDLE_SESSION_KEY is required (generate with: openssl rand -base64 48)".to_string()
    })?;
    if value.len() < SESSION_KEY_MIN_BYTES {
        return Err(format!(
            "WADDLE_SESSION_KEY must be at least {SESSION_KEY_MIN_BYTES} bytes \
             (generate with: openssl rand -base64 48)"
        ));
    }
    Ok(value.to_string())
}

fn parse_bool_var(
    vars: &std::collections::HashMap<String, String>,
    key: &str,
    default: bool,
) -> Result<bool, String> {
    let Some(value) = vars.get(key).map(|value| value.trim().to_ascii_lowercase()) else {
        return Ok(default);
    };
    match value.as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!(
            "{key}='{value}' must be a boolean: true/false, yes/no, on/off, or 1/0"
        )),
    }
}

fn parse_usize_var(
    vars: &std::collections::HashMap<String, String>,
    key: &str,
    default: usize,
) -> Result<usize, String> {
    vars.get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|error| format!("{key}='{value}' must be a positive integer: {error}"))
        })
        .unwrap_or(Ok(default))
}

fn parse_u64_var(
    vars: &std::collections::HashMap<String, String>,
    key: &str,
    default: u64,
) -> Result<u64, String> {
    vars.get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|error| format!("{key}='{value}' must be a positive integer: {error}"))
        })
        .unwrap_or(Ok(default))
}

/// Ceiling for `WADDLE_WS_KEEPALIVE_INTERVAL_SECS`.
///
/// On an idle-but-alive connection the probe's pong counts as activity
/// for the following tick, so the worst-case inter-traffic gap on the
/// stream is `2 × interval`. The Cilium/Envoy gateway in front resets
/// idle streams at its ~300s default; capping the interval at 120s
/// bounds the gap at 240s with a 60s margin. This startup guard
/// replaces the "raise gateway idleTimeout" defense-in-depth from
/// issue #1090's original acceptance criteria — a fat-fingered
/// interval fails fast instead of silently reintroducing the ~304s
/// reset storm.
const WS_KEEPALIVE_MAX_INTERVAL_SECS: u64 = 120;

/// Upper bound for `WADDLE_WS_KEEPALIVE_MISS_LIMIT`; beyond this the
/// dead-peer detection is too slow to beat the XEP-0198 unacked-queue
/// cap on busy rooms.
const WS_KEEPALIVE_MAX_MISS_LIMIT: u64 = 10;

/// Typed validation failure for the `WADDLE_WS_KEEPALIVE_*` knobs
/// (issue #1090).
///
/// Per the typed-payloads rule, error results are typed enums; the
/// `Display` text is the human-facing startup diagnostic surfaced by
/// [`ServerConfig::from_env`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WsKeepaliveConfigError {
    /// The interval would let the worst-case inter-traffic gap
    /// (`2 × interval`) reach the gateway's ~300s stream-idle timeout.
    #[error(
        "WADDLE_WS_KEEPALIVE_INTERVAL_SECS='{value}' must be between 1 and {max}: the \
         worst-case inter-traffic gap is twice the interval and must stay under the \
         gateway's 300s stream-idle timeout"
    )]
    IntervalOutOfRange { value: u64, max: u64 },
    /// The miss limit is zero (would close every idle peer instantly)
    /// or so high that dead peers outlive the unacked-queue cap.
    #[error("WADDLE_WS_KEEPALIVE_MISS_LIMIT='{value}' must be between 1 and {max}")]
    MissLimitOutOfRange { value: u64, max: u64 },
    /// The env var is set but is not a base-10 unsigned integer.
    #[error("{key}='{value}' must be a positive integer")]
    NotAnInteger { key: &'static str, value: String },
}

/// Read a `WADDLE_WS_KEEPALIVE_*` var as `u64`, treating unset/blank
/// as the default. Sibling of [`parse_u64_var`] with a typed error.
fn ws_keepalive_u64_var(
    vars: &std::collections::HashMap<String, String>,
    key: &'static str,
    default: u64,
) -> Result<u64, WsKeepaliveConfigError> {
    match vars
        .get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        None => Ok(default),
        Some(raw) => raw
            .parse::<u64>()
            .map_err(|_| WsKeepaliveConfigError::NotAnInteger {
                key,
                value: raw.to_string(),
            }),
    }
}

/// Parse + validate the RFC 7395 §3.8 keepalive knobs (issue #1090):
///
/// - `WADDLE_WS_KEEPALIVE_INTERVAL_SECS` — probe/tick interval,
///   default 45, valid range 1..=120 (see
///   [`WS_KEEPALIVE_MAX_INTERVAL_SECS`]).
/// - `WADDLE_WS_KEEPALIVE_MISS_LIMIT` — consecutive unanswered probes
///   before the connection is closed, default 2, valid range 1..=10.
///
/// Out-of-range values are startup errors, never clamped: a config
/// that would defeat the keepalive must fail loudly.
pub fn ws_keepalive_from_vars<I, K, V>(
    vars: I,
) -> Result<waddle_xmpp::protocol::KeepaliveConfig, WsKeepaliveConfigError>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let vars = vars
        .into_iter()
        .map(|(key, value)| (key.as_ref().to_string(), value.as_ref().to_string()))
        .collect::<std::collections::HashMap<_, _>>();
    let interval_secs = ws_keepalive_u64_var(&vars, "WADDLE_WS_KEEPALIVE_INTERVAL_SECS", 45)?;
    if !(1..=WS_KEEPALIVE_MAX_INTERVAL_SECS).contains(&interval_secs) {
        return Err(WsKeepaliveConfigError::IntervalOutOfRange {
            value: interval_secs,
            max: WS_KEEPALIVE_MAX_INTERVAL_SECS,
        });
    }
    let miss_limit = ws_keepalive_u64_var(&vars, "WADDLE_WS_KEEPALIVE_MISS_LIMIT", 2)?;
    if !(1..=WS_KEEPALIVE_MAX_MISS_LIMIT).contains(&miss_limit) {
        return Err(WsKeepaliveConfigError::MissLimitOutOfRange {
            value: miss_limit,
            max: WS_KEEPALIVE_MAX_MISS_LIMIT,
        });
    }
    Ok(waddle_xmpp::protocol::KeepaliveConfig {
        interval_ms: interval_secs * 1_000,
        // Infallible: miss_limit is range-checked to 1..=10 above.
        miss_limit: miss_limit as u32,
    })
}

/// Env-reading wrapper around [`ws_keepalive_from_vars`].
pub fn ws_keepalive_from_env(
) -> Result<waddle_xmpp::protocol::KeepaliveConfig, WsKeepaliveConfigError> {
    ws_keepalive_from_vars(std::env::vars())
}

fn parse_host_patterns_var(
    vars: &std::collections::HashMap<String, String>,
    key: &str,
) -> Result<Vec<LinkPreviewHostPattern>, String> {
    let Some(raw) = vars
        .get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            entry
                .parse::<LinkPreviewHostPattern>()
                .map_err(|error| format!("{key}: {error}"))
        })
        .collect()
}

#[cfg(test)]
const TEST_OCCUPANT_ID_SECRET: &str = "test-occupant-id-secret-32-bytes-long";

#[cfg(test)]
fn test_occupant_id_secret() -> OccupantIdSecret {
    OccupantIdSecret::new(TEST_OCCUPANT_ID_SECRET.as_bytes().to_vec())
        .expect("test secret meets length floor")
}

// `Default` is gated to `#[cfg(test)]`. Production startup MUST go
// through `ServerConfig::from_env`, which enforces the deployment-keyed
// `WADDLE_SESSION_KEY` and `WADDLE_OCCUPANT_ID_SECRET`; a non-test
// `Default` impl could be silently used (e.g. via `..Default::default()`
// in scaffolding) and reintroduce the cross-deployment linkability that
// #283 closes.
#[cfg(test)]
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            mode: ServerMode::default(),
            base_url: "http://localhost:3000".to_string(),
            session_key: "test-session-key-32-bytes-minimum".to_string(),
            auth: AuthConfig::default(),
            extensions: ExtensionConfig::default(),
            link_preview: LinkPreviewConfig::default(),
            ws_keepalive: waddle_xmpp::protocol::KeepaliveConfig::default(),
            spicedb: None,
            occupant_id_secret: test_occupant_id_secret(),
            clustering: ClusteringConfig::default(),
        }
    }
}

impl ServerConfig {
    pub fn from_env() -> Result<Self, String> {
        let mode_str = std::env::var("WADDLE_MODE").unwrap_or_else(|_| "homeserver".to_string());
        let mode = mode_str.parse().unwrap_or_default();

        let base_url = std::env::var("WADDLE_BASE_URL")
            .unwrap_or_else(|_| "http://localhost:3000".to_string());

        let session_key = parse_session_key(std::env::var("WADDLE_SESSION_KEY").ok().as_deref())?;
        let auth = AuthConfig::from_env()?;

        let extensions =
            ExtensionConfig::from_env().map_err(|e| format!("invalid extension config: {e}"))?;
        let link_preview = LinkPreviewConfig::from_env()?;
        // `ServerConfig::from_env` predates the typed-error rule and
        // still aggregates `String` diagnostics; render the typed
        // keepalive error at this boundary.
        let ws_keepalive = ws_keepalive_from_env().map_err(|error| error.to_string())?;
        let spicedb = SpiceDbConfig::from_env()?;

        let occupant_id_secret =
            parse_occupant_id_secret(std::env::var("WADDLE_OCCUPANT_ID_SECRET").ok().as_deref())?;

        let clustering = ClusteringConfig::from_env()?;

        Ok(Self {
            mode,
            base_url,
            session_key,
            auth,
            extensions,
            link_preview,
            ws_keepalive,
            spicedb,
            occupant_id_secret,
            clustering,
        })
    }

    pub fn auth_enabled(&self) -> bool {
        self.mode.auth_broker_allowed() && !self.auth.providers.is_empty()
    }

    pub fn log_config(&self) {
        info!("Running in {} mode", self.mode);
        info!("Base URL: {}", self.base_url);
        info!("Auth providers configured: {}", self.auth.providers.len());
        info!(
            "HTTP auth broker: {}",
            if self.auth_enabled() {
                "enabled"
            } else {
                "disabled"
            }
        );
    }

    #[cfg(test)]
    pub fn test_homeserver() -> Self {
        Self {
            mode: ServerMode::HomeServer,
            base_url: "http://localhost:3000".to_string(),
            session_key: "test-key-32-bytes-long-for-aes!".to_string(),
            auth: AuthConfig::default(),
            extensions: ExtensionConfig::default(),
            link_preview: LinkPreviewConfig::default(),
            ws_keepalive: waddle_xmpp::protocol::KeepaliveConfig::default(),
            spicedb: None,
            occupant_id_secret: test_occupant_id_secret(),
            clustering: ClusteringConfig::default(),
        }
    }

    #[cfg(test)]
    pub fn test_standalone() -> Self {
        Self {
            mode: ServerMode::Standalone,
            base_url: "http://localhost:3000".to_string(),
            session_key: "test-key-32-bytes-long-for-aes!".to_string(),
            auth: AuthConfig::default(),
            extensions: ExtensionConfig::default(),
            link_preview: LinkPreviewConfig::default(),
            ws_keepalive: waddle_xmpp::protocol::KeepaliveConfig::default(),
            spicedb: None,
            occupant_id_secret: test_occupant_id_secret(),
            clustering: ClusteringConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clustering_defaults_are_disabled() {
        let config = ClusteringConfig::from_vars(std::iter::empty::<(&str, &str)>()).unwrap();
        assert!(!config.enabled);
        assert_eq!(config.listen_addrs, vec!["/ip4/0.0.0.0/tcp/0".to_string()]);
        assert!(config.bootstrap_peers.is_empty());
        assert_eq!(config.messaging, ClusteringMessagingConfig::default());
        // Byte-for-byte-identical guarantee: the whole struct equals Default.
        assert_eq!(config, ClusteringConfig::default());
    }

    #[test]
    fn clustering_parses_enabled_and_listen_addrs() {
        let config = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_ENABLED", "true"),
            (
                "WADDLE_CLUSTERING_LISTEN_ADDRS",
                "/ip4/0.0.0.0/tcp/7900, /ip4/0.0.0.0/udp/7900/quic-v1",
            ),
        ])
        .unwrap();
        assert!(config.enabled);
        assert_eq!(
            config.listen_addrs,
            vec![
                "/ip4/0.0.0.0/tcp/7900".to_string(),
                "/ip4/0.0.0.0/udp/7900/quic-v1".to_string(),
            ]
        );
    }

    #[test]
    fn clustering_parses_bootstrap_peer_list() {
        let config = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_ENABLED", "1"),
            (
                "WADDLE_CLUSTERING_BOOTSTRAP_PEERS",
                "waddle-server-swarm:7900, localhost:7901",
            ),
        ])
        .unwrap();
        assert_eq!(
            config.bootstrap_peers,
            vec![
                ClusteringBootstrapConfig {
                    dns_name: "waddle-server-swarm".to_string(),
                    port: 7900,
                },
                ClusteringBootstrapConfig {
                    dns_name: "localhost".to_string(),
                    port: 7901,
                },
            ]
        );
    }

    #[test]
    fn clustering_rejects_malformed_bootstrap_entries() {
        for bad in [
            "no-port",
            "host:0",
            ":7900",
            "host:notaport",
            // Unbracketed IPv6: rsplit would "parse" it and then it never
            // resolves — must fail fast at config time.
            "::1:7900",
            "fe80::1:7900",
            // Bracketed but broken.
            "[::1]",
            "[::1]:0",
            "[not-ipv6]:7900",
        ] {
            let err = ClusteringConfig::from_vars([("WADDLE_CLUSTERING_BOOTSTRAP_PEERS", bad)])
                .unwrap_err();
            assert!(
                err.contains("WADDLE_CLUSTERING_BOOTSTRAP_PEERS"),
                "entry '{bad}' must be rejected: {err}"
            );
        }
    }

    #[test]
    fn clustering_parses_bracketed_ipv6_bootstrap_entries() {
        let config = ClusteringConfig::from_vars([(
            "WADDLE_CLUSTERING_BOOTSTRAP_PEERS",
            "[::1]:7900, [fe80::1]:7901",
        )])
        .unwrap();
        assert_eq!(
            config.bootstrap_peers,
            vec![
                ClusteringBootstrapConfig {
                    dns_name: "::1".to_string(),
                    port: 7900,
                },
                ClusteringBootstrapConfig {
                    dns_name: "fe80::1".to_string(),
                    port: 7901,
                },
            ]
        );
    }

    #[test]
    fn clustering_rejects_reply_timeout_above_request_timeout() {
        // reply_timeout > request_timeout is dead configuration per ADR
        // element 5 (the sender caps out at the transport request_timeout).
        let err = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS", "10000"),
            ("WADDLE_CLUSTERING_REPLY_TIMEOUT_MS", "20000"),
        ])
        .unwrap_err();
        assert!(err.contains("must be <="));
    }

    #[test]
    fn clustering_rejects_mailbox_timeout_above_request_timeout() {
        let err = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS", "10000"),
            // Keep reply under the request cap so the mailbox check is the one
            // that trips (otherwise the reply-timeout invariant fires first).
            ("WADDLE_CLUSTERING_REPLY_TIMEOUT_MS", "5000"),
            ("WADDLE_CLUSTERING_MAILBOX_TIMEOUT_MS", "20000"),
        ])
        .unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_MAILBOX_TIMEOUT_MS"));
    }

    #[test]
    fn clustering_rejects_zero_timeouts() {
        // An all-zero triple satisfies every ordering check (0 <= 0), so the
        // non-zero guard must catch it — and each individually-zero value.
        for vars in [
            vec![
                ("WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS", "0"),
                ("WADDLE_CLUSTERING_REPLY_TIMEOUT_MS", "0"),
                ("WADDLE_CLUSTERING_MAILBOX_TIMEOUT_MS", "0"),
            ],
            vec![("WADDLE_CLUSTERING_REPLY_TIMEOUT_MS", "0")],
            vec![("WADDLE_CLUSTERING_MAILBOX_TIMEOUT_MS", "0")],
        ] {
            let err = ClusteringConfig::from_vars(vars.clone()).unwrap_err();
            assert!(
                err.contains("must all be greater than 0"),
                "{vars:?}: {err}"
            );
        }
    }

    #[test]
    fn clustering_rejects_combined_mailbox_plus_reply_above_request_timeout() {
        // Each timeout individually fits under the transport cap, but the
        // mailbox and reply phases are sequential, so their sum exceeding the
        // cap makes the reply budget's tail unreachable.
        let err = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS", "10000"),
            ("WADDLE_CLUSTERING_REPLY_TIMEOUT_MS", "8000"),
            ("WADDLE_CLUSTERING_MAILBOX_TIMEOUT_MS", "8000"),
        ])
        .unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_MAILBOX_TIMEOUT_MS"));
        assert!(err.contains("WADDLE_CLUSTERING_REPLY_TIMEOUT_MS"));
        assert!(err.contains("sequential"));
    }

    #[test]
    fn clustering_rejects_request_timeout_below_remote_registration_budget() {
        let request_timeout = Duration::from_millis(1000)
            .saturating_add(REMOTE_OWNER_REGISTER_REPLY_TIMEOUT)
            .saturating_sub(Duration::from_millis(1))
            .as_millis()
            .to_string();
        let err = ClusteringConfig::from_vars([
            // The fixed floor covers only the nested register admission plus
            // the post-registration currentness proof. Displaced-mirror
            // retirement must surface a prompt retryable typed status instead
            // of inflating this budget, so one millisecond below the exact
            // floor must still fail for the documented pieces only.
            (
                "WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS",
                request_timeout.as_str(),
            ),
            ("WADDLE_CLUSTERING_REPLY_TIMEOUT_MS", "1000"),
            ("WADDLE_CLUSTERING_MAILBOX_TIMEOUT_MS", "1000"),
        ])
        .unwrap_err();
        assert!(err.contains("remote-resource registration"));
    }

    #[test]
    fn clustering_remote_registration_budget_stays_scoped_to_register_and_currentness_proof() {
        assert_eq!(
            REMOTE_OWNER_REGISTER_REPLY_TIMEOUT,
            ORDERED_RELAY_MAILBOX_TIMEOUT
                .saturating_add(REMOTE_OWNER_REGISTER_USER_REGISTRY_REPLY_TIMEOUT)
                .saturating_add(REMOTE_OWNER_REGISTER_POST_REGISTRATION_REPLY_TIMEOUT),
            "the floor is exactly the nested admission window plus the admission-ask \
             reply and post-registration budgets"
        );
    }

    #[test]
    fn clustering_accepts_request_timeout_at_exact_remote_registration_budget() {
        let request_timeout = Duration::from_millis(1000)
            .saturating_add(REMOTE_OWNER_REGISTER_REPLY_TIMEOUT)
            .as_millis()
            .to_string();
        ClusteringConfig::from_vars([
            (
                "WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS",
                request_timeout.as_str(),
            ),
            ("WADDLE_CLUSTERING_REPLY_TIMEOUT_MS", "1000"),
            ("WADDLE_CLUSTERING_MAILBOX_TIMEOUT_MS", "1000"),
        ])
        .expect("the exact remote-resource registration budget floor must be accepted");
    }

    #[test]
    fn clustering_rejects_zero_concurrent_streams() {
        let err = ClusteringConfig::from_vars([("WADDLE_CLUSTERING_MAX_CONCURRENT_STREAMS", "0")])
            .unwrap_err();
        assert!(err.contains("MAX_CONCURRENT_STREAMS"));
    }

    #[test]
    fn clustering_rejects_non_boolean_enabled() {
        let err =
            ClusteringConfig::from_vars([("WADDLE_CLUSTERING_ENABLED", "maybe")]).unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_ENABLED"));
    }

    #[test]
    fn clustering_parses_keypair_pool_and_lease_timing() {
        let config = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_KEYPAIR_POOL", " keyA , keyB ,keyC"),
            ("WADDLE_CLUSTERING_HEARTBEAT_INTERVAL_MS", "5000"),
            ("WADDLE_CLUSTERING_LEASE_TTL_MS", "20000"),
        ])
        .unwrap();
        assert_eq!(config.keypair_pool, vec!["keyA", "keyB", "keyC"]);
        assert_eq!(config.lease.heartbeat_interval.as_millis(), 5000);
        assert_eq!(config.lease.lease_ttl.as_millis(), 20000);
    }

    #[test]
    fn clustering_debug_redacts_keypair_pool_secrets() {
        let config = ClusteringConfig {
            keypair_pool: vec![
                "c2VjcmV0LXNlZWQtQQ==".to_string(),
                "c2VjcmV0LXNlZWQtQg==".to_string(),
            ],
            ..ClusteringConfig::default()
        };
        let rendered = format!("{config:?}");
        assert!(
            !rendered.contains("c2VjcmV0"),
            "Debug output leaked keypair seed material: {rendered}"
        );
        assert!(rendered.contains("[2 keys redacted]"), "{rendered}");
    }

    #[test]
    fn clustering_rejects_duplicate_keypair_pool_entries() {
        let err = ClusteringConfig::from_vars([(
            "WADDLE_CLUSTERING_KEYPAIR_POOL",
            "c2VjcmV0QQ==, c2VjcmV0Qg==, c2VjcmV0QQ==",
        )])
        .unwrap_err();
        assert!(err.contains("positions 0 and 2"), "{err}");
        // The entries are secrets — the diagnostic must not echo them.
        assert!(
            !err.contains("c2VjcmV0"),
            "error leaked pool material: {err}"
        );
    }

    #[test]
    fn clustering_defaults_have_empty_pool_and_valid_lease() {
        let config = ClusteringConfig::from_vars(std::iter::empty::<(&str, &str)>()).unwrap();
        assert!(config.keypair_pool.is_empty());
        assert_eq!(config.lease, ClusteringLeaseConfig::default());
    }

    #[test]
    fn clustering_rejects_lease_ttl_below_two_heartbeats() {
        let err = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_HEARTBEAT_INTERVAL_MS", "10000"),
            ("WADDLE_CLUSTERING_LEASE_TTL_MS", "15000"),
        ])
        .unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_LEASE_TTL_MS"));
    }

    #[test]
    fn clustering_parses_allowlist_refresh_and_rejects_zero() {
        let config =
            ClusteringConfig::from_vars([("WADDLE_CLUSTERING_ALLOWLIST_REFRESH_MS", "5000")])
                .unwrap();
        assert_eq!(config.allowlist_refresh_interval.as_millis(), 5000);

        let err = ClusteringConfig::from_vars([("WADDLE_CLUSTERING_ALLOWLIST_REFRESH_MS", "0")])
            .unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_ALLOWLIST_REFRESH_MS"));
    }

    // ADR-0017 Phase 3 Slice 11 corrigenda (deviation 111, FIX C): the
    // orphan reaper's cadence, parsed/validated/defaulted exactly like its
    // sibling timers — see `clustering_parses_allowlist_refresh_and_rejects_zero`
    // just above.
    #[test]
    fn clustering_parses_orphan_reaper_interval_and_rejects_zero() {
        let config =
            ClusteringConfig::from_vars([("WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS", "500")])
                .unwrap();
        assert_eq!(config.orphan_reaper_interval.as_millis(), 500);

        let defaults = ClusteringConfig::from_vars(std::iter::empty::<(&str, &str)>()).unwrap();
        assert_eq!(defaults.orphan_reaper_interval.as_secs(), 120);

        let err =
            ClusteringConfig::from_vars([("WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS", "0")])
                .unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS"));
    }

    #[test]
    fn clustering_parses_harness_knobs() {
        let config = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_DIAL_INTERVAL_MS", "2000"),
            ("WADDLE_CLUSTERING_FAULT_INJECTION", "true"),
            ("WADDLE_CLUSTERING_NODE_ID_FILE", "/tmp/node-id"),
        ])
        .unwrap();
        assert_eq!(config.dial_interval.as_millis(), 2000);
        assert!(config.fault_injection);
        assert_eq!(
            config.node_id_file,
            Some(std::path::PathBuf::from("/tmp/node-id"))
        );

        let defaults = ClusteringConfig::from_vars(std::iter::empty::<(&str, &str)>()).unwrap();
        assert_eq!(defaults.dial_interval.as_secs(), 15);
        assert!(!defaults.fault_injection);
        assert!(defaults.node_id_file.is_none());

        let err =
            ClusteringConfig::from_vars([("WADDLE_CLUSTERING_DIAL_INTERVAL_MS", "0")]).unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_DIAL_INTERVAL_MS"));
    }

    // FIX 6: `pod_template_hash` moved from a raw `std::env::var` read at the
    // clustering-bringup call site into this typed pipeline — trimmed, empty
    // ⇒ `None`, absent ⇒ `None`, never a parse failure.
    #[test]
    fn clustering_parses_pod_template_hash() {
        let config = ClusteringConfig::from_vars([(
            "WADDLE_CLUSTERING_POD_TEMPLATE_HASH",
            "  waddle-server-7f8b9c6d5-  ",
        )])
        .unwrap();
        assert_eq!(
            config.pod_template_hash.as_deref(),
            Some("waddle-server-7f8b9c6d5-")
        );

        let defaults = ClusteringConfig::from_vars(std::iter::empty::<(&str, &str)>()).unwrap();
        assert!(defaults.pod_template_hash.is_none());

        let blank =
            ClusteringConfig::from_vars([("WADDLE_CLUSTERING_POD_TEMPLATE_HASH", "   ")]).unwrap();
        assert!(
            blank.pod_template_hash.is_none(),
            "a blank/whitespace-only value must parse as absent, not an empty string"
        );
    }

    #[test]
    fn clustering_rejects_zero_heartbeat_interval() {
        let err = ClusteringConfig::from_vars([("WADDLE_CLUSTERING_HEARTBEAT_INTERVAL_MS", "0")])
            .unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_HEARTBEAT_INTERVAL_MS"));
    }

    #[test]
    fn clustering_defaults_have_a_valid_node_lease_and_self_fence_config() {
        let config = ClusteringConfig::from_vars(std::iter::empty::<(&str, &str)>()).unwrap();
        assert_eq!(config.node_lease, ClusteringNodeLeaseConfig::default());
        assert_eq!(config.self_fence, ClusteringSelfFenceConfig::default());
        assert_eq!(config.self_fence.isolation_intervals, 3);
    }

    #[test]
    fn clustering_parses_node_lease_timing() {
        let config = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_NODE_LEASE_HEARTBEAT_INTERVAL_MS", "5000"),
            ("WADDLE_CLUSTERING_NODE_LEASE_TTL_MS", "20000"),
        ])
        .unwrap();
        assert_eq!(config.node_lease.heartbeat_interval.as_millis(), 5000);
        assert_eq!(config.node_lease.lease_ttl.as_millis(), 20000);
    }

    #[test]
    fn clustering_defaults_claim_release_budget_to_five_seconds() {
        let config = ClusteringConfig::from_vars(std::iter::empty::<(&str, &str)>()).unwrap();
        assert_eq!(config.node_lease.claim_release_budget.as_secs(), 5);
    }

    #[test]
    fn clustering_parses_claim_release_budget() {
        let config =
            ClusteringConfig::from_vars([("WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS", "7500")])
                .unwrap();
        assert_eq!(config.node_lease.claim_release_budget.as_millis(), 7500);
    }

    #[test]
    fn clustering_rejects_zero_claim_release_budget() {
        let err = ClusteringConfig::from_vars([("WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS", "0")])
            .unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS"));
    }

    // --- ADR-0017 Phase 3 Slice 10 FIX 2 (council-adjudicated):
    // node_lease_ttl must comfortably exceed claim_release_budget, so a
    // draining node's heartbeat-during-drain renewal has real margin. ---

    #[test]
    fn clustering_rejects_node_lease_ttl_too_close_to_claim_release_budget() {
        let err = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_NODE_LEASE_HEARTBEAT_INTERVAL_MS", "1000"),
            ("WADDLE_CLUSTERING_NODE_LEASE_TTL_MS", "10000"),
            ("WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS", "5000"),
        ])
        .unwrap_err();
        assert!(
            err.contains("WADDLE_CLUSTERING_NODE_LEASE_TTL_MS")
                && err.contains("WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS"),
            "expected the node_lease_ttl-vs-claim_release_budget error, got: {err}"
        );
    }

    #[test]
    fn clustering_accepts_node_lease_ttl_with_comfortable_claim_release_budget_margin() {
        let config = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_NODE_LEASE_HEARTBEAT_INTERVAL_MS", "1000"),
            ("WADDLE_CLUSTERING_NODE_LEASE_TTL_MS", "15000"),
            ("WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS", "5000"),
            // Below the default 20s resume-handshake timeout's own
            // "<= node_lease_ttl" requirement — unrelated to this test's
            // own assertion, just needed to keep the 15s node_lease_ttl
            // above valid.
            ("WADDLE_CLUSTERING_RESUME_HANDSHAKE_TIMEOUT_MS", "10000"),
        ])
        .expect("exactly 3x the claim_release_budget must be accepted (the floor is inclusive)");
        assert_eq!(config.node_lease.lease_ttl.as_millis(), 15000);
        assert_eq!(config.node_lease.claim_release_budget.as_millis(), 5000);
    }

    #[test]
    fn clustering_defaults_satisfy_the_claim_release_budget_margin() {
        // The shipped defaults (30s lease_ttl, 5s claim_release_budget)
        // must themselves satisfy the new floor — this is a regression
        // guard against the defaults and the validation drifting apart.
        ClusteringConfig::from_vars(std::iter::empty::<(&str, &str)>())
            .expect("default clustering config must satisfy its own validation");
    }

    #[test]
    fn clustering_rejects_node_lease_ttl_below_two_heartbeats() {
        let err = ClusteringConfig::from_vars([
            (
                "WADDLE_CLUSTERING_NODE_LEASE_HEARTBEAT_INTERVAL_MS",
                "10000",
            ),
            ("WADDLE_CLUSTERING_NODE_LEASE_TTL_MS", "15000"),
        ])
        .unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_NODE_LEASE_TTL_MS"));
    }

    #[test]
    fn clustering_rejects_zero_node_lease_heartbeat_interval() {
        let err = ClusteringConfig::from_vars([(
            "WADDLE_CLUSTERING_NODE_LEASE_HEARTBEAT_INTERVAL_MS",
            "0",
        )])
        .unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_NODE_LEASE_HEARTBEAT_INTERVAL_MS"));
    }

    #[test]
    fn clustering_parses_self_fence_timing() {
        let config = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_ISOLATION_INTERVALS", "5"),
            ("WADDLE_CLUSTERING_REREGISTER_BACKOFF_BASE_MS", "500"),
            ("WADDLE_CLUSTERING_REREGISTER_BACKOFF_MAX_MS", "30000"),
        ])
        .unwrap();
        assert_eq!(config.self_fence.isolation_intervals, 5);
        assert_eq!(config.self_fence.reregister_backoff_base.as_millis(), 500);
        assert_eq!(config.self_fence.reregister_backoff_max.as_millis(), 30000);
    }

    #[test]
    fn clustering_rejects_zero_isolation_intervals() {
        let err = ClusteringConfig::from_vars([("WADDLE_CLUSTERING_ISOLATION_INTERVALS", "0")])
            .unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_ISOLATION_INTERVALS"));
    }

    #[test]
    fn clustering_rejects_reregister_backoff_max_below_base() {
        let err = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_REREGISTER_BACKOFF_BASE_MS", "5000"),
            ("WADDLE_CLUSTERING_REREGISTER_BACKOFF_MAX_MS", "1000"),
        ])
        .unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_REREGISTER_BACKOFF_MAX_MS"));
    }

    #[test]
    fn clustering_defaults_have_a_valid_steal_intent_config() {
        let config = ClusteringConfig::from_vars(std::iter::empty::<(&str, &str)>()).unwrap();
        assert_eq!(config.steal_intent, ClusteringStealIntentConfig::default());
    }

    #[test]
    fn clustering_parses_steal_intent_ttl() {
        let config = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_NODE_LEASE_HEARTBEAT_INTERVAL_MS", "5000"),
            ("WADDLE_CLUSTERING_STEAL_INTENT_TTL_MS", "30000"),
        ])
        .unwrap();
        assert_eq!(config.steal_intent.intent_ttl.as_millis(), 30000);
    }

    #[test]
    fn clustering_rejects_zero_steal_intent_ttl() {
        let err = ClusteringConfig::from_vars([("WADDLE_CLUSTERING_STEAL_INTENT_TTL_MS", "0")])
            .unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_STEAL_INTENT_TTL_MS"));
    }

    #[test]
    fn clustering_rejects_steal_intent_ttl_below_two_heartbeats() {
        let err = ClusteringConfig::from_vars([
            (
                "WADDLE_CLUSTERING_NODE_LEASE_HEARTBEAT_INTERVAL_MS",
                "10000",
            ),
            ("WADDLE_CLUSTERING_STEAL_INTENT_TTL_MS", "15000"),
        ])
        .unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_STEAL_INTENT_TTL_MS"));
    }

    #[test]
    fn clustering_defaults_have_a_valid_resume_handshake_config() {
        let config = ClusteringConfig::from_vars(std::iter::empty::<(&str, &str)>()).unwrap();
        assert_eq!(
            config.resume_handshake,
            ClusteringResumeHandshakeConfig::default()
        );
    }

    #[test]
    fn clustering_parses_resume_handshake_timeout() {
        let config = ClusteringConfig::from_vars([(
            "WADDLE_CLUSTERING_RESUME_HANDSHAKE_TIMEOUT_MS",
            "15000",
        )])
        .unwrap();
        assert_eq!(config.resume_handshake.timeout.as_millis(), 15000);
    }

    #[test]
    fn clustering_rejects_zero_resume_handshake_timeout() {
        let err =
            ClusteringConfig::from_vars([("WADDLE_CLUSTERING_RESUME_HANDSHAKE_TIMEOUT_MS", "0")])
                .unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_RESUME_HANDSHAKE_TIMEOUT_MS"));
    }

    #[test]
    fn clustering_rejects_resume_handshake_timeout_above_node_lease_ttl() {
        let err = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_NODE_LEASE_TTL_MS", "30000"),
            ("WADDLE_CLUSTERING_RESUME_HANDSHAKE_TIMEOUT_MS", "45000"),
        ])
        .unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_RESUME_HANDSHAKE_TIMEOUT_MS"));
    }

    #[test]
    fn ws_keepalive_defaults_are_45s_2_misses() {
        let config = ws_keepalive_from_vars(std::iter::empty::<(&str, &str)>()).unwrap();
        assert_eq!(config.interval_ms, 45_000);
        assert_eq!(config.miss_limit, 2);
    }

    #[test]
    fn ws_keepalive_parses_operator_overrides() {
        let config = ws_keepalive_from_vars([
            ("WADDLE_WS_KEEPALIVE_INTERVAL_SECS", "60"),
            ("WADDLE_WS_KEEPALIVE_MISS_LIMIT", "3"),
        ])
        .unwrap();
        assert_eq!(config.interval_ms, 60_000);
        assert_eq!(config.miss_limit, 3);
    }

    #[test]
    fn ws_keepalive_rejects_intervals_that_defeat_the_gateway_timeout() {
        // 2×interval must stay under the gateway's 300s stream-idle
        // timeout; anything above the 120s ceiling fails startup
        // instead of silently reintroducing the ~304s reset storm.
        for (bad, value) in [("0", 0), ("121", 121), ("300", 300)] {
            let err =
                ws_keepalive_from_vars([("WADDLE_WS_KEEPALIVE_INTERVAL_SECS", bad)]).unwrap_err();
            assert_eq!(
                err,
                WsKeepaliveConfigError::IntervalOutOfRange { value, max: 120 }
            );
            let rendered = err.to_string();
            assert!(
                rendered.contains("WADDLE_WS_KEEPALIVE_INTERVAL_SECS"),
                "diagnostic must name the env var; got: {rendered}"
            );
            assert!(
                rendered.contains("300s"),
                "diagnostic must explain the gateway constraint; got: {rendered}"
            );
        }
    }

    #[test]
    fn ws_keepalive_rejects_out_of_range_miss_limits() {
        for (bad, value) in [("0", 0), ("11", 11)] {
            let err =
                ws_keepalive_from_vars([("WADDLE_WS_KEEPALIVE_MISS_LIMIT", bad)]).unwrap_err();
            assert_eq!(
                err,
                WsKeepaliveConfigError::MissLimitOutOfRange { value, max: 10 }
            );
            assert!(
                err.to_string().contains("WADDLE_WS_KEEPALIVE_MISS_LIMIT"),
                "diagnostic must name the env var; got: {err}"
            );
        }
    }

    #[test]
    fn ws_keepalive_rejects_non_numeric_values() {
        let err =
            ws_keepalive_from_vars([("WADDLE_WS_KEEPALIVE_INTERVAL_SECS", "45s")]).unwrap_err();
        assert_eq!(
            err,
            WsKeepaliveConfigError::NotAnInteger {
                key: "WADDLE_WS_KEEPALIVE_INTERVAL_SECS",
                value: "45s".to_string()
            }
        );
        assert!(err.to_string().contains("must be a positive integer"));
    }

    #[test]
    fn parse_session_key_rejects_unset() {
        let err = parse_session_key(None).unwrap_err();
        assert!(
            err.contains("WADDLE_SESSION_KEY is required"),
            "error must name the env var; got: {err}"
        );
        assert!(
            err.contains("openssl rand"),
            "error must include the generation recipe; got: {err}"
        );
    }

    #[test]
    fn parse_session_key_rejects_empty() {
        let err = parse_session_key(Some("")).unwrap_err();
        assert!(
            err.contains("WADDLE_SESSION_KEY is required"),
            "empty key must be treated as unset; got: {err}"
        );
    }

    #[test]
    fn parse_session_key_accepts_value() {
        let value = "test-session-key-32-bytes-minimum";
        assert_eq!(parse_session_key(Some(value)).unwrap(), value);
    }

    #[test]
    fn parse_session_key_rejects_short_value() {
        let err = parse_session_key(Some("short")).unwrap_err();
        assert!(
            err.contains("at least"),
            "error must mention the length floor; got: {err}"
        );
        assert!(
            err.contains("openssl rand"),
            "error must include the generation recipe; got: {err}"
        );
    }

    #[test]
    fn link_preview_config_parses_operator_policy_vars() {
        let config = LinkPreviewConfig::from_vars([
            ("WADDLE_LINK_PREVIEW_ENABLED", "false"),
            (
                "WADDLE_LINK_PREVIEW_ALLOWED_HOSTS",
                "example.com,*.trusted.example",
            ),
            ("WADDLE_LINK_PREVIEW_BLOCKED_HOSTS", "ads.example"),
            ("WADDLE_LINK_PREVIEW_MAX_HTML_HEAD_BYTES", "4096"),
            ("WADDLE_LINK_PREVIEW_MAX_CACHED_IMAGE_BYTES", "8192"),
            ("WADDLE_LINK_PREVIEW_MAX_REDIRECTS", "2"),
            ("WADDLE_LINK_PREVIEW_FETCH_TIMEOUT_MS", "250"),
            ("WADDLE_LINK_PREVIEW_VIDEO_ENABLED", "0"),
        ])
        .expect("config");

        assert!(!config.enabled);
        assert_eq!(config.allowed_hosts.len(), 2);
        assert!(config.allowed_hosts[0].matches("example.com"));
        assert!(config.allowed_hosts[1].matches("cdn.trusted.example"));
        assert!(config.blocked_hosts[0].matches("ads.example"));
        assert_eq!(config.max_html_head_bytes, 4096);
        assert_eq!(config.max_cached_image_bytes, 8192);
        assert_eq!(config.max_redirects, 2);
        assert_eq!(config.fetch_timeout, Duration::from_millis(250));
        assert!(!config.video_enabled);
    }

    #[test]
    fn link_preview_host_patterns_reject_non_host_shapes() {
        let error = "https://example.com"
            .parse::<LinkPreviewHostPattern>()
            .expect_err("URL must not parse as host pattern");

        assert!(error.contains("invalid host pattern"));

        let error = "ads.*.example"
            .parse::<LinkPreviewHostPattern>()
            .expect_err("unsupported wildcard position must not parse");

        assert!(error.contains("invalid host pattern"));
    }

    #[test]
    fn link_preview_config_rejects_invalid_boolean_vars() {
        let error = LinkPreviewConfig::from_vars([("WADDLE_LINK_PREVIEW_ENABLED", "ture")])
            .expect_err("typo must fail startup");

        assert!(error.contains("WADDLE_LINK_PREVIEW_ENABLED"));
        assert!(error.contains("must be a boolean"));
    }

    #[test]
    fn link_preview_config_rejects_fetch_timeouts_above_startup_cap() {
        let error =
            LinkPreviewConfig::from_vars([("WADDLE_LINK_PREVIEW_FETCH_TIMEOUT_MS", "61000")])
                .expect_err("oversized timeout must fail startup");

        assert!(error.contains("WADDLE_LINK_PREVIEW_FETCH_TIMEOUT_MS"));
        assert!(error.contains("at most"));
    }

    #[test]
    fn parse_occupant_id_secret_rejects_unset() {
        let err = parse_occupant_id_secret(None).unwrap_err();
        assert!(
            err.contains("WADDLE_OCCUPANT_ID_SECRET is required"),
            "error must name the env var; got: {err}"
        );
        assert!(
            err.contains("openssl rand"),
            "error must include the generation recipe; got: {err}"
        );
    }

    #[test]
    fn parse_occupant_id_secret_rejects_short_value() {
        let err = parse_occupant_id_secret(Some("short")).unwrap_err();
        assert!(
            err.contains("at least"),
            "error must mention the length floor; got: {err}"
        );
        assert!(
            err.contains("openssl rand"),
            "error must include the generation recipe; got: {err}"
        );
    }

    #[test]
    fn parse_occupant_id_secret_accepts_minimum_length() {
        // Exactly the floor — must succeed.
        let value: String = "x".repeat(OCCUPANT_ID_SECRET_MIN_BYTES);
        let secret = parse_occupant_id_secret(Some(&value)).expect("32 bytes is accepted");
        assert_eq!(secret.key().len(), OCCUPANT_ID_SECRET_MIN_BYTES);
    }
}

/// Typed validation failure for `WADDLE_DB_*` (ADR-0017 element 12: pool
/// capacity is planned, not discovered — a fat-fingered size fails startup
/// instead of silently reverting to sqlx's own default).
///
/// Per the typed-payloads rule, error results are typed enums; `Display` is
/// the human-facing startup diagnostic surfaced by
/// [`DatabaseRuntimeConfig::from_env`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DatabaseRuntimeConfigError {
    /// `WADDLE_DB_DRIVER` is not `sqlite`/`postgres`/`postgresql`.
    #[error("invalid WADDLE_DB_DRIVER: {0}")]
    InvalidDriver(String),
    /// A `WADDLE_DB_*_POOL_SIZE` var is set but is not a base-10 unsigned
    /// integer.
    #[error("{key}='{value}' must be a positive integer")]
    NotAnInteger { key: &'static str, value: String },
    /// A pool size of 0 can never serve a connection.
    #[error("{key}={value} must be at least 1")]
    PoolSizeZero { key: &'static str, value: u32 },
    /// `WADDLE_DATABASE_URL` or `WADDLE_DB_DRIVER` is set but blank
    /// (empty or whitespace-only) after trimming. An empty-templated
    /// secret/value in a Postgres deployment must fail loudly, never
    /// silently fall back to the sqlite defaults — unset is the only
    /// condition that takes the default.
    #[error("{name} is set but empty; unset it to use the default, or provide a value")]
    EmptyVar { name: &'static str },
}

/// Errors while parsing the explicit database-lineage operator configuration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LineageConfigError {
    #[error("{name} is set but empty")]
    EmptyVar { name: &'static str },
    #[error("{name} is not a UUID: {value}")]
    InvalidUuid { name: &'static str, value: String },
    #[error("WADDLE_DB_LINEAGE_ACTION must be 'enroll' or 'adopt=<lineage-uuid>'")]
    InvalidAction,
}

/// Parsed configuration for an explicit lineage enrollment/adoption action.
///
/// An unset deployment UUID is permitted for normal verify-only startup, but
/// the engine refuses enrollment or adoption without one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LineageConfig {
    pub deployment_uuid: Option<crate::db::lineage::DeploymentUuid>,
    pub action: Option<crate::db::lineage::LineageAction>,
}

impl LineageConfig {
    pub fn from_env() -> Result<Self, LineageConfigError> {
        Self::from_vars(std::env::vars())
    }

    pub fn from_vars<I, K, V>(vars: I) -> Result<Self, LineageConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let vars = vars
            .into_iter()
            .map(|(key, value)| (key.as_ref().to_string(), value.as_ref().to_string()))
            .collect::<std::collections::HashMap<_, _>>();

        let deployment_uuid = match vars.get("WADDLE_DEPLOYMENT_UUID") {
            None => None,
            Some(value) => {
                let value = nonempty_lineage_var("WADDLE_DEPLOYMENT_UUID", value)?;
                Some(value.parse().map_err(|_| LineageConfigError::InvalidUuid {
                    name: "WADDLE_DEPLOYMENT_UUID",
                    value: value.to_string(),
                })?)
            }
        };

        let action = match vars.get("WADDLE_DB_LINEAGE_ACTION") {
            None => None,
            Some(value) => {
                let value = nonempty_lineage_var("WADDLE_DB_LINEAGE_ACTION", value)?;
                match value {
                    "enroll" => Some(crate::db::lineage::LineageAction::Enroll),
                    _ => {
                        let Some(expected) = value.strip_prefix("adopt=") else {
                            return Err(LineageConfigError::InvalidAction);
                        };
                        if expected.is_empty() {
                            return Err(LineageConfigError::InvalidAction);
                        }
                        let expected =
                            expected
                                .parse()
                                .map_err(|_| LineageConfigError::InvalidUuid {
                                    name: "WADDLE_DB_LINEAGE_ACTION",
                                    value: expected.to_string(),
                                })?;
                        Some(crate::db::lineage::LineageAction::Adopt(expected))
                    }
                }
            }
        };

        Ok(Self {
            deployment_uuid,
            action,
        })
    }
}

fn nonempty_lineage_var<'a>(
    name: &'static str,
    value: &'a str,
) -> Result<&'a str, LineageConfigError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(LineageConfigError::EmptyVar { name });
    }
    Ok(value)
}

#[cfg(test)]
mod lineage_config_tests {
    use super::*;

    #[test]
    fn lineage_config_defaults_to_verify_only() {
        assert_eq!(
            LineageConfig::from_vars(std::iter::empty::<(&str, &str)>()),
            Ok(LineageConfig::default())
        );
    }

    #[test]
    fn lineage_config_parses_deployment_and_actions() {
        let deployment = "018f47b2-4b2e-7a3a-9a4c-52a5a6a90001";
        let adopted = "018f47b2-4b2e-7a3a-9a4c-52a5a6a90002";
        let enroll_config = LineageConfig::from_vars([
            ("WADDLE_DEPLOYMENT_UUID", deployment),
            ("WADDLE_DB_LINEAGE_ACTION", "enroll"),
        ])
        .expect("parse enrollment config");
        assert!(matches!(
            enroll_config.action,
            Some(crate::db::lineage::LineageAction::Enroll)
        ));

        let adopt_config =
            LineageConfig::from_vars([("WADDLE_DB_LINEAGE_ACTION", &format!("adopt={adopted}"))])
                .expect("parse adoption config");
        assert!(matches!(
            adopt_config.action,
            Some(crate::db::lineage::LineageAction::Adopt(_))
        ));
    }

    #[test]
    fn lineage_config_rejects_blank_and_invalid_values() {
        assert!(matches!(
            LineageConfig::from_vars([("WADDLE_DEPLOYMENT_UUID", " ")]),
            Err(LineageConfigError::EmptyVar {
                name: "WADDLE_DEPLOYMENT_UUID"
            })
        ));
        assert!(matches!(
            LineageConfig::from_vars([("WADDLE_DEPLOYMENT_UUID", "invalid")]),
            Err(LineageConfigError::InvalidUuid {
                name: "WADDLE_DEPLOYMENT_UUID",
                ..
            })
        ));
        assert!(matches!(
            LineageConfig::from_vars([("WADDLE_DB_LINEAGE_ACTION", "adopt=")]),
            Err(LineageConfigError::InvalidAction)
        ));
    }
}

/// Runtime database driver + DSN + pool-sizing contract, parsed from
/// `WADDLE_DB_*`/`WADDLE_DATABASE_URL` (ADR-0017 element 12).
#[derive(Debug, Clone)]
pub struct DatabaseRuntimeConfig {
    pub driver: DatabaseDriver,
    pub database_url: String,
    /// Main/shared pool connection cap (`WADDLE_DB_POOL_SIZE`, default
    /// [`crate::db::DEFAULT_POOL_SIZE`]).
    pub pool_size: u32,
    /// Dedicated control-plane pool size (`WADDLE_DB_CONTROL_PLANE_POOL_SIZE`,
    /// default [`crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE`]). Only used
    /// when `driver` is Postgres — the clustering control plane has no
    /// SQLite equivalent, so this value is parsed and validated regardless
    /// of driver (a fat-fingered value should still fail fast) but is simply
    /// unused by the SQLite path.
    pub control_plane_pool_size: u32,
}

impl Default for DatabaseRuntimeConfig {
    fn default() -> Self {
        Self {
            driver: DatabaseDriver::Sqlite,
            database_url: "sqlite::memory:".to_string(),
            pool_size: crate::db::DEFAULT_POOL_SIZE,
            control_plane_pool_size: crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE,
        }
    }
}

impl DatabaseRuntimeConfig {
    pub fn from_env() -> Result<Self, DatabaseRuntimeConfigError> {
        Self::from_vars(std::env::vars())
    }

    pub fn from_vars<I, K, V>(vars: I) -> Result<Self, DatabaseRuntimeConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let vars = vars
            .into_iter()
            .map(|(key, value)| (key.as_ref().to_string(), value.as_ref().to_string()))
            .collect::<std::collections::HashMap<_, _>>();

        // Unset falls back to the sqlite default; set-but-blank (empty or
        // whitespace-only after trimming) is a typed error, never a silent
        // fallback — an empty-templated secret in a Postgres deployment must
        // never silently boot sqlite::memory:.
        let driver = match vars.get("WADDLE_DB_DRIVER") {
            None => DatabaseDriver::Sqlite,
            Some(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return Err(DatabaseRuntimeConfigError::EmptyVar {
                        name: "WADDLE_DB_DRIVER",
                    });
                }
                trimmed
                    .parse::<DatabaseDriver>()
                    .map_err(|e| DatabaseRuntimeConfigError::InvalidDriver(e.to_string()))?
            }
        };

        let database_url = match vars.get("WADDLE_DATABASE_URL") {
            None => match driver {
                DatabaseDriver::Sqlite => "sqlite::memory:".to_string(),
                DatabaseDriver::Postgres => {
                    "postgres://postgres:postgres@localhost:5432/waddle".to_string()
                }
            },
            Some(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return Err(DatabaseRuntimeConfigError::EmptyVar {
                        name: "WADDLE_DATABASE_URL",
                    });
                }
                trimmed.to_string()
            }
        };

        let pool_size =
            db_pool_size_var(&vars, "WADDLE_DB_POOL_SIZE", crate::db::DEFAULT_POOL_SIZE)?;
        let control_plane_pool_size = db_pool_size_var(
            &vars,
            "WADDLE_DB_CONTROL_PLANE_POOL_SIZE",
            crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE,
        )?;

        Ok(Self {
            driver,
            database_url,
            pool_size,
            control_plane_pool_size,
        })
    }
}

/// Read a `WADDLE_DB_*_POOL_SIZE` var as `u32`, treating unset/blank as the
/// default and rejecting zero (a zero-sized pool can never serve a
/// connection).
fn db_pool_size_var(
    vars: &std::collections::HashMap<String, String>,
    key: &'static str,
    default: u32,
) -> Result<u32, DatabaseRuntimeConfigError> {
    match vars
        .get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        None => Ok(default),
        Some(raw) => {
            let value =
                raw.parse::<u32>()
                    .map_err(|_| DatabaseRuntimeConfigError::NotAnInteger {
                        key,
                        value: raw.to_string(),
                    })?;
            if value == 0 {
                return Err(DatabaseRuntimeConfigError::PoolSizeZero { key, value });
            }
            Ok(value)
        }
    }
}

#[cfg(test)]
mod database_runtime_config_tests {
    use super::*;

    #[test]
    fn defaults_are_sqlite_in_memory_with_historical_pool_sizes() {
        let config = DatabaseRuntimeConfig::from_vars(std::iter::empty::<(&str, &str)>()).unwrap();
        assert_eq!(config.driver, DatabaseDriver::Sqlite);
        assert_eq!(config.database_url, "sqlite::memory:");
        assert_eq!(config.pool_size, crate::db::DEFAULT_POOL_SIZE);
        assert_eq!(
            config.control_plane_pool_size,
            crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE
        );
    }

    #[test]
    fn parses_pool_size_overrides() {
        let config = DatabaseRuntimeConfig::from_vars([
            ("WADDLE_DB_DRIVER", "postgres"),
            ("WADDLE_DB_POOL_SIZE", "25"),
            ("WADDLE_DB_CONTROL_PLANE_POOL_SIZE", "6"),
        ])
        .unwrap();
        assert_eq!(config.driver, DatabaseDriver::Postgres);
        assert_eq!(config.pool_size, 25);
        assert_eq!(config.control_plane_pool_size, 6);
    }

    #[test]
    fn rejects_zero_pool_size() {
        let err = DatabaseRuntimeConfig::from_vars([("WADDLE_DB_POOL_SIZE", "0")]).unwrap_err();
        assert!(matches!(
            err,
            DatabaseRuntimeConfigError::PoolSizeZero {
                key: "WADDLE_DB_POOL_SIZE",
                value: 0
            }
        ));
    }

    #[test]
    fn rejects_zero_control_plane_pool_size() {
        let err = DatabaseRuntimeConfig::from_vars([("WADDLE_DB_CONTROL_PLANE_POOL_SIZE", "0")])
            .unwrap_err();
        assert!(matches!(
            err,
            DatabaseRuntimeConfigError::PoolSizeZero {
                key: "WADDLE_DB_CONTROL_PLANE_POOL_SIZE",
                value: 0
            }
        ));
    }

    #[test]
    fn rejects_non_integer_pool_size() {
        let err = DatabaseRuntimeConfig::from_vars([("WADDLE_DB_POOL_SIZE", "ten")]).unwrap_err();
        assert!(matches!(
            err,
            DatabaseRuntimeConfigError::NotAnInteger {
                key: "WADDLE_DB_POOL_SIZE",
                ..
            }
        ));
    }

    #[test]
    fn rejects_invalid_driver() {
        let err = DatabaseRuntimeConfig::from_vars([("WADDLE_DB_DRIVER", "mysql")]).unwrap_err();
        assert!(matches!(err, DatabaseRuntimeConfigError::InvalidDriver(_)));
    }

    #[test]
    fn rejects_empty_db_driver() {
        for blank in ["", "  "] {
            let err = DatabaseRuntimeConfig::from_vars([("WADDLE_DB_DRIVER", blank)]).unwrap_err();
            assert!(
                matches!(
                    err,
                    DatabaseRuntimeConfigError::EmptyVar {
                        name: "WADDLE_DB_DRIVER"
                    }
                ),
                "blank {blank:?}: {err}"
            );
        }
    }

    #[test]
    fn rejects_empty_database_url() {
        for blank in ["", "  "] {
            let err =
                DatabaseRuntimeConfig::from_vars([("WADDLE_DATABASE_URL", blank)]).unwrap_err();
            assert!(
                matches!(
                    err,
                    DatabaseRuntimeConfigError::EmptyVar {
                        name: "WADDLE_DATABASE_URL"
                    }
                ),
                "blank {blank:?}: {err}"
            );
        }
    }

    #[test]
    fn unset_db_driver_and_database_url_still_use_defaults() {
        // Byte-for-byte-identical guarantee: unset (never set-but-blank)
        // must still take the sqlite defaults.
        let config = DatabaseRuntimeConfig::from_vars(std::iter::empty::<(&str, &str)>()).unwrap();
        assert_eq!(config.driver, DatabaseDriver::Sqlite);
        assert_eq!(config.database_url, "sqlite::memory:");
    }
}
