//! Pure-Rust SSRF IPv4/IPv6/IPv4-mapped blocklist classifier.
//!
//! Formally verified for completeness over loopback, RFC1918, CGNAT,
//! link-local/cloud metadata, ULA, unspecified, broadcast, multicast,
//! reserved ranges, and `::ffff:0:0/96` IPv4-mapped addresses in
//! `server/specs/verus/ssrf_ip_filter.rs`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Returns `true` if the IPv4 address falls in any blocked non-public or
/// SSRF-sensitive range.
pub const fn is_blocked_ipv4(octets: [u8; 4]) -> bool {
    let a = octets[0];
    let b = octets[1];

    // 0.0.0.0/8 (unspecified / "this network")
    let unspecified = a == 0;
    // 10.0.0.0/8 (RFC1918)
    let rfc1918_10 = a == 10;
    // 100.64.0.0/10 (RFC6598 CGNAT)
    let cgnat = a == 100 && (b & 0b1100_0000) == 64;
    // 127.0.0.0/8 (loopback)
    let loopback = a == 127;
    // 169.254.0.0/16 (link-local & cloud IMDS metadata 169.254.169.254)
    let link_local = a == 169 && b == 254;
    // 172.16.0.0/12 (RFC1918)
    let rfc1918_172 = a == 172 && (b & 0b1111_0000) == 16;
    // 192.168.0.0/16 (RFC1918)
    let rfc1918_192 = a == 192 && b == 168;
    // 224.0.0.0/4 (multicast: 224..=239)
    let multicast = (a & 0b1111_0000) == 224;
    // 240.0.0.0/4 (reserved & limited broadcast 255.255.255.255)
    let reserved_or_broadcast = (a & 0b1111_0000) == 240;

    unspecified
        || rfc1918_10
        || cgnat
        || loopback
        || link_local
        || rfc1918_172
        || rfc1918_192
        || multicast
        || reserved_or_broadcast
}

/// Returns `true` if the IPv6 address falls in any blocked non-public or
/// SSRF-sensitive range, including IPv4-mapped `::ffff:0:0/96` addresses
/// whose embedded IPv4 address is blocked.
pub const fn is_blocked_ipv6(segs: [u16; 8]) -> bool {
    let all_zero_prefix =
        segs[0] == 0 && segs[1] == 0 && segs[2] == 0 && segs[3] == 0 && segs[4] == 0;

    // :: (unspecified)
    let unspecified = all_zero_prefix && segs[5] == 0 && segs[6] == 0 && segs[7] == 0;
    // ::1 (loopback)
    let loopback = all_zero_prefix && segs[5] == 0 && segs[6] == 0 && segs[7] == 1;
    // fc00::/7 (ULA)
    let ula = (segs[0] & 0xfe00) == 0xfc00;
    // fe80::/10 (link-local)
    let link_local = (segs[0] & 0xffc0) == 0xfe80;
    // ff00::/8 (multicast)
    let multicast = (segs[0] & 0xff00) == 0xff00;
    // ::ffff:0:0/96 (IPv4-mapped IPv6)
    let ipv4_mapped_blocked = if all_zero_prefix && segs[5] == 0xffff {
        let v4 = [
            (segs[6] >> 8) as u8,
            (segs[6] & 0xff) as u8,
            (segs[7] >> 8) as u8,
            (segs[7] & 0xff) as u8,
        ];
        is_blocked_ipv4(v4)
    } else {
        false
    };

    unspecified || loopback || ula || link_local || multicast || ipv4_mapped_blocked
}

pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4_addr(v4),
        IpAddr::V6(v6) => is_blocked_ipv6_addr(v6),
    }
}

pub const fn is_blocked_ipv4_addr(v4: Ipv4Addr) -> bool {
    is_blocked_ipv4(v4.octets())
}

pub const fn is_blocked_ipv6_addr(v6: Ipv6Addr) -> bool {
    is_blocked_ipv6(v6.segments())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::*;

    #[test]
    fn blocks_all_required_ipv4_and_ipv6_ranges() {
        let blocked_v4 = [
            Ipv4Addr::new(0, 0, 0, 0),
            Ipv4Addr::new(10, 0, 0, 1),
            Ipv4Addr::new(100, 64, 0, 1),
            Ipv4Addr::new(127, 0, 0, 1),
            Ipv4Addr::new(169, 254, 169, 254),
            Ipv4Addr::new(172, 16, 0, 1),
            Ipv4Addr::new(172, 31, 255, 254),
            Ipv4Addr::new(192, 168, 1, 1),
            Ipv4Addr::new(224, 0, 0, 1),
            Ipv4Addr::new(240, 0, 0, 1),
            Ipv4Addr::new(255, 255, 255, 255),
        ];
        for ip in blocked_v4 {
            assert!(is_blocked_ip(IpAddr::V4(ip)), "expected {ip} to be blocked");
            assert!(
                is_blocked_ip(IpAddr::V6(ip.to_ipv6_mapped())),
                "expected ::ffff:{ip} to be blocked"
            );
        }

        let blocked_v6 = [
            Ipv6Addr::UNSPECIFIED,
            Ipv6Addr::LOCALHOST,
            "fc00::1".parse::<Ipv6Addr>().expect("valid"),
            "fd12:3456::1".parse::<Ipv6Addr>().expect("valid"),
            "fe80::1".parse::<Ipv6Addr>().expect("valid"),
            "ff02::1".parse::<Ipv6Addr>().expect("valid"),
        ];
        for ip in blocked_v6 {
            assert!(is_blocked_ip(IpAddr::V6(ip)), "expected {ip} to be blocked");
        }
    }

    #[test]
    fn allows_public_provider_ips() {
        assert!(!is_blocked_ip(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        assert!(!is_blocked_ip(IpAddr::V4(Ipv4Addr::new(104, 18, 32, 7))));
        assert!(!is_blocked_ip(IpAddr::V6(
            Ipv4Addr::new(104, 18, 32, 7).to_ipv6_mapped()
        )));
        assert!(!is_blocked_ip(IpAddr::V6(
            "2606:4700::1111".parse::<Ipv6Addr>().expect("valid")
        )));
    }
}
