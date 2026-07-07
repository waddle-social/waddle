//! Module-level tests for the XEP-0397 rewrite (ADR-0017 Phase 3 Slice 8).
//! Wire-shape tests live in `wire.rs`; store-contract tests live in
//! `store.rs`.

use super::*;

#[test]
fn generated_tokens_have_at_least_128_bits_of_entropy() {
    // XEP-0397: "MUST contain at least 128 bit of entropy." Base64
    // (no padding) encodes 6 bits/char, so >= 128 bits needs >= 22 chars.
    let token = generate_isr_token();
    let decoded = base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, &token)
        .expect("token is valid base64url");
    assert!(
        decoded.len() * 8 >= 128,
        "expected >= 128 bits of entropy, got {} bits",
        decoded.len() * 8
    );
}

#[test]
fn generated_tokens_are_unique() {
    let a = generate_isr_token();
    let b = generate_isr_token();
    assert_ne!(a, b);
}

#[test]
fn isr_ns_uses_the_https_spelling_not_the_vendored_xep_typo() {
    // ADR-0017 Phase 3 Slice 8 XEP fact-check: the vendored XEP-0397 source
    // itself typos this as `htpps://...` in two places; the Registrar
    // Considerations section confirms `https://` is canonical.
    assert_eq!(ISR_NS, "https://xmpp.org/extensions/isr/0");
    assert!(!ISR_NS.starts_with("htpps"));
}
