// Verus formal proof: Length-prefixed AAD encoder injectivity.
// Paired with `server/crates/waddle-broker-core/src/kernels/aad_codec.rs`.
//
// Proves:
// 1. `u32` big-endian encoding is injective.
// 2. Prefixing a byte sequence with its 4-byte big-endian length is prefix-free / injective.
// 3. Encoding all 6 AAD fields `(trust_domain, tenant_id, account_id, provider, credential_id, version)`
//    is injective: `encode_aad_spec(a) == encode_aad_spec(b) ==> a == b`.
// 4. Executable `encode_field` matches `encode_field_spec` with zero length-cast overflow.

use vstd::prelude::*;

verus! {

pub struct AadSpec {
    pub trust_domain: Seq<u8>,
    pub tenant_id: Seq<u8>,
    pub account_id: Seq<u8>,
    pub provider: Seq<u8>,
    pub credential_id: Seq<u8>,
    pub version: Seq<u8>,
}

pub open spec fn u32_be_spec(n: u32) -> Seq<u8> {
    seq![
        ((n >> 24u32) & 0xffu32) as u8,
        ((n >> 16u32) & 0xffu32) as u8,
        ((n >> 8u32) & 0xffu32) as u8,
        (n & 0xffu32) as u8,
    ]
}

pub open spec fn encode_field_spec(f: Seq<u8>) -> Seq<u8>
    recommends f.len() <= u32::MAX as int,
{
    u32_be_spec(f.len() as u32) + f
}

pub open spec fn valid_aad(a: AadSpec) -> bool {
    a.trust_domain.len() <= u32::MAX as int
        && a.tenant_id.len() <= u32::MAX as int
        && a.account_id.len() <= u32::MAX as int
        && a.provider.len() <= u32::MAX as int
        && a.credential_id.len() <= u32::MAX as int
        && a.version.len() <= u32::MAX as int
}

pub open spec fn encode_aad_spec(a: AadSpec) -> Seq<u8> {
    encode_field_spec(a.trust_domain)
        + encode_field_spec(a.tenant_id)
        + encode_field_spec(a.account_id)
        + encode_field_spec(a.provider)
        + encode_field_spec(a.credential_id)
        + encode_field_spec(a.version)
}

pub proof fn lemma_u32_be_injective(x: u32, y: u32)
    requires
        u32_be_spec(x) == u32_be_spec(y),
    ensures
        x == y,
{
    let bx = u32_be_spec(x);
    let by = u32_be_spec(y);
    assert(bx[0] == by[0]);
    assert(bx[1] == by[1]);
    assert(bx[2] == by[2]);
    assert(bx[3] == by[3]);
    assert(
        (((x >> 24u32) & 0xffu32) as u8 == ((y >> 24u32) & 0xffu32) as u8
            && ((x >> 16u32) & 0xffu32) as u8 == ((y >> 16u32) & 0xffu32) as u8
            && ((x >> 8u32) & 0xffu32) as u8 == ((y >> 8u32) & 0xffu32) as u8
            && (x & 0xffu32) as u8 == (y & 0xffu32) as u8)
            ==> x == y
    ) by (bit_vector);
}

pub proof fn lemma_encode_field_prefix_injective(
    a: Seq<u8>,
    b: Seq<u8>,
    rest_a: Seq<u8>,
    rest_b: Seq<u8>,
)
    requires
        a.len() <= u32::MAX as int,
        b.len() <= u32::MAX as int,
        encode_field_spec(a) + rest_a == encode_field_spec(b) + rest_b,
    ensures
        a == b,
        rest_a == rest_b,
{
    let full_a = encode_field_spec(a) + rest_a;
    let full_b = encode_field_spec(b) + rest_b;

    let la = u32_be_spec(a.len() as u32);
    let lb = u32_be_spec(b.len() as u32);
    assert(full_a.subrange(0, 4) =~= la);
    assert(full_b.subrange(0, 4) =~= lb);
    assert(la =~= lb);
    lemma_u32_be_injective(a.len() as u32, b.len() as u32);
    assert(a.len() == b.len());

    let n = a.len() as int;
    assert(full_a.subrange(4, 4 + n) =~= a);
    assert(full_b.subrange(4, 4 + n) =~= b);
    assert(a =~= b);

    assert(full_a.subrange(4 + n, full_a.len() as int) =~= rest_a);
    assert(full_b.subrange(4 + n, full_b.len() as int) =~= rest_b);
    assert(rest_a =~= rest_b);
}

pub proof fn lemma_aad_encode_injective(x: AadSpec, y: AadSpec)
    requires
        valid_aad(x),
        valid_aad(y),
        encode_aad_spec(x) == encode_aad_spec(y),
    ensures
        x == y,
{
    let rx1 = encode_field_spec(x.tenant_id)
        + encode_field_spec(x.account_id)
        + encode_field_spec(x.provider)
        + encode_field_spec(x.credential_id)
        + encode_field_spec(x.version);
    let ry1 = encode_field_spec(y.tenant_id)
        + encode_field_spec(y.account_id)
        + encode_field_spec(y.provider)
        + encode_field_spec(y.credential_id)
        + encode_field_spec(y.version);
    assert(encode_aad_spec(x) =~= encode_field_spec(x.trust_domain) + rx1);
    assert(encode_aad_spec(y) =~= encode_field_spec(y.trust_domain) + ry1);
    lemma_encode_field_prefix_injective(x.trust_domain, y.trust_domain, rx1, ry1);

    let rx2 = encode_field_spec(x.account_id)
        + encode_field_spec(x.provider)
        + encode_field_spec(x.credential_id)
        + encode_field_spec(x.version);
    let ry2 = encode_field_spec(y.account_id)
        + encode_field_spec(y.provider)
        + encode_field_spec(y.credential_id)
        + encode_field_spec(y.version);
    assert(rx1 =~= encode_field_spec(x.tenant_id) + rx2);
    assert(ry1 =~= encode_field_spec(y.tenant_id) + ry2);
    lemma_encode_field_prefix_injective(x.tenant_id, y.tenant_id, rx2, ry2);

    let rx3 = encode_field_spec(x.provider)
        + encode_field_spec(x.credential_id)
        + encode_field_spec(x.version);
    let ry3 = encode_field_spec(y.provider)
        + encode_field_spec(y.credential_id)
        + encode_field_spec(y.version);
    assert(rx2 =~= encode_field_spec(x.account_id) + rx3);
    assert(ry2 =~= encode_field_spec(y.account_id) + ry3);
    lemma_encode_field_prefix_injective(x.account_id, y.account_id, rx3, ry3);

    let rx4 = encode_field_spec(x.credential_id) + encode_field_spec(x.version);
    let ry4 = encode_field_spec(y.credential_id) + encode_field_spec(y.version);
    assert(rx3 =~= encode_field_spec(x.provider) + rx4);
    assert(ry3 =~= encode_field_spec(y.provider) + ry4);
    lemma_encode_field_prefix_injective(x.provider, y.provider, rx4, ry4);

    let rx5 = encode_field_spec(x.version);
    let ry5 = encode_field_spec(y.version);
    assert(rx4 =~= encode_field_spec(x.credential_id) + rx5);
    assert(ry4 =~= encode_field_spec(y.credential_id) + ry5);
    lemma_encode_field_prefix_injective(x.credential_id, y.credential_id, rx5, ry5);

    assert(rx5 =~= encode_field_spec(x.version) + Seq::<u8>::empty());
    assert(ry5 =~= encode_field_spec(y.version) + Seq::<u8>::empty());
    lemma_encode_field_prefix_injective(
        x.version,
        y.version,
        Seq::<u8>::empty(),
        Seq::<u8>::empty(),
    );
}

pub fn encode_field_exec(field: &[u8]) -> (out: Vec<u8>)
    requires
        field@.len() <= u32::MAX as int,
    ensures
        out@ == encode_field_spec(field@),
{
    let len: u32 = field.len() as u32;
    let mut out: Vec<u8> = Vec::new();
    out.push(((len >> 24u32) & 0xffu32) as u8);
    out.push(((len >> 16u32) & 0xffu32) as u8);
    out.push(((len >> 8u32) & 0xffu32) as u8);
    out.push((len & 0xffu32) as u8);

    let mut i: usize = 0;
    while i < field.len()
        invariant
            0 <= i <= field.len(),
            out@ =~= u32_be_spec(len) + field@.subrange(0, i as int),
        decreases field.len() - i,
    {
        out.push(field[i]);
        i = i + 1;
    }
    assert(field@.subrange(0, field.len() as int) =~= field@);
    assert(out@ =~= encode_field_spec(field@));
    out
}

fn main() {}

} // verus!
