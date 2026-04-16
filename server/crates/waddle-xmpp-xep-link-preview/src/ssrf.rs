//! SSRF guard: classify IP addresses that must never be fetched from the
//! server, regardless of whether they appear in a URL literal or are
//! returned by DNS resolution of a user-supplied hostname.
//!
//! Rejects all private/loopback/link-local/reserved/documentation ranges.
//! Used both at URL-parse time (literal IPs in the host component) and
//! inside the custom DNS resolver (filtering resolved addresses).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Returns `true` if fetching from this address must be blocked.
///
/// Covered ranges:
/// - IPv4: `0.0.0.0/8`, `10.0.0.0/8`, `127.0.0.0/8`, `169.254.0.0/16`,
///   `172.16.0.0/12`, `192.168.0.0/16`, multicast (`224.0.0.0/4`),
///   broadcast, reserved (`240.0.0.0/4`), documentation ranges
///   (`192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`),
///   shared address space (`100.64.0.0/10`), benchmarking (`198.18.0.0/15`).
/// - IPv6: loopback `::1`, unspecified `::`, unique-local `fc00::/7`,
///   link-local `fe80::/10`, multicast `ff00::/8`, documentation
///   `2001:db8::/32`, and IPv4-mapped addresses that resolve to a
///   disallowed IPv4 address per the rules above.
pub fn is_disallowed_ip(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => is_disallowed_ipv4(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_disallowed_ipv4(mapped);
            }
            is_disallowed_ipv6(v6)
        }
    }
}

fn is_disallowed_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    if a == 0 { return true; }            // 0.0.0.0/8
    if a == 10 { return true; }           // 10.0.0.0/8
    if a == 127 { return true; }          // loopback
    if a == 169 && b == 254 { return true; } // link-local
    if a == 172 && (16..=31).contains(&b) { return true; } // 172.16/12
    if a == 192 && b == 168 { return true; } // 192.168/16
    if a == 192 && b == 0 { return true; }   // 192.0.0/24 (reserved), includes 192.0.2 (TEST-NET-1)
    if a == 198 && b == 18 { return true; }  // 198.18/15 benchmarking
    if a == 198 && b == 19 { return true; }
    if a == 198 && b == 51 && ip.octets()[2] == 100 { return true; } // TEST-NET-2
    if a == 203 && b == 0 && ip.octets()[2] == 113 { return true; }  // TEST-NET-3
    if a == 100 && (64..=127).contains(&b) { return true; } // 100.64/10 CGNAT
    if a >= 224 { return true; }          // multicast + reserved + broadcast
    false
}

fn is_disallowed_ipv6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() { return true; }
    if ip.is_unspecified() { return true; }
    if ip.is_multicast() { return true; }

    let seg0 = ip.segments()[0];
    if (seg0 & 0xfe00) == 0xfc00 { return true; } // fc00::/7 ULA
    if (seg0 & 0xffc0) == 0xfe80 { return true; } // fe80::/10 link-local

    // 2001:db8::/32 documentation
    let segs = ip.segments();
    if segs[0] == 0x2001 && segs[1] == 0x0db8 { return true; }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> IpAddr {
        s.parse().expect("valid v4")
    }
    fn v6(s: &str) -> IpAddr {
        s.parse().expect("valid v6")
    }

    #[test]
    fn accepts_public_ipv4() {
        assert!(!is_disallowed_ip(v4("8.8.8.8")));
        assert!(!is_disallowed_ip(v4("1.1.1.1")));
        assert!(!is_disallowed_ip(v4("140.82.114.4"))); // github.com
    }

    #[test]
    fn rejects_ipv4_loopback() {
        assert!(is_disallowed_ip(v4("127.0.0.1")));
        assert!(is_disallowed_ip(v4("127.1.2.3")));
    }

    #[test]
    fn rejects_ipv4_10_8() {
        assert!(is_disallowed_ip(v4("10.0.0.1")));
        assert!(is_disallowed_ip(v4("10.255.255.255")));
    }

    #[test]
    fn rejects_ipv4_172_16_12() {
        assert!(is_disallowed_ip(v4("172.16.0.1")));
        assert!(is_disallowed_ip(v4("172.31.255.255")));
        // Boundaries: 172.15 and 172.32 should be allowed
        assert!(!is_disallowed_ip(v4("172.15.0.1")));
        assert!(!is_disallowed_ip(v4("172.32.0.1")));
    }

    #[test]
    fn rejects_ipv4_192_168_16() {
        assert!(is_disallowed_ip(v4("192.168.0.1")));
        assert!(is_disallowed_ip(v4("192.168.255.255")));
    }

    #[test]
    fn rejects_ipv4_169_254_16() {
        assert!(is_disallowed_ip(v4("169.254.169.254"))); // cloud metadata
        assert!(is_disallowed_ip(v4("169.254.0.1")));
    }

    #[test]
    fn rejects_ipv4_0_8() {
        assert!(is_disallowed_ip(v4("0.0.0.0")));
        assert!(is_disallowed_ip(v4("0.255.255.255")));
    }

    #[test]
    fn rejects_ipv4_cgnat() {
        assert!(is_disallowed_ip(v4("100.64.0.1")));
        assert!(is_disallowed_ip(v4("100.127.255.255")));
        // 100.63 and 100.128 are public
        assert!(!is_disallowed_ip(v4("100.63.0.1")));
        assert!(!is_disallowed_ip(v4("100.128.0.1")));
    }

    #[test]
    fn rejects_ipv4_test_nets() {
        assert!(is_disallowed_ip(v4("192.0.2.1")));    // TEST-NET-1
        assert!(is_disallowed_ip(v4("198.51.100.1"))); // TEST-NET-2
        assert!(is_disallowed_ip(v4("203.0.113.1"))); // TEST-NET-3
    }

    #[test]
    fn rejects_ipv4_benchmark() {
        assert!(is_disallowed_ip(v4("198.18.0.1")));
        assert!(is_disallowed_ip(v4("198.19.255.255")));
    }

    #[test]
    fn rejects_ipv4_multicast_and_reserved() {
        assert!(is_disallowed_ip(v4("224.0.0.1")));   // multicast
        assert!(is_disallowed_ip(v4("239.255.255.250")));
        assert!(is_disallowed_ip(v4("255.255.255.255"))); // broadcast
        assert!(is_disallowed_ip(v4("240.0.0.1")));   // reserved
    }

    #[test]
    fn rejects_ipv6_loopback() {
        assert!(is_disallowed_ip(v6("::1")));
    }

    #[test]
    fn rejects_ipv6_unspecified() {
        assert!(is_disallowed_ip(v6("::")));
    }

    #[test]
    fn rejects_ipv6_link_local() {
        assert!(is_disallowed_ip(v6("fe80::1")));
        assert!(is_disallowed_ip(v6("febf:ffff::1")));
    }

    #[test]
    fn rejects_ipv6_ula() {
        assert!(is_disallowed_ip(v6("fc00::1")));
        assert!(is_disallowed_ip(v6("fd00::1")));
        assert!(is_disallowed_ip(v6("fdff:ffff::1")));
    }

    #[test]
    fn rejects_ipv6_multicast() {
        assert!(is_disallowed_ip(v6("ff00::1")));
        assert!(is_disallowed_ip(v6("ff02::1")));
    }

    #[test]
    fn rejects_ipv6_documentation() {
        assert!(is_disallowed_ip(v6("2001:db8::1")));
    }

    #[test]
    fn rejects_ipv4_mapped_private_v6() {
        assert!(is_disallowed_ip(v6("::ffff:10.0.0.1")));
        assert!(is_disallowed_ip(v6("::ffff:192.168.1.1")));
        assert!(is_disallowed_ip(v6("::ffff:127.0.0.1")));
    }

    #[test]
    fn accepts_public_ipv4_mapped_v6() {
        assert!(!is_disallowed_ip(v6("::ffff:8.8.8.8")));
    }

    #[test]
    fn accepts_public_ipv6() {
        // Google public DNS v6
        assert!(!is_disallowed_ip(v6("2001:4860:4860::8888")));
    }
}
