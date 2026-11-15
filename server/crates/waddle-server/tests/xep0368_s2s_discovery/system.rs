//! `SystemDnsLookup` against a name server in this process on loopback,
//! pinning how Hickory DNS 0.26.3 behaves under the configuration
//! `SystemDnsLookup::from_system` applies: only the exact absolute name is
//! queried, whatever default domain and search list the system
//! configuration names; the hosts file is not consulted; SERVFAIL and
//! REFUSED are failures; NXDOMAIN and an empty NOERROR answer are negative;
//! and SRV records stay published even when no target is usable.
//!
//! The name server answers from a fixed table and records every question.
//! No query leaves the loopback interface.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use hickory_resolver::config::{ConnectionConfig, NameServerConfig, ResolverConfig, ResolverOpts};
use hickory_resolver::proto::op::{Message, Query, ResponseCode};
use hickory_resolver::proto::rr::rdata::{A, SRV};
use hickory_resolver::proto::rr::{Name, RData, Record, RecordType};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;
use waddle_server::s2s::discovery::{DnsAnswer, DnsFailure, DnsLookup, DnsName, SystemDnsLookup};

/// The search domain the test configuration names, which must never be
/// appended to a query.
const SEARCH: &str = "search.example.";

/// A question the name server received: the name, lowercase and without
/// the final dot, and the type.
type Question = (String, RecordType);

/// A UDP name server on loopback answering from [`answer`].
struct NameServer {
    address: SocketAddr,
    questions: Arc<Mutex<Vec<Question>>>,
    task: JoinHandle<()>,
}

impl NameServer {
    async fn start() -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
        let address = socket.local_addr().expect("an address");
        let questions = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&questions);
        let task = tokio::spawn(async move {
            let mut buffer = vec![0u8; 4096];
            loop {
                let Ok((read, peer)) = socket.recv_from(&mut buffer).await else {
                    return;
                };
                let Ok(request) = Message::from_vec(&buffer[..read]) else {
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
                let (code, answers) = answer(&name, &query);
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
        Self {
            address,
            questions,
            task,
        }
    }

    fn questions(&self) -> Vec<Question> {
        match self.questions.lock() {
            Ok(questions) => questions.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// A lookup through this server, built by the same path as the
    /// system's, from a configuration that also names a default domain and
    /// a search list.
    fn lookup(&self) -> SystemDnsLookup {
        let mut connection = ConnectionConfig::udp();
        connection.port = self.address.port();
        let server = NameServerConfig::new(self.address.ip(), true, vec![connection]);
        let search = Name::from_ascii(SEARCH).expect("a search domain");
        let config = ResolverConfig::from_parts(Some(search.clone()), vec![search], vec![server]);
        SystemDnsLookup::from_config(config, ResolverOpts::default()).expect("a resolver")
    }
}

impl Drop for NameServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The name server's table, by the lowercase name without its final dot.
fn answer(name: &str, query: &Query) -> (ResponseCode, Vec<Record>) {
    let owner = query.name().clone();
    match (name, query.query_type()) {
        ("servfail.example", _) => (ResponseCode::ServFail, Vec::new()),
        ("refused.example", _) => (ResponseCode::Refused, Vec::new()),
        ("empty.example", _) => (ResponseCode::NoError, Vec::new()),
        ("records.example", RecordType::A) => (
            ResponseCode::NoError,
            vec![Record::from_rdata(
                owner,
                60,
                RData::A(A(Ipv4Addr::new(8, 8, 8, 8))),
            )],
        ),
        ("_xmpp-server._tcp.records.example", RecordType::SRV) => (
            ResponseCode::NoError,
            vec![Record::from_rdata(
                owner,
                60,
                RData::SRV(SRV::new(
                    10,
                    0,
                    5269,
                    Name::from_ascii("xmpp.records.example.").expect("a target"),
                )),
            )],
        ),
        ("_xmpp-server._tcp.dot.example", RecordType::SRV) => (
            ResponseCode::NoError,
            vec![Record::from_rdata(
                owner,
                60,
                RData::SRV(SRV::new(0, 0, 0, Name::root())),
            )],
        ),
        // Everything else, including any name with the search domain
        // appended, does not exist.
        _ => (ResponseCode::NXDomain, Vec::new()),
    }
}

fn name(text: &str) -> DnsName {
    DnsName::parse(text).expect("an absolute name")
}

#[tokio::test]
async fn failures_and_negative_answers_are_told_apart() {
    let server = NameServer::start().await;
    let lookup = server.lookup();

    // SERVFAIL and REFUSED say nothing about whether records exist.
    assert_eq!(
        lookup.ipv4(&name("servfail.example.")).await,
        Err(DnsFailure)
    );
    assert_eq!(
        lookup.ipv4(&name("refused.example.")).await,
        Err(DnsFailure)
    );
    assert_eq!(
        lookup.srv(&name("servfail.example.")).await,
        Err(DnsFailure)
    );

    // NXDOMAIN and a successful answer without records are negative.
    assert_eq!(
        lookup.ipv4(&name("nxdomain.example.")).await,
        Ok(DnsAnswer::Negative)
    );
    assert_eq!(
        lookup.ipv4(&name("empty.example.")).await,
        Ok(DnsAnswer::Negative)
    );
    assert_eq!(
        lookup.srv(&name("_xmpp-server._tcp.empty.example.")).await,
        Ok(DnsAnswer::Negative)
    );

    // Records are records.
    assert_eq!(
        lookup.ipv4(&name("records.example.")).await,
        Ok(DnsAnswer::Records(vec![Ipv4Addr::new(8, 8, 8, 8)]))
    );
    match lookup
        .srv(&name("_xmpp-server._tcp.records.example."))
        .await
    {
        Ok(DnsAnswer::Records(records)) => {
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].port, 5269);
            assert_eq!(records[0].target.as_str(), "xmpp.records.example.");
        }
        other => panic!("expected SRV records, got {other:?}"),
    }
    // A "." target is a record, not absence.
    match lookup.srv(&name("_xmpp-server._tcp.dot.example.")).await {
        Ok(DnsAnswer::Records(records)) => {
            assert_eq!(records.len(), 1);
            assert!(records[0].target.is_root());
        }
        other => panic!("expected a \".\" SRV record, got {other:?}"),
    }
}

#[tokio::test]
async fn only_the_exact_absolute_name_is_ever_queried() {
    let server = NameServer::start().await;
    let lookup = server.lookup();
    let asked = [
        "nxdomain.example.",
        "empty.example.",
        "records.example.",
        "single.",
    ];
    for text in asked {
        let _ = lookup.ipv4(&name(text)).await;
        let _ = lookup.ipv6(&name(text)).await;
    }
    let _ = lookup
        .srv(&name("_xmpps-server._tcp.nxdomain.example."))
        .await;

    let expected: HashSet<String> = asked
        .iter()
        .map(|text| text.trim_end_matches('.').to_owned())
        .chain(["_xmpps-server._tcp.nxdomain.example".to_owned()])
        .collect();
    let questions = server.questions();
    assert!(!questions.is_empty());
    for (queried, _) in &questions {
        assert!(
            expected.contains(queried),
            "{queried} was queried, which no lookup asked for"
        );
        assert!(
            !queried.ends_with(SEARCH.trim_end_matches('.')),
            "the search domain was appended to {queried}"
        );
    }
    // Every name asked was put to the name server as it was given.
    let queried: HashSet<String> = questions.into_iter().map(|(name, _)| name).collect();
    assert_eq!(queried, expected);
}

#[tokio::test]
async fn the_hosts_file_is_never_consulted() {
    // A name the system's hosts file maps to an address would be answered
    // from the file without a query if the file were consulted. Hickory
    // reads the system file itself, so the test uses whatever entry it
    // carries beyond the `localhost` names, which Hickory answers itself
    // under RFC 6761 whatever the configuration.
    let Some((host, _)) = hosts_entry() else {
        return;
    };
    let server = NameServer::start().await;
    let lookup = server.lookup();
    let text = format!("{host}.");
    let Some(absolute) = DnsName::parse(&text) else {
        return;
    };
    // The name server does not know the name: negative, and asked.
    assert_eq!(lookup.ipv4(&absolute).await, Ok(DnsAnswer::Negative));
    assert!(server
        .questions()
        .iter()
        .any(|(queried, kind)| queried == &host && *kind == RecordType::A));
}

/// A name with at least one dot that the system's hosts file maps to an
/// IPv4 address, other than a name Hickory answers itself under RFC 6761:
/// `localhost` and its subdomains, `.invalid` and `.onion`.
fn hosts_entry() -> Option<(String, IpAddr)> {
    let text = std::fs::read_to_string("/etc/hosts").ok()?;
    text.lines()
        .map(|line| line.split('#').next().unwrap_or_default())
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let address: IpAddr = fields.next()?.parse().ok()?;
            address.is_ipv4().then_some((address, fields))
        })
        .find_map(|(address, names)| {
            names
                .map(str::to_ascii_lowercase)
                .find(|name| {
                    name.contains('.')
                        && name.is_ascii()
                        && name != "localhost"
                        && !name.ends_with(".localhost")
                        && !name.ends_with(".invalid")
                        && !name.ends_with(".onion")
                })
                .map(|name| (name, address))
        })
}
