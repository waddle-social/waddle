//! The blocked address classes: ADR-0020 D3's table, completed from the
//! IANA special-purpose address registries, with embedded IPv4 checks; and
//! the operator's own blocked prefixes, applied with them.

use std::net::IpAddr;

use waddle_server::s2s::discovery::address::{is_blocked, AddressBlock, BlockedAddresses};

fn ip(text: &str) -> IpAddr {
    text.parse().expect("an address")
}

#[test]
fn every_blocked_class_is_refused() {
    let blocked = [
        // ADR-0020 D3, IPv4.
        "0.0.0.0",
        "0.1.2.3",
        "127.0.0.1",
        "127.255.255.254",
        "10.1.2.3",
        "172.16.0.1",
        "172.31.255.255",
        "192.168.0.1",
        "169.254.1.1",
        "100.64.0.1",
        "100.127.255.255",
        "224.0.0.1",
        "239.255.255.255",
        "255.255.255.255",
        "240.0.0.1",
        "192.0.2.1",
        "198.51.100.1",
        "203.0.113.1",
        // IANA additions: IETF protocol assignments, benchmarking and the
        // deprecated 6to4 relay anycast.
        "192.0.0.1",
        "192.0.0.9",
        "192.0.0.170",
        "198.18.0.1",
        "198.19.255.255",
        "192.88.99.1",
        // ADR-0020 D3, IPv6.
        "::",
        "::1",
        "fc00::1",
        "fd12:3456::1",
        "fe80::1",
        "ff02::1",
        "2001:db8::1",
        // IPv4-mapped and IPv4-compatible addresses, whatever they embed.
        "::ffff:8.8.8.8",
        "::ffff:127.0.0.1",
        "::8.8.8.8",
        // NAT64 well-known prefix embedding a blocked address, and the
        // local-use NAT64 prefix.
        "64:ff9b::7f00:1",
        "64:ff9b::c0a8:101",
        "64:ff9b:1::808:808",
        // 6to4 embedding a private address.
        "2002:c0a8:101::1",
        "2002:7f00:1::1",
        // Teredo with a blocked server, and with a blocked client (the
        // client address is stored with every bit inverted).
        "2001:0:a00:1::fefd:fcfb",
        "2001:0:808:808::3f57:fefe",
        // ISATAP interface identifiers embedding a blocked address.
        "2a00:1450::5efe:c0a8:101",
        "2a00:1450::200:5efe:a00:1",
        // IETF protocol assignments 2001::/23, documentation 3fff::/20,
        // SRv6 SIDs, discard-only and deprecated site-local.
        "2001:2::1",
        "2001:20::1",
        "3fff::1",
        "3fff:fff::1",
        "5f00::1",
        "100::1",
        "fec0::1",
    ];
    for address in blocked {
        assert!(is_blocked(ip(address)), "{address} must be blocked");
    }
}

#[test]
fn globally_reachable_addresses_are_allowed() {
    let allowed = [
        "8.8.8.8",
        "1.1.1.1",
        "100.128.0.1",
        "172.32.0.1",
        "198.20.0.1",
        "192.0.3.1",
        "223.255.255.254",
        "2a00:1450::1",
        "2606:4700::1111",
        // NAT64, 6to4, Teredo and ISATAP forms embedding public addresses.
        "64:ff9b::808:808",
        "2002:808:808::1",
        "2001:0:808:808::fefd:fcfb",
        "2a00:1450::200:5efe:808:808",
    ];
    for address in allowed {
        assert!(!is_blocked(ip(address)), "{address} must be allowed");
    }
}

#[test]
fn address_blocks_parse_only_exact_cidr_notation() {
    for text in [
        "8.8.4.0/24",
        "2a00:64::/96",
        "192.0.2.1/32",
        "2a00::1/128",
        "128.0.0.0/1",
    ] {
        assert!(AddressBlock::parse(text).is_some(), "{text} must parse");
    }
    for text in [
        "8.8.4.1/24",
        "2a00:64::1/96",
        "8.8.4.0/0",
        "::/0",
        "8.8.4.0/33",
        "2a00::/129",
        "8.8.4.0",
        "8.8.4.0/",
        "8.8.4.0/x",
        "peer.example/24",
        "[2a00::]/16",
    ] {
        assert!(
            AddressBlock::parse(text).is_none(),
            "{text} must be refused"
        );
    }
    let block = AddressBlock::parse("8.8.4.0/24").expect("a block");
    assert!(block.contains(ip("8.8.4.255")));
    assert!(!block.contains(ip("8.8.5.0")));
    // An IPv4 block never contains an IPv6 address, mapped or not.
    assert!(!block.contains(ip("::ffff:8.8.4.4")));
}

#[test]
fn operator_blocks_apply_with_the_built_in_classes() {
    let blocked = BlockedAddresses::new(vec![
        AddressBlock::parse("2a00:64::/96").expect("a network-specific NAT64 prefix"),
        AddressBlock::parse("8.8.4.0/24").expect("an IPv4 block"),
    ]);
    for address in [
        // The listed blocks themselves.
        "2a00:64::808:808",
        "8.8.4.4",
        // An IPv4 block also covers the IPv4 addresses embedded by 6to4,
        // Teredo (server address), the NAT64 well-known prefix and ISATAP.
        "2002:808:404::1",
        "2001:0:808:404::f7f7:f7f7",
        "64:ff9b::808:404",
        "2a00:1450::5efe:808:404",
        // The built-in classes still apply.
        "10.0.0.1",
        "::1",
    ] {
        assert!(blocked.is_blocked(ip(address)), "{address} must be blocked");
        if !address.starts_with("10.") && address != "::1" {
            assert!(!is_blocked(ip(address)), "{address} is not built in");
        }
    }
    for address in ["8.8.8.8", "2a00:65::808:808", "2002:808:808::1"] {
        assert!(
            !blocked.is_blocked(ip(address)),
            "{address} must be allowed"
        );
    }
}
