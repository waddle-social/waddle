//! The pinned Prosody (`prosody.test`): one configuration file in Lua,
//! accounts registered with `prosodyctl`, DNS through the harness's stub
//! via libunbound's forwarder, and the `.test` CA as its S2S trust with
//! secure authentication required, except with home-a on
//! `insecure.prosody.test`, which trusts only an unrelated CA and so
//! negotiates dialback.
//!
//! Two Prosody details shape the configuration. libunbound refuses to
//! query a forwarder on a loopback address and answers the special-use
//! `test.` zone itself (NXDOMAIN) unless told otherwise, so the stub is
//! reached only with `do-not-query-localhost: no` and `test.` made a
//! transparent local zone. And `mod_s2s` is a global module: its
//! `s2s_secure_auth` is one setting for every host, keyed by the remote
//! domain, which a VirtualHost cannot relax. Secure authentication stays
//! on, and home-a is listed in `s2s_insecure_domains` so that
//! `insecure.prosody.test`, which cannot verify home-a's certificate,
//! may use dialback with it; `prosody.test` still offers SASL EXTERNAL
//! only for a valid chain and identity, and the flows check that home-a
//! established `pkix` streams with it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use tokio::process::Command;

use crate::pki::Leaf;
use crate::process::{self, StopSignal, Supervised};
use crate::topology::{Ports, HOME_A, PROSODY, PROSODY_INSECURE, PROSODY_MUC, PROSODY_PUBSUB};

pub const ACCOUNTS: [&str; 4] = ["admin", "alice", "bob", "caseprobe"];

/// Which TLS versions Prosody's S2S contexts allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsProfile {
    Default,
    Tls12Only,
}

/// A configuration variant.
#[derive(Clone)]
pub struct Variant {
    /// The certificate each host presents.
    pub leaves: BTreeMap<String, Leaf>,
    pub tls: TlsProfile,
}

pub struct Prosody {
    bin: PathBuf,
    root: PathBuf,
    logs: PathBuf,
    ports: Ports,
    ca: PathBuf,
    other_ca: PathBuf,
    pub passwords: BTreeMap<String, String>,
    process: Option<Supervised>,
    pub config: PathBuf,
    starts: usize,
}

/// A Lua string literal.
fn lua(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '"' | '\\' => {
                quoted.push('\\');
                quoted.push(character);
            }
            '\n' => quoted.push_str("\\n"),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

fn path(path: &Path) -> String {
    lua(&path.display().to_string())
}

impl Prosody {
    pub fn new(
        bin: &Path,
        root: &Path,
        logs: &Path,
        ports: Ports,
        ca: &Path,
        other_ca: &Path,
    ) -> Self {
        let passwords = ACCOUNTS
            .iter()
            .map(|account| {
                (
                    (*account).to_owned(),
                    uuid::Uuid::new_v4().simple().to_string(),
                )
            })
            .collect();
        Self {
            bin: bin.to_owned(),
            root: root.join("prosody"),
            logs: logs.to_owned(),
            ports,
            ca: ca.to_owned(),
            other_ca: other_ca.to_owned(),
            passwords,
            process: None,
            config: root.join("prosody").join("prosody.cfg.lua"),
            starts: 0,
        }
    }

    fn ssl(&self, leaf: Option<&Leaf>, trust: &Path, tls: TlsProfile) -> String {
        let mut fields = Vec::new();
        if let Some(leaf) = leaf {
            fields.push(["certificate = ", &path(&leaf.certificate_path)].concat());
            fields.push(["key = ", &path(&leaf.key_path)].concat());
        }
        fields.push(["cafile = ", &path(trust)].concat());
        if tls == TlsProfile::Tls12Only {
            fields.push("protocol = \"tlsv1_2\"".to_owned());
        }
        ["{ ", &fields.join("; "), " }"].concat()
    }

    fn render(&self, variant: &Variant) -> String {
        let leaf = |domain: &str| variant.leaves.get(domain);
        let data = self.root.join("data");
        let log = self.logs.join("prosody-debug.log");
        let lines = vec![
            ["pidfile = ", &path(&self.root.join("prosody.pid"))].concat(),
            ["data_path = ", &path(&data)].concat(),
            ["log = { { levels = { min = \"debug\" }, to = \"file\", filename = ", &path(&log), " }, { levels = { min = \"info\" }, to = \"console\" } }"].concat(),
            "admins = { \"admin@prosody.test\" }".to_owned(),
            "modules_enabled = { \"roster\", \"saslauth\", \"tls\", \"dialback\", \"disco\", \"carbons\", \"pep\", \"ping\", \"mam\", \"bookmarks\", \"private\", \"version\", \"uptime\", \"time\" }".to_owned(),
            "modules_disabled = { \"s2s_bidi\" }".to_owned(),
            ["c2s_ports = { ", &self.ports.prosody_c2s().to_string(), " }"].concat(),
            "c2s_interfaces = { \"127.0.0.1\" }".to_owned(),
            ["s2s_ports = { ", &self.ports.prosody_s2s().to_string(), " }"].concat(),
            "s2s_interfaces = { \"127.0.0.1\" }".to_owned(),
            "c2s_direct_tls_ports = { }".to_owned(),
            "s2s_direct_tls_ports = { }".to_owned(),
            "http_ports = { }".to_owned(),
            "https_ports = { }".to_owned(),
            "c2s_require_encryption = true".to_owned(),
            "s2s_require_encryption = true".to_owned(),
            "s2s_secure_auth = true".to_owned(),
            ["s2s_insecure_domains = { ", &lua(HOME_A), " }"].concat(),
            "use_dane = false".to_owned(),
            "authentication = \"internal_hashed\"".to_owned(),
            "storage = \"internal\"".to_owned(),
            "archive_expires_after = \"never\"".to_owned(),
            [
                "unbound = { forward = ",
                &lua(&["127.0.0.1@", &self.ports.dns_for_prosody().to_string()].concat()),
                "; resolvconf = false; hoststxt = false; options = { [\"do-not-query-localhost\"] = \"no\"; [\"local-zone\"] = \"test. transparent\" } }",
            ]
            .concat(),
            ["ssl = ", &self.ssl(leaf(PROSODY), &self.ca, variant.tls)].concat(),
            String::new(),
            ["VirtualHost ", &lua(PROSODY)].concat(),
            ["  ssl = ", &self.ssl(leaf(PROSODY), &self.ca, variant.tls)].concat(),
            String::new(),
            ["VirtualHost ", &lua(PROSODY_INSECURE)].concat(),
            ["  ssl = ", &self.ssl(leaf(PROSODY_INSECURE), &self.other_ca, variant.tls)].concat(),
            String::new(),
            ["Component ", &lua(PROSODY_MUC), " \"muc\""].concat(),
            ["  ssl = ", &self.ssl(leaf(PROSODY_MUC), &self.ca, variant.tls)].concat(),
            "  modules_enabled = { \"muc_mam\" }".to_owned(),
            "  restrict_room_creation = \"local\"".to_owned(),
            "  muc_room_default_persistent = true".to_owned(),
            String::new(),
            ["Component ", &lua(PROSODY_PUBSUB), " \"pubsub\""].concat(),
            ["  ssl = ", &self.ssl(leaf(PROSODY_PUBSUB), &self.ca, variant.tls)].concat(),
            String::new(),
        ];
        lines.join("\n")
    }

    /// Write the configuration, register the accounts and start.
    pub async fn start(&mut self, variant: &Variant) -> Result<()> {
        std::fs::create_dir_all(self.root.join("data"))?;
        std::fs::write(&self.config, self.render(variant))?;
        for host in [PROSODY, PROSODY_INSECURE] {
            for (account, password) in &self.passwords {
                let mut register = Command::new(self.bin.join("prosodyctl"));
                register
                    .arg("--config")
                    .arg(&self.config)
                    .arg("register")
                    .arg(account)
                    .arg(host)
                    .arg(password);
                process::run("prosodyctl register", register, Duration::from_secs(60)).await?;
            }
        }
        self.starts += 1;
        let mut command = Command::new(self.bin.join("prosody"));
        command.arg("--config").arg(&self.config).arg("-F");
        let label = ["prosody-", &self.starts.to_string()].concat();
        self.process = Some(Supervised::spawn(
            &label,
            command,
            &self.logs,
            StopSignal::TermGroup,
        )?);
        process::wait_tcp("prosody", self.ports.prosody_c2s(), Duration::from_secs(60)).await?;
        process::wait_tcp("prosody", self.ports.prosody_s2s(), Duration::from_secs(30)).await
    }

    pub async fn stop(&mut self) -> Result<()> {
        if let Some(mut process) = self.process.take() {
            process.stop(Duration::from_secs(30)).await?;
        }
        Ok(())
    }

    pub async fn restart(&mut self, variant: &Variant) -> Result<()> {
        self.stop().await?;
        self.start(variant).await
    }

    pub fn log_tail(&self) -> String {
        self.process
            .as_ref()
            .map(|process| process.log_tail(40))
            .unwrap_or_default()
    }
}
