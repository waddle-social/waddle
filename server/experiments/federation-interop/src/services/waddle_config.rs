//! The S2S configuration file of a Waddle deployment
//! (`WADDLE_S2S_CONFIG_FILE`, server/docs/planning/s2s-connection-manager-2047.md),
//! serialized from typed tables so every value is quoted by the TOML
//! writer. Paths are absolute, as the loader requires.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::topology::{
    self, Home, Link, Ports, Server, DENIED, EJABBERD, EJABBERD_MUC, EJABBERD_PUBSUB, PROSODY,
    PROSODY_INSECURE, PROSODY_MUC, PROSODY_PUBSUB, PUBLIC_PROFILE, RAW_PEER, REDIRECT,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Closed,
    Allowlist,
    Open,
}

#[derive(Debug, Serialize)]
struct Document {
    generation: String,
    listen: String,
    local_domains: Vec<String>,
    peer_scope: &'static str,
    credentials: Vec<Credential>,
    private_ca: Vec<PrivateCa>,
    endpoints: Vec<Endpoint>,
    admission: Admission,
    limits: BTreeMap<String, u64>,
    dialback: Dialback,
}

#[derive(Debug, Serialize)]
struct Credential {
    domain: String,
    certificate_chain: PathBuf,
    private_key: PathBuf,
}

#[derive(Debug, Serialize)]
struct PrivateCa {
    domains: Vec<String>,
    anchors: Vec<PathBuf>,
    crls: Vec<PathBuf>,
}

#[derive(Debug, Serialize)]
struct Endpoint {
    domain: String,
    address: String,
}

#[derive(Debug, Serialize)]
struct Admission {
    mode: Mode,
    allow: Vec<String>,
    deny: Vec<String>,
    dialback: Vec<String>,
    trusted_homes: Vec<String>,
    require_private_ca: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Dialback {
    outbound: bool,
    inbound: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    secret_file: Option<PathBuf>,
}

/// One deployment's admission and trust, before it is written.
#[derive(Debug, Clone)]
pub struct Profile {
    pub home: Home,
    pub mode: Mode,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub dialback: Vec<String>,
    pub trusted_homes: Vec<String>,
    pub require_private_ca: Vec<String>,
    /// Remote domains whose certificates the `.test` CA issues.
    pub private_ca: Vec<String>,
    pub endpoints: Vec<(String, u16)>,
    pub dialback_inbound: bool,
    pub dialback_outbound: bool,
    pub limits: BTreeMap<String, u64>,
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

/// The remote domains of the other deployment and both third parties.
fn peers(home: Home) -> Vec<String> {
    let mut domains: Vec<String> = topology::waddle_domains(home.other().domain()).to_vec();
    domains.extend(strings(&[
        PROSODY,
        PROSODY_INSECURE,
        PROSODY_MUC,
        PROSODY_PUBSUB,
        EJABBERD,
        topology::EJABBERD_INSECURE,
        EJABBERD_MUC,
        EJABBERD_PUBSUB,
        RAW_PEER,
        REDIRECT,
    ]));
    domains
}

impl Profile {
    /// The harness's standard profile: allowlist mode admitting the other
    /// deployment, both third parties and the harness's own domains, each
    /// on the `.test` CA's private-CA profile except
    /// [`PUBLIC_PROFILE`]; the other deployment as trusted home and, when
    /// `trust_third_parties`, Prosody and ejabberd too; dialback in both
    /// roles, inbound only for the harness peer and the third parties'
    /// dialback hosts; every peer reached through its relay.
    pub fn standard(home: Home, ports: Ports, trust_third_parties: bool) -> Self {
        let mut allow = peers(home);
        allow.push(PUBLIC_PROFILE.to_owned());
        let private_ca = peers(home);
        let mut trusted_homes = vec![home.other().domain().to_owned()];
        if trust_third_parties {
            trusted_homes.extend(strings(&[PROSODY, EJABBERD]));
        }
        let me = Server::Waddle(home);
        let mut endpoints = Vec::new();
        let relay = |to: Server| topology::relay_for(me, to).map(|link: Link| ports.relay(link));
        if let Some(port) = relay(Server::Waddle(home.other())) {
            for domain in topology::waddle_domains(home.other().domain()) {
                endpoints.push((domain, port));
            }
        }
        if let Some(port) = relay(Server::Prosody) {
            for domain in [PROSODY, PROSODY_INSECURE, PROSODY_MUC, PROSODY_PUBSUB] {
                endpoints.push((domain.to_owned(), port));
            }
        }
        for node in [1, 2] {
            if let Some(port) = relay(Server::Ejabberd(node)) {
                for domain in [
                    EJABBERD,
                    topology::EJABBERD_INSECURE,
                    EJABBERD_MUC,
                    EJABBERD_PUBSUB,
                ] {
                    endpoints.push((domain.to_owned(), port));
                }
            }
        }
        endpoints.push((RAW_PEER.to_owned(), ports.raw_receiver()));
        endpoints.push((REDIRECT.to_owned(), ports.redirect_receiver()));
        Self {
            home,
            mode: Mode::Allowlist,
            allow,
            deny: Vec::new(),
            dialback: strings(&[RAW_PEER, PROSODY_INSECURE, topology::EJABBERD_INSECURE]),
            trusted_homes,
            require_private_ca: Vec::new(),
            private_ca,
            endpoints,
            dialback_inbound: true,
            dialback_outbound: true,
            limits: loopback_limits(),
        }
    }

    /// Remove `domain` from every admitting list.
    pub fn without(mut self, domain: &str) -> Self {
        for list in [
            &mut self.allow,
            &mut self.dialback,
            &mut self.trusted_homes,
            &mut self.require_private_ca,
        ] {
            list.retain(|entry| entry != domain);
        }
        self
    }

    /// Denylist `domain`, after removing it from the admitting lists.
    pub fn denying(self, domain: &str) -> Self {
        let mut profile = self.without(domain);
        profile.deny.push(domain.to_owned());
        profile
    }

    pub fn with_mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_limit(mut self, name: &str, value: u64) -> Self {
        self.limits.insert(name.to_owned(), value);
        self
    }

    /// Put extra domains on the `.test` CA's private-CA profile without
    /// listing them anywhere, so only `open` mode admits them and they are
    /// outside the reserve that allowlisted and trusted peers keep.
    pub fn on_test_ca(mut self, domains: &[&str]) -> Self {
        for domain in domains {
            if !self.private_ca.iter().any(|entry| entry == domain) {
                self.private_ca.push((*domain).to_owned());
            }
        }
        self
    }

    /// The default denylist entry of the admission cases.
    pub fn with_default_deny(mut self) -> Self {
        if !self.deny.iter().any(|entry| entry == DENIED) {
            self.deny.push(DENIED.to_owned());
        }
        self
    }

    /// Write the file into `directory`.
    pub fn write(&self, directory: &Path, ports: Ports, material: &Material) -> Result<PathBuf> {
        let local_domains = topology::waddle_domains(self.home.domain()).to_vec();
        let credentials = local_domains
            .iter()
            .map(|domain| {
                let (certificate_chain, private_key) = material
                    .credentials
                    .get(domain)
                    .cloned()
                    .with_context(|| format!("no credential for {domain}"))?;
                Ok(Credential {
                    domain: domain.clone(),
                    certificate_chain,
                    private_key,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let document = Document {
            generation: uuid::Uuid::new_v4().to_string(),
            listen: ["127.0.0.1:", &ports.waddle_s2s(self.home).to_string()].concat(),
            local_domains,
            peer_scope: "test",
            credentials,
            private_ca: vec![PrivateCa {
                domains: self.private_ca.clone(),
                anchors: vec![material.ca.clone()],
                crls: vec![material.crl.clone()],
            }],
            endpoints: self
                .endpoints
                .iter()
                .map(|(domain, port)| Endpoint {
                    domain: domain.clone(),
                    address: ["127.0.0.1:", &port.to_string()].concat(),
                })
                .collect(),
            admission: Admission {
                mode: self.mode,
                allow: self.allow.clone(),
                deny: self.deny.clone(),
                dialback: self.dialback.clone(),
                trusted_homes: self.trusted_homes.clone(),
                require_private_ca: self.require_private_ca.clone(),
            },
            limits: self.limits.clone(),
            dialback: Dialback {
                outbound: self.dialback_outbound,
                inbound: self.dialback_inbound,
                secret_file: self
                    .dialback_outbound
                    .then(|| material.dialback_secret.clone()),
            },
        };
        let text = toml::to_string(&document).context("serialize the S2S configuration")?;
        let path = directory.join(
            [
                self.home.label(),
                "-s2s-",
                &uuid::Uuid::new_v4().simple().to_string(),
                ".toml",
            ]
            .concat(),
        );
        std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
        Ok(path)
    }
}

/// The files a configuration refers to.
#[derive(Debug, Clone)]
pub struct Material {
    /// Chain and key per local domain.
    pub credentials: BTreeMap<String, (PathBuf, PathBuf)>,
    pub ca: PathBuf,
    pub crl: PathBuf,
    pub dialback_secret: PathBuf,
}

/// Every peer of the harness reaches a deployment from 127.0.0.1 through
/// its relay, so the limits Waddle keeps per source address would act as
/// one cap shared by every peer at once, and a clustered ejabberd opens up
/// to four streams for one pair. The unauthenticated sockets per address
/// and the negotiating streams per peer are therefore at their ceilings
/// (4 each); the per-address accept rate is already at its ceiling, and
/// the harness's own raw connections keep to it.
fn loopback_limits() -> BTreeMap<String, u64> {
    BTreeMap::from([
        ("max_unauthenticated_per_address".to_owned(), 4),
        ("max_negotiating_per_peer".to_owned(), 4),
    ])
}
