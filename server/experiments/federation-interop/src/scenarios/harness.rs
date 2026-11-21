//! The running environment of one harness run and the helpers the rows
//! share: logins, relays, configuration profiles and restarts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::client::session::{Account, Session};
use crate::dns::{StubDns, View};
use crate::evidence::Unverifiable;
use crate::manifest::Peer;
use crate::net::http::FetchRecorder;
use crate::net::relay::{Relay, TapIdentities};
use crate::net::tls_front::TlsFront;
use crate::pki::{Leaf, LeafSpec, Pki};
use crate::s2s::receiver::{Behaviour, Receiver, Settings};
use crate::services::ejabberd::{self, Ejabberd};
use crate::services::postgres::Postgres;
use crate::services::prosody::{self, Prosody};
use crate::services::spicedb::SpiceDb;
use crate::services::waddle::{self, S2s, Spec, Waddle};
use crate::services::waddle_config::{Material, Profile};
use crate::topology::{
    self, Home, Link, Ports, Server, DENIED, DENIED_ALABEL, EJABBERD, EJABBERD_INSECURE,
    EJABBERD_MUC, EJABBERD_PUBSUB, FLOOD_PEERS, PROSODY, PROSODY_INSECURE, PROSODY_MUC,
    PROSODY_PUBSUB, PUBLIC_PROFILE, RAW_PEER, REDIRECT,
};

/// Paths of the binaries, from the native shell's environment.
#[derive(Debug, Clone)]
pub struct Binaries {
    pub waddle: PathBuf,
    pub postgres: PathBuf,
    pub spicedb: PathBuf,
    pub prosody: PathBuf,
    pub ejabberd: PathBuf,
    pub erlang: PathBuf,
    pub openssl: PathBuf,
}

fn env_path(name: &str) -> Result<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .with_context(|| format!("{name} is not set; run through run-native.sh"))
}

impl Binaries {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            waddle: env_path("INTEROP_WADDLE_BIN")?,
            postgres: env_path("INTEROP_POSTGRES_BIN")?,
            spicedb: env_path("INTEROP_SPICEDB_BIN")?,
            prosody: env_path("INTEROP_PROSODY_BIN")?,
            ejabberd: env_path("INTEROP_EJABBERD_BIN")?,
            erlang: env_path("INTEROP_ERLANG_BIN")?,
            openssl: env_path("INTEROP_OPENSSL_BIN")?,
        })
    }
}

/// Every domain the harness issues a leaf for.
fn leaf_domains() -> Vec<String> {
    let mut domains = Vec::new();
    for home in [Home::A, Home::B] {
        domains.extend(topology::waddle_domains(home.domain()));
    }
    for domain in [
        PROSODY,
        PROSODY_INSECURE,
        PROSODY_MUC,
        PROSODY_PUBSUB,
        EJABBERD,
        EJABBERD_INSECURE,
        EJABBERD_MUC,
        EJABBERD_PUBSUB,
        RAW_PEER,
        REDIRECT,
        PUBLIC_PROFILE,
        DENIED,
        DENIED_ALABEL,
    ] {
        domains.push(domain.to_owned());
    }
    domains.extend(FLOOD_PEERS.iter().map(|domain| (*domain).to_owned()));
    domains
}

pub struct Harness {
    pub ports: Ports,
    pub root: PathBuf,
    pub logs: PathBuf,
    pub pki: Arc<Pki>,
    /// The standard leaf of every harness domain.
    pub leaves: BTreeMap<String, Leaf>,
    pub bins: Binaries,
    pub material: BTreeMap<Home, Material>,
    pub postgres: BTreeMap<Home, Postgres>,
    pub spicedb: BTreeMap<Home, SpiceDb>,
    pub waddle: BTreeMap<Home, Waddle>,
    pub prosody: Prosody,
    /// Why Prosody is unavailable, if it is.
    pub prosody_down: Option<String>,
    pub ejabberd: Ejabberd,
    pub ejabberd_down: Option<String>,
    /// Why a Waddle deployment is unavailable, if one did not restart.
    pub waddle_down: Option<String>,
    pub relays: BTreeMap<Link, Relay>,
    pub dns: Vec<StubDns>,
    pub receiver: Receiver,
    pub redirect: Receiver,
    pub redirect_target: Relay,
    pub fetches: FetchRecorder,
    fronts: Vec<TlsFront>,
    /// Services a row left on a non-standard configuration.
    pub dirty: BTreeSet<Component>,
    /// How many times ejabberd's relay to home-a had been cut when the
    /// standard configuration was last restored.
    ejabberd_cuts: u64,
    pub git_sha: String,
}

impl Harness {
    pub async fn prepare(
        root: &Path,
        ports: Ports,
        bins: Binaries,
        git_sha: String,
        enrolled_space_fixture: bool,
    ) -> Result<Self> {
        let logs = root.join("logs");
        std::fs::create_dir_all(&logs)?;
        let pki = Arc::new(Pki::generate(&root.join("pki"))?);
        let mut leaves = BTreeMap::new();
        for domain in leaf_domains() {
            let leaf = pki.issue(&LeafSpec::new(&domain, &[domain.as_str()]))?;
            leaves.insert(domain, leaf);
        }
        let mut material = BTreeMap::new();
        for home in [Home::A, Home::B] {
            let secret = root.join([home.label(), "-dialback-secret"].concat());
            crate::process::write_private(
                &secret,
                uuid::Uuid::new_v4()
                    .simple()
                    .to_string()
                    .repeat(2)
                    .as_bytes(),
            )?;
            let mut credentials = BTreeMap::new();
            for domain in topology::waddle_domains(home.domain()) {
                let leaf = leaves.get(&domain).context("a Waddle leaf")?;
                credentials.insert(
                    domain,
                    (leaf.certificate_path.clone(), leaf.key_path.clone()),
                );
            }
            material.insert(
                home,
                Material {
                    credentials,
                    ca: pki.ca_path.clone(),
                    crl: pki.crl_path.clone(),
                    dialback_secret: secret,
                },
            );
        }

        let mut relays = BTreeMap::new();
        for link in topology::RELAYS {
            let relay = Relay::start(
                link.label(),
                ports.relay(link),
                topology::s2s_port(ports, link.to),
                link.from.relay_source(),
            )
            .await?;
            relays.insert(link, relay);
        }
        let mut dns = Vec::new();
        for (source, port) in [
            (Server::Prosody, ports.dns_for_prosody()),
            (Server::Ejabberd(1), ports.dns_for_ejabberd()),
        ] {
            let mut view = View::default();
            for home in [Home::A, Home::B] {
                if let Some(link) = topology::relay_for(source, Server::Waddle(home)) {
                    for domain in topology::waddle_domains(home.domain()) {
                        view.services.insert(domain, ports.relay(link));
                    }
                }
            }
            // The third parties reach each other directly.
            let (other, other_port) = match source {
                Server::Prosody => (
                    vec![EJABBERD, EJABBERD_INSECURE, EJABBERD_MUC, EJABBERD_PUBSUB],
                    ports.ejabberd_s2s(1),
                ),
                _ => (
                    vec![PROSODY, PROSODY_INSECURE, PROSODY_MUC, PROSODY_PUBSUB],
                    ports.prosody_s2s(),
                ),
            };
            for domain in other {
                view.services.insert(domain.to_owned(), other_port);
            }
            view.services
                .insert(RAW_PEER.to_owned(), ports.raw_receiver());
            if let Server::Ejabberd(_) = source {
                view.srv_target = Some("localhost".to_owned());
            }
            dns.push(StubDns::start(port, view).await?);
        }
        let mut fronts = Vec::new();
        for home in [Home::A, Home::B] {
            let leaf = leaves.get(home.domain()).context("a home leaf")?;
            fronts.push(
                TlsFront::start(&pki, leaf, ports.waddle_wss(home), ports.waddle_http(home))
                    .await?,
            );
        }
        let authoritative: BTreeMap<String, u16> = [Home::A, Home::B]
            .into_iter()
            .flat_map(|home| {
                topology::waddle_domains(home.domain())
                    .into_iter()
                    .map(move |domain| (domain, ports.waddle_s2s(home)))
            })
            .collect();
        let receiver = Receiver::start(
            Arc::clone(&pki),
            ports.raw_receiver(),
            Settings {
                behaviour: Behaviour::External,
                versions: crate::pki::all_versions(),
                leaves: leaves.clone(),
                authoritative: authoritative.clone(),
            },
        )
        .await?;
        let redirect = Receiver::start(
            Arc::clone(&pki),
            ports.redirect_receiver(),
            Settings {
                behaviour: Behaviour::RedirectBeforeTls(
                    ["127.0.0.1:", &ports.redirect_target().to_string()].concat(),
                ),
                versions: crate::pki::all_versions(),
                leaves: leaves.clone(),
                authoritative,
            },
        )
        .await?;
        // Anything that follows a redirect lands here and is counted.
        let redirect_target = Relay::start(
            "redirect-target".to_owned(),
            ports.redirect_target(),
            ports.raw_receiver(),
            std::net::Ipv4Addr::LOCALHOST,
        )
        .await?;
        let fetches = FetchRecorder::start(ports.fetch_recorder()).await?;

        let mut postgres = BTreeMap::new();
        let mut spicedb = BTreeMap::new();
        for home in [Home::A, Home::B] {
            let cluster = Postgres::start(
                &bins.postgres,
                root,
                &logs,
                &[home.label(), "-postgres"].concat(),
                ports.postgres(home),
                enrolled_space_fixture && home == Home::A,
            )
            .await?;
            cluster.create_database("waddle").await?;
            cluster.create_database("spicedb").await?;
            let spice = SpiceDb::start(
                &bins.spicedb,
                &logs,
                &[home.label(), "-spicedb"].concat(),
                ports.spicedb(home),
                cluster.url("spicedb"),
            )
            .await?;
            postgres.insert(home, cluster);
            spicedb.insert(home, spice);
        }
        let mut waddles = BTreeMap::new();
        for home in [Home::A, Home::B] {
            let cluster = postgres.get(&home).context("a cluster")?;
            let spice = spicedb.get(&home).context("a SpiceDB")?;
            let passwords = waddle::ACCOUNTS
                .iter()
                .map(|account| {
                    (
                        (*account).to_owned(),
                        uuid::Uuid::new_v4().simple().to_string(),
                    )
                })
                .collect();
            let upload_dir = root.join([home.label(), "-uploads"].concat());
            std::fs::create_dir_all(&upload_dir)?;
            let spec = Spec {
                home,
                binary: bins.waddle.clone(),
                database_url: cluster.url("waddle"),
                spicedb_endpoint: spice.endpoint(),
                spicedb_key: spice.preshared_key.clone(),
                passwords,
                deployment_uuid: uuid::Uuid::new_v4().to_string(),
                session_key: uuid::Uuid::new_v4().simple().to_string().repeat(2),
                occupant_secret: uuid::Uuid::new_v4().simple().to_string().repeat(2),
                upload_dir,
                git_sha: git_sha.clone(),
            };
            waddles.insert(home, Waddle::new(spec, &logs));
        }
        let prosody = Prosody::new(
            &bins.prosody,
            root,
            &logs,
            ports,
            &pki.ca_path,
            &pki.other_ca_path,
        );
        let ejabberd = Ejabberd::new(
            &bins.ejabberd,
            &bins.erlang,
            root,
            &logs,
            ports,
            &pki.ca_path,
            &pki.other_ca_path,
        );
        Ok(Self {
            ports,
            root: root.to_owned(),
            logs,
            pki,
            leaves,
            bins,
            material,
            postgres,
            spicedb,
            waddle: waddles,
            prosody,
            prosody_down: Some("not started".to_owned()),
            ejabberd,
            ejabberd_down: Some("not started".to_owned()),
            waddle_down: None,
            relays,
            dns,
            receiver,
            redirect,
            redirect_target,
            fetches,
            fronts,
            dirty: BTreeSet::new(),
            ejabberd_cuts: 0,
            git_sha,
        })
    }

    pub fn leaf(&self, domain: &str) -> Result<Leaf> {
        self.leaves
            .get(domain)
            .cloned()
            .with_context(|| format!("no leaf for {domain}"))
    }

    /// Issue an extra leaf.
    pub fn issue(&self, spec: &LeafSpec) -> Result<Leaf> {
        self.pki.issue(spec)
    }

    pub fn profile(&self, home: Home, trust_third_parties: bool) -> Profile {
        Profile::standard(home, self.ports, trust_third_parties)
    }

    /// Write `profile` for its deployment and return the file.
    pub fn write_profile(&self, profile: &Profile) -> Result<PathBuf> {
        let material = self.material.get(&profile.home).context("material")?;
        profile.write(&self.root, self.ports, material)
    }

    pub fn waddle(&mut self, home: Home) -> Result<&mut Waddle> {
        self.waddle.get_mut(&home).context("a deployment")
    }

    /// (Re)start a deployment with S2S on `profile`.
    pub async fn start_waddle(&mut self, profile: &Profile) -> Result<()> {
        let config = self.write_profile(profile)?;
        let ports = self.ports;
        let waddle = self.waddle(profile.home)?;
        waddle.stop().await?;
        waddle.start(ports, S2s::Enabled { config }).await
    }

    /// (Re)start a deployment with S2S on `profile` and other credentials.
    pub async fn start_waddle_with(
        &mut self,
        profile: &Profile,
        material: &Material,
    ) -> Result<()> {
        let config = profile.write(&self.root, self.ports, material)?;
        let ports = self.ports;
        let waddle = self.waddle(profile.home)?;
        waddle.stop().await?;
        waddle.start(ports, S2s::Enabled { config }).await
    }

    pub async fn start_standard_waddles(&mut self) -> Result<()> {
        for home in [Home::A, Home::B] {
            let profile = self.profile(home, true);
            self.start_waddle(&profile).await?;
        }
        Ok(())
    }

    pub fn prosody_variant(&self) -> prosody::Variant {
        prosody::Variant {
            leaves: [PROSODY, PROSODY_INSECURE, PROSODY_MUC, PROSODY_PUBSUB]
                .iter()
                .filter_map(|domain| {
                    Some(((*domain).to_owned(), self.leaves.get(*domain)?.clone()))
                })
                .collect(),
            tls: prosody::TlsProfile::Default,
            certificate_less_outbound: None,
        }
    }

    pub fn ejabberd_variant(&self) -> ejabberd::Variant {
        ejabberd::Variant {
            leaves: [EJABBERD, EJABBERD_INSECURE, EJABBERD_MUC, EJABBERD_PUBSUB]
                .iter()
                .filter_map(|domain| {
                    Some(((*domain).to_owned(), self.leaves.get(*domain)?.clone()))
                })
                .collect(),
            tls: ejabberd::TlsProfile::Default,
            certificate_less_node_two: false,
        }
    }

    pub async fn start_third_parties(&mut self) {
        let variant = self.prosody_variant();
        self.prosody_down = match self.prosody.start(&variant).await {
            Ok(()) => None,
            Err(error) => Some(format!("{error:#}\n{}", self.prosody.log_tail())),
        };
        let variant = self.ejabberd_variant();
        self.ejabberd_down = match self.ejabberd.start(&variant).await {
            Ok(()) => None,
            Err(error) => Some(format!("{error:#}\n{}", self.ejabberd.log_tail(1))),
        };
    }

    /// Put every service a row changed back on its standard
    /// configuration, and every relay and harness peer back to normal. A
    /// service that does not come back is marked down, so later rows
    /// report UNVERIFIED instead of failing on the environment. A stock
    /// peer that was restarted is then settled ([`Harness::settle_stock`]).
    pub async fn restore_standard(&mut self) -> Result<()> {
        for relay in self.relays.values() {
            relay.restore();
        }
        self.receiver.set_behaviour(Behaviour::External);
        self.receiver.set_versions(crate::pki::all_versions());
        self.redirect.set_behaviour(Behaviour::RedirectBeforeTls(
            ["127.0.0.1:", &self.ports.redirect_target().to_string()].concat(),
        ));
        for (domain, leaf) in self.leaves.clone() {
            self.receiver.set_leaf(&domain, leaf);
        }
        // ejabberd bounces a pair's traffic for minutes after a stream to
        // home-a closes before it is established, which a cut of its relay
        // (a tap row, a fresh-stream control) can cause; a row that cut it
        // leaves a cluster restarted and settled for the next one.
        if let Ok(relay) = self.relay(Server::Ejabberd(1), Server::Waddle(Home::A)) {
            let cuts = relay.cuts();
            if cuts != self.ejabberd_cuts {
                self.dirty.insert(Component::Ejabberd);
            }
        }
        let dirty = std::mem::take(&mut self.dirty);
        let mut failures = Vec::new();
        let mut restarted = Vec::new();
        for component in dirty {
            match component {
                Component::Waddle(home) => {
                    let profile = self.profile(home, true);
                    if let Err(error) = self.start_waddle(&profile).await {
                        let reason = format!("{} did not restart: {error:#}", home.label());
                        self.waddle_down = Some(reason.clone());
                        failures.push(reason);
                    }
                }
                Component::Prosody if self.prosody_down.is_none() => {
                    let variant = self.prosody_variant();
                    if let Err(error) = self.prosody.restart(&variant).await {
                        let reason = format!("Prosody did not restart: {error:#}");
                        self.prosody_down = Some(reason.clone());
                        failures.push(reason);
                    } else {
                        restarted.push(Peer::Prosody);
                    }
                }
                Component::Ejabberd if self.ejabberd_down.is_none() => {
                    let variant = self.ejabberd_variant();
                    if let Err(error) = self.ejabberd.restart(&variant).await {
                        let reason = format!("ejabberd did not restart: {error:#}");
                        self.ejabberd_down = Some(reason.clone());
                        failures.push(reason);
                    } else {
                        restarted.push(Peer::Ejabberd);
                    }
                }
                Component::Prosody | Component::Ejabberd => {}
            }
        }
        if self.waddle_down.is_none() {
            for peer in restarted {
                if let Err(reason) = self.settle_stock(peer).await {
                    failures.push(reason);
                }
            }
        }
        if let Ok(relay) = self.relay(Server::Ejabberd(1), Server::Waddle(Home::A)) {
            self.ejabberd_cuts = relay.cuts();
        }
        anyhow::ensure!(failures.is_empty(), "{}", failures.join("; "));
        Ok(())
    }

    /// After a stock peer restarted, wait until home-a reaches it and it
    /// reaches home-a. While the peer was down or presented another
    /// certificate, home-a backed off the traffic it failed to deliver
    /// (from one second, doubling) and the peer may have bounced its own
    /// traffic to home-a for a while; the restart clears the peer's state,
    /// and this waits out home-a's, so later rows inherit neither. A peer
    /// that is not reached both ways within [`SETTLE`] is reported, not
    /// marked down.
    pub async fn settle_stock(&self, peer: Peer) -> Result<(), String> {
        let host = Self::peer_domain(peer);
        for (from_home_a, to) in [(true, host), (false, topology::HOME_A)] {
            let answer =
                crate::scenarios::support::ping_until_result(self, peer, from_home_a, to, SETTLE)
                    .await;
            match answer {
                Ok(element) if crate::scenarios::support::is_result(&element) => {}
                other => {
                    let direction = if from_home_a {
                        ["home-a -> ", host].concat()
                    } else {
                        [peer.label(), " -> home-a"].concat()
                    };
                    return Err(format!(
                        "{direction} not answered within {} s after the restart: {}",
                        SETTLE.as_secs(),
                        other
                            .map(|element| crate::scenarios::support::describe(&element))
                            .unwrap_or_else(|error| format!("{error:#}"))
                    ));
                }
            }
        }
        Ok(())
    }

    /// Whether `peer` can be driven, or why not.
    pub fn available(&self, peer: Peer) -> Result<(), String> {
        if let Some(reason) = &self.waddle_down {
            return Err(["a Waddle deployment is down: ", reason].concat());
        }
        match peer {
            Peer::Waddle => Ok(()),
            Peer::Prosody => match &self.prosody_down {
                Some(reason) => Err(["Prosody is not running: ", reason].concat()),
                None => Ok(()),
            },
            Peer::Ejabberd => match &self.ejabberd_down {
                Some(reason) => Err(["ejabberd is not running: ", reason].concat()),
                None => Ok(()),
            },
        }
    }

    /// The peer's account domain: `home-b.test` for the Waddle column.
    pub fn peer_domain(peer: Peer) -> &'static str {
        match peer {
            Peer::Waddle => topology::HOME_B,
            Peer::Prosody => PROSODY,
            Peer::Ejabberd => EJABBERD,
        }
    }

    pub fn peer_muc(peer: Peer) -> String {
        match peer {
            Peer::Waddle => topology::muc_domain(topology::HOME_B),
            Peer::Prosody => PROSODY_MUC.to_owned(),
            Peer::Ejabberd => EJABBERD_MUC.to_owned(),
        }
    }

    pub fn peer_pubsub(peer: Peer) -> String {
        match peer {
            Peer::Waddle => topology::spaces_domain(topology::HOME_B),
            Peer::Prosody => PROSODY_PUBSUB.to_owned(),
            Peer::Ejabberd => EJABBERD_PUBSUB.to_owned(),
        }
    }

    /// The server the peer's S2S traffic originates from.
    pub fn peer_server(peer: Peer) -> Server {
        match peer {
            Peer::Waddle => Server::Waddle(Home::B),
            Peer::Prosody => Server::Prosody,
            Peer::Ejabberd => Server::Ejabberd(1),
        }
    }

    pub fn relay(&self, from: Server, to: Server) -> Result<&Relay> {
        let link = topology::relay_for(from, to).context("no relay for that path")?;
        self.relays.get(&link).context("relay not started")
    }

    /// A WSS session on a Waddle deployment.
    pub async fn login_waddle(&self, home: Home, account: &str, resource: &str) -> Result<Session> {
        let waddle = self.waddle.get(&home).context("a deployment")?;
        let account = Account {
            domain: home.domain().to_owned(),
            username: account.to_owned(),
            password: waddle.spec.password(account),
        };
        let label = [home.label(), ":", &account.username, "/", resource].concat();
        Session::wss(
            &self.pki,
            self.ports.waddle_wss(home),
            &account,
            resource,
            &label,
        )
        .await
    }

    /// A session on the peer's own server: home-b, Prosody or ejabberd
    /// node 1.
    pub async fn login_peer(&self, peer: Peer, account: &str, resource: &str) -> Result<Session> {
        match peer {
            Peer::Waddle => self.login_waddle(Home::B, account, resource).await,
            Peer::Prosody => {
                if let Some(reason) = &self.prosody_down {
                    return Err(Unverifiable(["Prosody is not running: ", reason].concat()).into());
                }
                let account = Account {
                    domain: PROSODY.to_owned(),
                    username: account.to_owned(),
                    password: self
                        .prosody
                        .passwords
                        .get(account)
                        .cloned()
                        .unwrap_or_default(),
                };
                let label = ["prosody:", &account.username, "/", resource].concat();
                Session::tcp(self.ports.prosody_c2s(), &account, resource, &label).await
            }
            Peer::Ejabberd => self.login_ejabberd(1, account, resource).await,
        }
    }

    pub async fn login_ejabberd(&self, node: u8, account: &str, resource: &str) -> Result<Session> {
        if let Some(reason) = &self.ejabberd_down {
            return Err(Unverifiable(["ejabberd is not running: ", reason].concat()).into());
        }
        let account = Account {
            domain: EJABBERD.to_owned(),
            username: account.to_owned(),
            password: self
                .ejabberd
                .passwords
                .get(account)
                .cloned()
                .unwrap_or_default(),
        };
        let label = [
            "ejabberd",
            &node.to_string(),
            ":",
            &account.username,
            "/",
            resource,
        ]
        .concat();
        Session::tcp(self.ports.ejabberd_c2s(node), &account, resource, &label).await
    }

    /// Tap identities: every harness leaf by domain.
    pub fn tap_identities(&self) -> TapIdentities {
        TapIdentities {
            leaves: self.leaves.clone(),
        }
    }

    pub async fn shutdown(&mut self) {
        let _ = self.ejabberd.stop().await;
        let _ = self.prosody.stop().await;
        for waddle in self.waddle.values_mut() {
            let _ = waddle.stop().await;
        }
        for spice in self.spicedb.values_mut() {
            let _ = spice.stop().await;
        }
        for cluster in self.postgres.values_mut() {
            let _ = cluster.stop().await;
        }
        self.fronts.clear();
    }
}

/// A service whose configuration a row may change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Component {
    Waddle(Home),
    Prosody,
    Ejabberd,
}

/// How long a flow waits for one answer across S2S.
pub const ANSWER: Duration = Duration::from_secs(20);
/// How long a negative check waits for something that must not happen.
pub const QUIET: Duration = Duration::from_secs(5);

/// How long [`Harness::settle_stock`] waits for each direction after a
/// stock peer restarted: home-a's backoff after a few failed dials, and
/// the peer's own start.
const SETTLE: Duration = Duration::from_secs(90);
