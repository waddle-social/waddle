//! The pinned ejabberd (`ejabberd.test`) as a two-node cluster.
//!
//! Each node has its own configuration directory (ejabberd.yml,
//! ejabberdctl.cfg and the `inetrc` that points Erlang's resolver at the
//! harness's stub DNS), spool and logs; both share a cookie and a private
//! epmd on a harness port, which the harness supervises itself because an
//! epmd started by `erl` would leave the process group. The configuration
//! is written as JSON, which ejabberd's YAML reader accepts, so every
//! value is quoted by the serializer. `s2s_use_starttls` is `required` and
//! `max_s2s_connections` is above one (a shaper rule in ejabberd, read by
//! `ejabberd_s2s`), so the cluster opens several streams for one pair.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::process::Command;

use crate::pki::Leaf;
use crate::process::{self, StopSignal, Supervised};
use crate::topology::{Ports, EJABBERD, EJABBERD_INSECURE};

pub const ACCOUNTS: [&str; 4] = ["admin", "alice", "bob", "caseprobe"];
pub const NODES: [u8; 2] = [1, 2];

/// The `max_s2s_connections` and `max_s2s_connections_per_node` rules.
pub const MAX_S2S_CONNECTIONS: u64 = 4;
pub const MAX_S2S_CONNECTIONS_PER_NODE: u64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsProfile {
    Default,
    Tls12Only,
}

#[derive(Clone)]
pub struct Variant {
    /// The certificate files ejabberd loads (`certfiles`).
    pub leaves: BTreeMap<String, Leaf>,
    pub tls: TlsProfile,
}

pub struct Ejabberd {
    bin: PathBuf,
    erlang: PathBuf,
    root: PathBuf,
    logs: PathBuf,
    ports: Ports,
    ca: PathBuf,
    other_ca: PathBuf,
    cookie: String,
    pub passwords: BTreeMap<String, String>,
    epmd: Option<Supervised>,
    nodes: BTreeMap<u8, Supervised>,
    starts: usize,
}

pub fn node_name(node: u8) -> String {
    ["ejabberd", &node.to_string(), "@localhost"].concat()
}

impl Ejabberd {
    pub fn new(
        bin: &Path,
        erlang: &Path,
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
            erlang: erlang.to_owned(),
            root: root.join("ejabberd"),
            logs: logs.to_owned(),
            ports,
            ca: ca.to_owned(),
            other_ca: other_ca.to_owned(),
            cookie: uuid::Uuid::new_v4().simple().to_string(),
            passwords,
            epmd: None,
            nodes: BTreeMap::new(),
            starts: 0,
        }
    }

    fn config_dir(&self, node: u8) -> PathBuf {
        self.root.join(["node", &node.to_string()].concat())
    }

    fn configuration(&self, node: u8, variant: &Variant) -> Value {
        let certfiles: Vec<String> = variant
            .leaves
            .values()
            .map(|leaf| leaf.combined_path.display().to_string())
            .collect();
        let admin = ["admin@", EJABBERD].concat();
        let mut config = json!({
            "hosts": [EJABBERD, EJABBERD_INSECURE],
            "loglevel": "info",
            "certfiles": certfiles,
            "ca_file": self.ca.display().to_string(),
            "s2s_use_starttls": "required",
            "s2s_dns_timeout": 2,
            "s2s_dns_retries": 0,
            "outgoing_s2s_families": ["ipv4"],
            "listen": [
                {
                    "port": self.ports.ejabberd_c2s(node),
                    "ip": "127.0.0.1",
                    "module": "ejabberd_c2s",
                    "starttls_required": true,
                    "max_stanza_size": 262144
                },
                {
                    "port": self.ports.ejabberd_s2s(node),
                    "ip": "127.0.0.1",
                    "module": "ejabberd_s2s_in",
                    "max_stanza_size": 524288
                }
            ],
            "acl": {
                "local": { "user_regexp": "" },
                "admin": { "user": [admin] }
            },
            "access_rules": {
                "local": { "allow": "local" },
                "c2s": { "allow": "all" },
                "configure": { "allow": "admin" },
                "muc_create": { "allow": "local" },
                "pubsub_createnode": { "allow": "local" }
            },
            "shaper_rules": {
                "max_user_sessions": 10,
                "max_s2s_connections": MAX_S2S_CONNECTIONS,
                "max_s2s_connections_per_node": MAX_S2S_CONNECTIONS_PER_NODE
            },
            "host_config": {
                EJABBERD_INSECURE: { "s2s_cafile": self.other_ca.display().to_string() }
            },
            "modules": {
                "mod_caps": {},
                "mod_carboncopy": {},
                "mod_disco": {},
                "mod_mam": { "default": "always" },
                "mod_muc": {
                    "access_create": "muc_create",
                    "default_room_options": { "mam": true, "persistent": true }
                },
                "mod_ping": {},
                "mod_private": {},
                "mod_pubsub": {
                    "access_createnode": "pubsub_createnode",
                    "plugins": ["flat", "pep"]
                },
                "mod_roster": {},
                "mod_s2s_dialback": {},
                "mod_stream_mgmt": {}
            }
        });
        if variant.tls == TlsProfile::Tls12Only {
            if let Some(object) = config.as_object_mut() {
                object.insert(
                    "s2s_protocol_options".to_owned(),
                    json!(["no_sslv3", "no_tlsv1", "no_tlsv1_1", "no_tlsv1_3"]),
                );
            }
        }
        config
    }

    fn write_node(&self, node: u8, variant: &Variant) -> Result<()> {
        let dir = self.config_dir(node);
        std::fs::create_dir_all(dir.join("spool"))?;
        std::fs::write(
            dir.join("ejabberd.yml"),
            serde_json::to_vec_pretty(&self.configuration(node, variant))?,
        )?;
        let dist = self.ports.ejabberd_dist(node);
        let ctl = [
            "ERLANG_NODE=",
            &node_name(node),
            "\nFIREWALL_WINDOW=",
            &dist.to_string(),
            "-",
            &dist.to_string(),
            "\nINET_DIST_INTERFACE=127.0.0.1\nERL_OPTIONS=\"-setcookie ",
            &self.cookie,
            "\"\n",
        ]
        .concat();
        std::fs::write(dir.join("ejabberdctl.cfg"), ctl)?;
        let inetrc = [
            "{host, {127,0,0,1}, [\"localhost\"]}.\n{nameserver, {127,0,0,1}, ",
            &self.ports.dns_for_ejabberd().to_string(),
            "}.\n{resolv_conf, \"\"}.\n{hosts_file, \"\"}.\n{lookup, [file, dns]}.\n",
        ]
        .concat();
        std::fs::write(dir.join("inetrc"), inetrc)?;
        Ok(())
    }

    fn ctl(&self, node: u8) -> Command {
        let dir = self.config_dir(node);
        let mut command = Command::new(self.bin.join("ejabberdctl"));
        command
            .env("ERL_EPMD_PORT", self.ports.epmd().to_string())
            .env("HOME", &dir)
            .arg("--config-dir")
            .arg(&dir)
            .arg("--spool")
            .arg(dir.join("spool"))
            .arg("--logs")
            .arg(self.logs.join(["ejabberd", &node.to_string()].concat()))
            .arg("--node")
            .arg(node_name(node));
        command
    }

    async fn start_epmd(&mut self) -> Result<()> {
        if self.epmd.is_some() {
            return Ok(());
        }
        let mut command = Command::new(self.erlang.join("epmd"));
        command
            .arg("-port")
            .arg(self.ports.epmd().to_string())
            .arg("-address")
            .arg("127.0.0.1");
        self.epmd = Some(Supervised::spawn(
            "epmd",
            command,
            &self.logs,
            StopSignal::TermGroup,
        )?);
        process::wait_tcp("epmd", self.ports.epmd(), Duration::from_secs(20)).await
    }

    async fn start_node(&mut self, node: u8) -> Result<()> {
        std::fs::create_dir_all(self.logs.join(["ejabberd", &node.to_string()].concat()))?;
        let mut command = self.ctl(node);
        command.arg("foreground");
        self.starts += 1;
        let label = ["ejabberd", &node.to_string(), "-", &self.starts.to_string()].concat();
        let process = Supervised::spawn(&label, command, &self.logs, StopSignal::TermGroup)?;
        self.nodes.insert(node, process);
        let mut started = self.ctl(node);
        started.arg("started");
        process::run("ejabberdctl started", started, Duration::from_secs(90)).await?;
        process::wait_tcp(
            "ejabberd",
            self.ports.ejabberd_c2s(node),
            Duration::from_secs(30),
        )
        .await
    }

    /// Write both nodes' configuration, start them, join node 2 to node 1
    /// and register the accounts.
    pub async fn start(&mut self, variant: &Variant) -> Result<()> {
        for node in NODES {
            self.write_node(node, variant)?;
        }
        self.start_epmd().await?;
        self.start_node(1).await?;
        self.start_node(2).await?;
        let mut join = self.ctl(2);
        join.arg("join_cluster").arg(node_name(1));
        process::run("ejabberdctl join_cluster", join, Duration::from_secs(90))
            .await
            .context("join node 2 to node 1")?;
        let mut started = self.ctl(2);
        started.arg("started");
        process::run("ejabberdctl started", started, Duration::from_secs(90)).await?;
        for host in [EJABBERD, EJABBERD_INSECURE] {
            for (account, password) in self.passwords.clone() {
                let mut register = self.ctl(1);
                register
                    .arg("register")
                    .arg(&account)
                    .arg(host)
                    .arg(&password);
                // An account that survived a restart is already registered.
                let _ =
                    process::run("ejabberdctl register", register, Duration::from_secs(60)).await;
            }
        }
        Ok(())
    }

    /// The cluster's members as node 1 reports them.
    pub async fn cluster_nodes(&self) -> Result<String> {
        let mut list = self.ctl(1);
        list.arg("list_cluster");
        process::run("ejabberdctl list_cluster", list, Duration::from_secs(60)).await
    }

    pub async fn stop_node(&mut self, node: u8) -> Result<()> {
        if let Some(mut process) = self.nodes.remove(&node) {
            let mut stop = self.ctl(node);
            stop.arg("stop");
            let _ = process::run("ejabberdctl stop", stop, Duration::from_secs(60)).await;
            process.stop(Duration::from_secs(30)).await?;
        }
        Ok(())
    }

    pub async fn restart_node(&mut self, node: u8, variant: &Variant) -> Result<()> {
        self.stop_node(node).await?;
        self.write_node(node, variant)?;
        self.start_node(node).await
    }

    pub async fn stop(&mut self) -> Result<()> {
        for node in [2, 1] {
            self.stop_node(node).await?;
        }
        if let Some(mut epmd) = self.epmd.take() {
            epmd.stop(Duration::from_secs(10)).await?;
        }
        Ok(())
    }

    /// Restart the whole cluster with a different variant.
    pub async fn restart(&mut self, variant: &Variant) -> Result<()> {
        for node in [2, 1] {
            self.stop_node(node).await?;
        }
        for node in NODES {
            self.write_node(node, variant)?;
        }
        self.start_node(1).await?;
        self.start_node(2).await
    }

    pub fn log_tail(&self, node: u8) -> String {
        self.nodes
            .get(&node)
            .map(|process| process.log_tail(40))
            .unwrap_or_default()
    }
}
