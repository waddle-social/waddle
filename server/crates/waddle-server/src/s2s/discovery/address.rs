//! The address classes discovery never connects to (ADR-0020 D3; gate G6).
//!
//! An address learned from DNS, from an SRV target or from the A/AAAA
//! fallback, is refused when it is in a blocked class. The list completes
//! ADR-0020's table from the IANA IPv4 and IPv6 Special-Purpose Address
//! Registries: every block those registries mark as not globally reachable
//! is blocked, and the few globally reachable special-purpose blocks inside
//! larger blocked ones (anycast service addresses and AS112) are blocked
//! with them, because no XMPP server is reached at them. IPv6 is checked
//! against the global unicast space `2000::/3`: everything outside it is
//! reserved, multicast, link-local, unique local or otherwise not a server
//! address, except the NAT64 well-known prefix, whose embedded IPv4 address
//! is checked instead. An IPv4 address embedded by NAT64, 6to4, Teredo
//! (server and de-obfuscated client) or an ISATAP interface identifier is
//! checked as an IPv4 address; an IPv4-mapped or IPv4-compatible IPv6
//! address is refused outright, as an AAAA record has no reason to carry
//! one. The decisions are recorded in
//! `server/docs/planning/s2s-discovery-2047.md`.
//!
//! Network-specific NAT64 (RFC 6052) and 6rd (RFC 5969) prefixes embed
//! IPv4 addresses under prefixes an address alone does not reveal. The
//! operator lists them, or any other block, in
//! `discovery.blocked_prefixes`; [`BlockedAddresses`] refuses those blocks
//! with the built-in classes, and an operator-listed IPv4 block also
//! applies to every embedded IPv4 address checked above.
//!
//! Configured endpoints are the only exemption; they never pass through
//! here.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// An IPv4 block: the network address and prefix length.
struct V4Block(u32, u8);

/// IPv4 blocks never connected to.
const BLOCKED_V4: &[V4Block] = &[
    // "This network" and unspecified (RFC 791, RFC 1122).
    V4Block(0x0000_0000, 8),
    // Private use (RFC 1918).
    V4Block(0x0a00_0000, 8),
    // Shared address space, carrier-grade NAT (RFC 6598).
    V4Block(0x6440_0000, 10),
    // Loopback (RFC 1122).
    V4Block(0x7f00_0000, 8),
    // Link-local (RFC 3927).
    V4Block(0xa9fe_0000, 16),
    // Private use (RFC 1918).
    V4Block(0xac10_0000, 12),
    // IETF protocol assignments (RFC 6890), including DS-Lite, the dummy
    // address and the NAT64 discovery addresses, and the PCP and TURN
    // anycast addresses within it.
    V4Block(0xc000_0000, 24),
    // Documentation, TEST-NET-1 (RFC 5737).
    V4Block(0xc000_0200, 24),
    // Deprecated 6to4 relay anycast (RFC 7526).
    V4Block(0xc058_6300, 24),
    // Private use (RFC 1918).
    V4Block(0xc0a8_0000, 16),
    // Benchmarking (RFC 2544).
    V4Block(0xc612_0000, 15),
    // Documentation, TEST-NET-2 (RFC 5737).
    V4Block(0xc633_6400, 24),
    // Documentation, TEST-NET-3 (RFC 5737).
    V4Block(0xcb00_7100, 24),
    // Multicast (RFC 5771).
    V4Block(0xe000_0000, 4),
    // Reserved (RFC 1112) and limited broadcast (RFC 919).
    V4Block(0xf000_0000, 4),
];

/// An operator-listed address block: a network and its prefix length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressBlock {
    network: IpAddr,
    length: u8,
}

impl AddressBlock {
    /// Parse CIDR notation, `address/length`. The length is from one up to
    /// the address family's width, and the address has no bit set beyond
    /// it, so a mistyped block is refused instead of silently widened.
    pub fn parse(text: &str) -> Option<Self> {
        let (address, length) = text.split_once('/')?;
        let network: IpAddr = address.parse().ok()?;
        let length: u8 = length.parse().ok()?;
        let block = Self { network, length };
        let width = match network {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        ((1..=width).contains(&length) && block.host_bits_clear()).then_some(block)
    }

    fn host_bits_clear(&self) -> bool {
        match self.network {
            IpAddr::V4(network) => {
                let bits = u32::from(network);
                bits & v4_mask(self.length) == bits
            }
            IpAddr::V6(network) => {
                let bits = u128::from(network);
                bits & v6_mask(self.length) == bits
            }
        }
    }

    /// Whether `address` is within the block. An IPv4 block never contains
    /// an IPv6 address or the reverse.
    pub fn contains(&self, address: IpAddr) -> bool {
        match (self.network, address) {
            (IpAddr::V4(network), IpAddr::V4(address)) => {
                within(u32::from(address), u32::from(network), self.length)
            }
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                let mask = v6_mask(self.length);
                u128::from(address) & mask == u128::from(network) & mask
            }
            _ => false,
        }
    }
}

/// The built-in blocked classes and the operator's own blocks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockedAddresses {
    extra: Vec<AddressBlock>,
}

impl BlockedAddresses {
    /// The built-in classes and `extra`.
    pub fn new(extra: Vec<AddressBlock>) -> Self {
        Self { extra }
    }

    /// The operator's own blocks.
    pub fn extra(&self) -> &[AddressBlock] {
        &self.extra
    }

    /// Whether discovery refuses `address`: it is in a built-in class or an
    /// operator-listed block, or it embeds an IPv4 address that is.
    pub fn is_blocked(&self, address: IpAddr) -> bool {
        if self.extra.iter().any(|block| block.contains(address)) {
            return true;
        }
        match address {
            IpAddr::V4(address) => self.blocks_v4(address),
            IpAddr::V6(address) => blocked_v6(address, &|embedded| self.blocks_v4(embedded)),
        }
    }

    fn blocks_v4(&self, address: Ipv4Addr) -> bool {
        is_blocked_v4(address)
            || self
                .extra
                .iter()
                .any(|block| block.contains(IpAddr::V4(address)))
    }
}

/// Whether discovery refuses `address` under the built-in classes alone.
pub fn is_blocked(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_blocked_v4(address),
        IpAddr::V6(address) => is_blocked_v6(address),
    }
}

/// Whether discovery refuses the IPv4 `address`.
pub fn is_blocked_v4(address: Ipv4Addr) -> bool {
    let bits = u32::from(address);
    BLOCKED_V4
        .iter()
        .any(|V4Block(network, length)| within(bits, *network, *length))
}

fn v4_mask(length: u8) -> u32 {
    u32::MAX.checked_shl(32 - u32::from(length)).unwrap_or(0)
}

fn v6_mask(length: u8) -> u128 {
    u128::MAX.checked_shl(128 - u32::from(length)).unwrap_or(0)
}

fn within(bits: u32, network: u32, length: u8) -> bool {
    let mask = v4_mask(length);
    bits & mask == network & mask
}

/// Whether discovery refuses the IPv6 `address`.
pub fn is_blocked_v6(address: Ipv6Addr) -> bool {
    blocked_v6(address, &is_blocked_v4)
}

/// Whether the IPv6 `address` is in a built-in class, checking every
/// embedded IPv4 address with `blocked_v4`.
fn blocked_v6(address: Ipv6Addr, blocked_v4: &dyn Fn(Ipv4Addr) -> bool) -> bool {
    let bits = u128::from(address);
    let segments = address.segments();
    // NAT64 well-known prefix 64:ff9b::/96 (RFC 6052): the embedded IPv4
    // address decides. It is outside 2000::/3, so it is checked first.
    if bits >> 32 == 0x0064_ff9b_0000_0000_0000_0000 {
        return blocked_v4(low_v4(bits));
    }
    // Everything outside global unicast 2000::/3: unspecified, loopback,
    // IPv4-mapped and IPv4-compatible, the local-use NAT64 prefix
    // 64:ff9b:1::/48, the discard and dummy prefixes, SRv6 SIDs 5f00::/16,
    // unique local fc00::/7, link-local fe80::/10, the deprecated
    // site-local fec0::/10, multicast ff00::/8 and unassigned space.
    if segments[0] & 0xe000 != 0x2000 {
        return true;
    }
    // Teredo 2001::/32 (RFC 4380): the server address and the
    // de-obfuscated client address both decide.
    if segments[0] == 0x2001 && segments[1] == 0x0000 {
        let server = Ipv4Addr::from(((bits >> 64) & 0xffff_ffff) as u32);
        let client = Ipv4Addr::from(!low_v4_bits(bits));
        return blocked_v4(server) || blocked_v4(client);
    }
    // IETF protocol assignments 2001::/23 (RFC 2928), with the globally
    // reachable anycast, AMT, AS112 and ORCHIDv2 blocks inside it.
    if segments[0] == 0x2001 && segments[1] < 0x0200 {
        return true;
    }
    // Documentation 2001:db8::/32 (RFC 3849) and 3fff::/20 (RFC 9637).
    if (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] == 0x3fff && segments[1] & 0xf000 == 0)
    {
        return true;
    }
    // 6to4 2002::/16 (RFC 3056): the embedded IPv4 address decides.
    if segments[0] == 0x2002 {
        let embedded = Ipv4Addr::from(((bits >> 80) & 0xffff_ffff) as u32);
        return blocked_v4(embedded);
    }
    // An ISATAP interface identifier (RFC 5214) embeds an IPv4 address in
    // any prefix.
    let interface = bits as u64;
    if interface >> 32 == 0x0000_5efe || interface >> 32 == 0x0200_5efe {
        return blocked_v4(low_v4(bits));
    }
    false
}

fn low_v4_bits(bits: u128) -> u32 {
    (bits & 0xffff_ffff) as u32
}

fn low_v4(bits: u128) -> Ipv4Addr {
    Ipv4Addr::from(low_v4_bits(bits))
}
