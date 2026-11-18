//! A stub DNS server on loopback through which Prosody (libunbound,
//! `unbound.forward`) and ejabberd (`ERL_INETRC` `nameserver`) find the
//! `.test` domains.
//!
//! Each third-party server gets its own view: `_xmpp-server._tcp.<domain>`
//! answers one SRV record whose port is that server's relay towards the
//! domain, and whose target is the domain itself, whose A record is
//! `127.0.0.1`, or `localhost` in a view whose server resolves SRV targets
//! with the operating system's resolver instead of this stub (ejabberd's
//! `xmpp_stream_out` looks SRV records up with `inet_res` but their targets
//! with `getaddrinfo`, which cannot see the `.test` names). `_xmpps-server` answers no record, so no peer attempts
//! direct TLS, which Waddle does not accept inbound. Every other name does
//! not exist. Waddle itself never uses this server: it reaches its peers
//! through configured endpoints, and its DNS discovery is covered by the
//! stub-resolver suite instead (`xep0368_s2s_discovery`).

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use hickory_proto::op::{Message, Query, ResponseCode};
use hickory_proto::rr::rdata::{A, SRV};
use hickory_proto::rr::{Name, RData, Record, RecordType};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

/// The names one view answers: domain to SRV port.
#[derive(Debug, Clone, Default)]
pub struct View {
    pub services: BTreeMap<String, u16>,
    /// The SRV target for every service, or the domain itself when `None`.
    pub srv_target: Option<String>,
}

/// A running stub server and the questions it was asked.
pub struct StubDns {
    questions: Arc<Mutex<Vec<(String, RecordType)>>>,
    task: JoinHandle<()>,
}

impl StubDns {
    pub async fn start(port: u16, view: View) -> Result<Self> {
        let socket = UdpSocket::bind(("127.0.0.1", port))
            .await
            .with_context(|| format!("bind stub DNS on {port}"))?;
        let questions = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&questions);
        let task = tokio::spawn(async move {
            let mut buffer = vec![0u8; 4096];
            loop {
                let Ok((read, peer)) = socket.recv_from(&mut buffer).await else {
                    return;
                };
                let Some(bytes) = buffer.get(..read) else {
                    continue;
                };
                let Ok(request) = Message::from_vec(bytes) else {
                    continue;
                };
                let Some(query) = request.queries.first().cloned() else {
                    continue;
                };
                let name = query
                    .name()
                    .to_ascii()
                    .trim_end_matches('.')
                    .to_ascii_lowercase();
                match log.lock() {
                    Ok(mut log) => log.push((name.clone(), query.query_type())),
                    Err(poisoned) => poisoned
                        .into_inner()
                        .push((name.clone(), query.query_type())),
                }
                let mut response = Message::response(request.metadata.id, request.metadata.op_code);
                response.metadata.recursion_desired = request.metadata.recursion_desired;
                response.metadata.recursion_available = true;
                response.metadata.authoritative = true;
                let (code, answers) = answer(&view, &name, &query);
                response.metadata.response_code = code;
                response.add_query(query);
                for record in answers {
                    response.add_answer(record);
                }
                if let Ok(bytes) = response.to_vec() {
                    let _ = socket.send_to(&bytes, peer).await;
                }
            }
        });
        Ok(Self { questions, task })
    }

    /// The names asked so far, lowercase without the final dot.
    pub fn questions(&self) -> Vec<(String, RecordType)> {
        match self.questions.lock() {
            Ok(questions) => questions.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl Drop for StubDns {
    fn drop(&mut self) {
        self.task.abort();
    }
}

const TTL: u32 = 1;

fn answer(view: &View, name: &str, query: &Query) -> (ResponseCode, Vec<Record>) {
    let owner = query.name().clone();
    if name == "localhost" {
        return match query.query_type() {
            RecordType::A => (
                ResponseCode::NoError,
                vec![Record::from_rdata(
                    owner,
                    TTL,
                    RData::A(A(Ipv4Addr::LOCALHOST)),
                )],
            ),
            _ => (ResponseCode::NoError, Vec::new()),
        };
    }
    if let Some(domain) = name.strip_prefix("_xmpp-server._tcp.") {
        return match (view.services.get(domain), query.query_type()) {
            (Some(port), RecordType::SRV) => {
                match Name::from_ascii([view.srv_target.as_deref().unwrap_or(domain), "."].concat())
                {
                    Ok(target) => (
                        ResponseCode::NoError,
                        vec![Record::from_rdata(
                            owner,
                            TTL,
                            RData::SRV(SRV::new(0, 0, *port, target)),
                        )],
                    ),
                    Err(_) => (ResponseCode::ServFail, Vec::new()),
                }
            }
            (Some(_), _) => (ResponseCode::NoError, Vec::new()),
            (None, _) => (ResponseCode::NXDomain, Vec::new()),
        };
    }
    if let Some(domain) = name.strip_prefix("_xmpps-server._tcp.") {
        return if view.services.contains_key(domain) {
            (ResponseCode::NoError, Vec::new())
        } else {
            (ResponseCode::NXDomain, Vec::new())
        };
    }
    if view.services.contains_key(name) {
        return match query.query_type() {
            RecordType::A => (
                ResponseCode::NoError,
                vec![Record::from_rdata(
                    owner,
                    TTL,
                    RData::A(A(Ipv4Addr::LOCALHOST)),
                )],
            ),
            _ => (ResponseCode::NoError, Vec::new()),
        };
    }
    (ResponseCode::NXDomain, Vec::new())
}
