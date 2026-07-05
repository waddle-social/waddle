//! Headless-Service peer discovery for the clustering swarm.
//!
//! A Kubernetes headless Service resolves to the A/AAAA records of every ready
//! pod. We resolve the configured DNS name to socket addresses and turn each
//! into a dialable TCP multiaddr; libp2p learns each peer's `PeerId` after the
//! Noise handshake, so we never need to know peer IDs ahead of time (kademlia
//! carries node discovery only — ADR element 6).

use crate::config::ClusteringBootstrapConfig;
use libp2p::multiaddr::Protocol;
use libp2p::Multiaddr;
use std::net::IpAddr;

/// Resolve the headless-Service DNS name to a deduplicated set of dialable TCP
/// multiaddrs. Returns an empty vec (never an error) when resolution fails, so
/// the caller's dial loop simply retries on its next tick.
pub async fn resolve_bootstrap_peers(bootstrap: &ClusteringBootstrapConfig) -> Vec<Multiaddr> {
    let host_port = format!("{}:{}", bootstrap.dns_name, bootstrap.port);
    let resolved = match tokio::net::lookup_host(&host_port).await {
        Ok(addrs) => addrs,
        Err(error) => {
            tracing::debug!(
                dns = %bootstrap.dns_name,
                %error,
                "clustering bootstrap DNS resolution failed; will retry"
            );
            return Vec::new();
        }
    };

    let mut peers: Vec<Multiaddr> = Vec::new();
    for sockaddr in resolved {
        let multiaddr = tcp_multiaddr(sockaddr.ip(), bootstrap.port);
        if !peers.contains(&multiaddr) {
            peers.push(multiaddr);
        }
    }
    peers
}

/// Build a dialable `/ip{4,6}/…/tcp/<port>` multiaddr for a resolved peer IP.
fn tcp_multiaddr(ip: IpAddr, port: u16) -> Multiaddr {
    let ip_proto = match ip {
        IpAddr::V4(v4) => Protocol::Ip4(v4),
        IpAddr::V6(v6) => Protocol::Ip6(v6),
    };
    Multiaddr::empty().with(ip_proto).with(Protocol::Tcp(port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn builds_ipv4_tcp_multiaddr() {
        let ma = tcp_multiaddr(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), 7900);
        assert_eq!(ma.to_string(), "/ip4/10.0.0.5/tcp/7900");
    }

    #[test]
    fn builds_ipv6_tcp_multiaddr() {
        let ma = tcp_multiaddr(IpAddr::V6(Ipv6Addr::LOCALHOST), 7900);
        assert_eq!(ma.to_string(), "/ip6/::1/tcp/7900");
    }
}
