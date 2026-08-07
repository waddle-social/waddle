//! Resume-identity witness minting (ADR-0017 Phase 3 Slice 1 stub; the
//! resume call sites themselves land in Slice 6).
//!
//! [`verify_resume_identity`] is the **sole constructor**, in either crate,
//! of [`ResumeIdentityProof`] — see that type's doc comment for the
//! compiler-enforcement argument. [`ResumeIdentityProof`] is declared in
//! *this* module (not `ownership::mod`) so the "sole constructor" claim is
//! literally true: Rust's privacy rule extends a private field's visibility
//! to the defining module's descendants only, never to its ancestors or
//! siblings, so a type declared here is unconstructable from `ownership::mod`
//! itself or any sibling submodule (e.g. `in_process`) — only from `resume`
//! and its own descendants (none exist).

use jid::BareJid;

/// Witness that a resume attempt's locally SASL-authenticated bare JID was
/// checked against the detached snapshot's bound bare JID **before** any
/// write (element 8's identity-binding rule).
///
/// **Compiler-enforced, not conventional** (this is the ADR's own framing,
/// element 4): the `_private` field is not `pub`, and this type is declared
/// in `ownership::resume` rather than `ownership::mod`, so only code inside
/// *this* module (there are no descendants) can construct a value — not
/// `ownership::mod` itself, and not any sibling submodule such as
/// `in_process`. [`verify_resume_identity`] is therefore the *only*
/// function in the entire workspace capable of producing one:
///
/// ```compile_fail
/// // Constructing this outside `ownership::resume` does not compile:
/// // `_private` is a private field, and this module is not a descendant
/// // of `ownership::resume`.
/// let _ = waddle_xmpp::ownership::ResumeIdentityProof { _private: () };
/// ```
///
/// Nor can a caller smuggle one into [`ClaimStore::steal_for_resume`]
/// ([`super::ClaimStore::steal_for_resume`]) by constructing it inline at
/// the call site — the same privacy boundary applies there too:
///
/// ```compile_fail
/// # async fn f(store: &dyn waddle_xmpp::ownership::ClaimStore) {
/// // Does not compile: `ResumeIdentityProof { _private: () }` is
/// // unconstructable outside `ownership::resume`, so this call site can
/// // never manufacture a witness for itself.
/// store.steal_for_resume(
///     &waddle_xmpp::ownership::Entity::new(waddle_xmpp::ownership::EntityType::SmSession, "x"),
///     waddle_xmpp::ownership::ClaimEpoch(0),
///     waddle_xmpp::ownership::ResumeIdentityProof { _private: () },
///     &waddle_xmpp::ownership::NodeIdentity::local(),
/// );
/// # }
/// ```
///
/// This is what makes [`ClaimStore::steal_for_resume`]
/// ([`super::ClaimStore::steal_for_resume`]) — the consent/epoch-only CAS
/// variant that steals from a fresh-lease owner — callable only from the
/// identity-checked resume path: "any enrolled node displacing a
/// fresh-lease owner without an identity check" is unrepresentable, not
/// merely forbidden by review.
#[derive(Debug)]
pub struct ResumeIdentityProof {
    _private: (),
}

/// Check a resume attempt's locally SASL-authenticated bare JID against the
/// detached snapshot's bound bare JID (element 8's "identity check before
/// any write" rule). Returns `Some` only on an exact match; a mismatch
/// returns `None` so the caller can answer XEP-0198 `<failed/>`
/// `not-authorized` without ever touching the claim epoch.
///
/// Slice 6 wires the real `stream_management.rs` resume call sites against
/// this function; this slice lands the function itself so
/// [`super::ClaimStore::steal_for_resume`]'s
/// signature can name [`ResumeIdentityProof`] today.
pub fn verify_resume_identity(
    sasl_identity: &BareJid,
    snapshot_owner: &BareJid,
) -> Option<ResumeIdentityProof> {
    if sasl_identity == snapshot_owner {
        Some(ResumeIdentityProof { _private: () })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_bare_jids_mint_a_proof() {
        let jid: BareJid = "alice@example.com".parse().expect("valid jid");
        assert!(verify_resume_identity(&jid, &jid).is_some());
    }

    #[test]
    fn mismatched_bare_jids_return_none() {
        let sasl: BareJid = "alice@example.com".parse().expect("valid jid");
        let snapshot: BareJid = "mallory@example.com".parse().expect("valid jid");
        assert!(verify_resume_identity(&sasl, &snapshot).is_none());
    }
}
