// Verus formal proof: SSRF IPv4/IPv6/IPv4-mapped blocklist completeness.
// Paired with `server/crates/waddle-broker-core/src/kernels/ssrf_ip_filter.rs`.
//
// Proves completeness over:
// - IPv4: 0.0.0.0/8 (unspecified), 10.0.0.0/8 (RFC1918), 100.64.0.0/10 (CGNAT),
//   127.0.0.0/8 (loopback), 169.254.0.0/16 (link-local/metadata), 172.16.0.0/12 (RFC1918),
//   192.168.0.0/16 (RFC1918), 224.0.0.0/4 (multicast), 240.0.0.0/4 (reserved/broadcast).
// - IPv6: :: (unspecified), ::1 (loopback), fc00::/7 (ULA), fe80::/10 (link-local),
//   ff00::/8 (multicast), and ::ffff:0:0/96 whenever the embedded IPv4 is blocked.

use vstd::prelude::*;

verus! {

pub open spec fn is_blocked_ipv4_spec(a: u8, b: u8, _c: u8, _d: u8) -> bool {
    a == 0u8
        || a == 10u8
        || (a == 100u8 && b >= 64u8 && b <= 127u8)
        || a == 127u8
        || (a == 169u8 && b == 254u8)
        || (a == 172u8 && b >= 16u8 && b <= 31u8)
        || (a == 192u8 && b == 168u8)
        || (a >= 224u8 && a <= 239u8)
        || a >= 240u8
}

pub fn is_blocked_ipv4(octets: [u8; 4]) -> (blocked: bool)
    ensures
        blocked <==> is_blocked_ipv4_spec(octets[0], octets[1], octets[2], octets[3]),
{
    let a = octets[0];
    let b = octets[1];

    let unspecified = a == 0u8;
    let rfc1918_10 = a == 10u8;
    let cgnat = a == 100u8 && (b & 0b1100_0000u8) == 64u8;
    let loopback = a == 127u8;
    let link_local = a == 169u8 && b == 254u8;
    let rfc1918_172 = a == 172u8 && (b & 0b1111_0000u8) == 16u8;
    let rfc1918_192 = a == 192u8 && b == 168u8;
    let multicast = (a & 0b1111_0000u8) == 224u8;
    let reserved_or_broadcast = (a & 0b1111_0000u8) == 240u8;

    proof {
        assert(((b & 0b1100_0000u8) == 64u8) <==> (b >= 64u8 && b <= 127u8)) by (bit_vector);
        assert(((b & 0b1111_0000u8) == 16u8) <==> (b >= 16u8 && b <= 31u8)) by (bit_vector);
        assert(((a & 0b1111_0000u8) == 224u8) <==> (a >= 224u8 && a <= 239u8)) by (bit_vector);
        assert(((a & 0b1111_0000u8) == 240u8) <==> (a >= 240u8)) by (bit_vector);
    }

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

pub open spec fn is_blocked_ipv6_spec(segs: [u16; 8]) -> bool {
    let z = segs[0] == 0u16 && segs[1] == 0u16 && segs[2] == 0u16 && segs[3] == 0u16 && segs[4] == 0u16;
    let unspecified = z && segs[5] == 0u16 && segs[6] == 0u16 && segs[7] == 0u16;
    let loopback = z && segs[5] == 0u16 && segs[6] == 0u16 && segs[7] == 1u16;
    let ipv4_compatible = z && segs[5] == 0u16;
    let ipv4_translated = segs[0] == 0u16
        && segs[1] == 0u16
        && segs[2] == 0u16
        && segs[3] == 0u16
        && segs[4] == 0xffffu16
        && segs[5] == 0u16;
    let nat64 = segs[0] == 0x0064u16
        && segs[1] == 0xff9bu16
        && ((segs[2] == 0u16 && segs[3] == 0u16 && segs[4] == 0u16 && segs[5] == 0u16)
            || segs[2] == 0x0001u16);
    let six_to_four = segs[0] == 0x2002u16;
    let teredo = segs[0] == 0x2001u16 && segs[1] == 0u16;
    let discard_only = segs[0] == 0x0100u16 && segs[1] == 0u16 && segs[2] == 0u16 && segs[3] == 0u16;
    let ula = segs[0] >= 0xfc00u16 && segs[0] <= 0xfdffu16;
    let link_local = segs[0] >= 0xfe80u16 && segs[0] <= 0xfebfu16;
    let multicast = segs[0] >= 0xff00u16;
    let mapped_v4_blocked = z && segs[5] == 0xffffu16
        && is_blocked_ipv4_spec(
            (segs[6] >> 8u16) as u8,
            (segs[6] & 0xffu16) as u8,
            (segs[7] >> 8u16) as u8,
            (segs[7] & 0xffu16) as u8,
        );
    unspecified
        || loopback
        || ipv4_compatible
        || ipv4_translated
        || nat64
        || six_to_four
        || teredo
        || discard_only
        || ula
        || link_local
        || multicast
        || mapped_v4_blocked
}

pub fn is_blocked_ipv6(segs: [u16; 8]) -> (blocked: bool)
    ensures
        blocked <==> is_blocked_ipv6_spec(segs),
{
    let z = segs[0] == 0u16 && segs[1] == 0u16 && segs[2] == 0u16 && segs[3] == 0u16 && segs[4] == 0u16;
    let unspecified = z && segs[5] == 0u16 && segs[6] == 0u16 && segs[7] == 0u16;
    let loopback = z && segs[5] == 0u16 && segs[6] == 0u16 && segs[7] == 1u16;
    let ipv4_compatible = z && segs[5] == 0u16;
    let ipv4_translated = segs[0] == 0u16
        && segs[1] == 0u16
        && segs[2] == 0u16
        && segs[3] == 0u16
        && segs[4] == 0xffffu16
        && segs[5] == 0u16;
    let nat64 = segs[0] == 0x0064u16
        && segs[1] == 0xff9bu16
        && ((segs[2] == 0u16 && segs[3] == 0u16 && segs[4] == 0u16 && segs[5] == 0u16)
            || segs[2] == 0x0001u16);
    let six_to_four = segs[0] == 0x2002u16;
    let teredo = segs[0] == 0x2001u16 && segs[1] == 0u16;
    let discard_only = segs[0] == 0x0100u16 && segs[1] == 0u16 && segs[2] == 0u16 && segs[3] == 0u16;
    let s0 = segs[0];
    let ula = (s0 & 0xfe00u16) == 0xfc00u16;
    let link_local = (s0 & 0xffc0u16) == 0xfe80u16;
    let multicast = (s0 & 0xff00u16) == 0xff00u16;

    proof {
        assert(((s0 & 0xfe00u16) == 0xfc00u16) <==> (s0 >= 0xfc00u16 && s0 <= 0xfdffu16)) by (bit_vector);
        assert(((s0 & 0xffc0u16) == 0xfe80u16) <==> (s0 >= 0xfe80u16 && s0 <= 0xfebfu16)) by (bit_vector);
        assert(((s0 & 0xff00u16) == 0xff00u16) <==> (s0 >= 0xff00u16)) by (bit_vector);
    }

    let mapped_v4_blocked = if z && segs[5] == 0xffffu16 {
        let v4: [u8; 4] = [
            (segs[6] >> 8u16) as u8,
            (segs[6] & 0xffu16) as u8,
            (segs[7] >> 8u16) as u8,
            (segs[7] & 0xffu16) as u8,
        ];
        is_blocked_ipv4(v4)
    } else {
        false
    };

    unspecified
        || loopback
        || ipv4_compatible
        || ipv4_translated
        || nat64
        || six_to_four
        || teredo
        || discard_only
        || ula
        || link_local
        || multicast
        || mapped_v4_blocked
}

pub proof fn lemma_metadata_and_loopback_blocked(c: u8, d: u8)
    ensures
        is_blocked_ipv4_spec(169u8, 254u8, c, d),
        is_blocked_ipv4_spec(127u8, 0u8, c, d),
        is_blocked_ipv4_spec(10u8, 0u8, c, d),
        is_blocked_ipv4_spec(192u8, 168u8, c, d),
        is_blocked_ipv4_spec(255u8, 255u8, 255u8, 255u8),
{
}

pub proof fn lemma_ipv6_transition_and_discard_blocked(s6: u16, s7: u16)
    ensures
        is_blocked_ipv6_spec([0u16, 0u16, 0u16, 0u16, 0u16, 0u16, s6, s7]),
        is_blocked_ipv6_spec([0u16, 0u16, 0u16, 0u16, 0xffffu16, 0u16, s6, s7]),
        is_blocked_ipv6_spec([0x0064u16, 0xff9bu16, 0u16, 0u16, 0u16, 0u16, s6, s7]),
        is_blocked_ipv6_spec([0x0064u16, 0xff9bu16, 0x0001u16, 0u16, 0u16, 0u16, s6, s7]),
        is_blocked_ipv6_spec([0x2002u16, s6, s7, 0u16, 0u16, 0u16, 0u16, 1u16]),
        is_blocked_ipv6_spec([0x2001u16, 0u16, 0u16, 0u16, 0u16, 0u16, s6, s7]),
        is_blocked_ipv6_spec([0x0100u16, 0u16, 0u16, 0u16, 0u16, 0u16, s6, s7]),
{
}

fn main() {}

} // verus!
