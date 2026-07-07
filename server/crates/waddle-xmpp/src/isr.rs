//! XEP-0397 Instant Stream Resumption (ISR).
//!
//! ADR-0017 Phase 3 Slice 8 rewrite: the previous implementation minted and
//! returned tokens via a standalone `urn:xmpp:isr:0` IQ round-trip
//! (`token-request`/`token`), which is a XEP-0397 conformance violation —
//! the XEP mints and returns an ISR token exclusively as an **inline**
//! `<isr-enable/>`/`<isr-enabled/>` element riding XEP-0198's
//! `<enable/>`/`<enabled/>`, and performs instant resumption exclusively as
//! an inline `<inst-resume/>`/`<inst-resumed/>`/`<inst-resume-failed/>`
//! element riding a SASL2 (XEP-0388) `<authenticate/>`/`<success/>`. The IQ
//! path has been retired outright (see the phase plan's "IQ-issuance
//! retirement inventory").
//!
//! ## Protocol overview (as actually implemented here)
//!
//! - The server advertises `<isr xmlns='{ISR_NS}'><mechanisms
//!   xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><mechanism>PLAIN</mechanism>
//!   </mechanisms></isr>` as a stream feature, and `{ISR_NS}` as a
//!   disco#info feature — **only** when `clustering.enabled && Postgres`
//!   (ADR-0017 Phase 3 Slice 8, Q8's compounding decision).
//! - A client requests a token by adding `<isr-enable mechanism='PLAIN'/>`
//!   (qualified by `{ISR_NS}`) to its `<enable/>`. The server's `<enabled/>`
//!   reply contains `<isr-enabled token='...'/>` when a token was minted.
//! - A client performs instant resumption on a **fresh** connection by
//!   sending a SASL2 `<authenticate mechanism='PLAIN'>` whose
//!   `<initial-response>` is a SASL PLAIN blob (`\0<bare-jid>\0<token>`,
//!   i.e. the ISR token stands in for the password — XEP-0397's "pinned
//!   mechanism" design) and which carries an inline `<inst-resume
//!   with-isr-token='true'><resume .../></inst-resume>`. Only `PLAIN` is
//!   supported as the pinned ISR mechanism in this implementation
//!   (deviation, see the phase plan: HT-SHA-256-ENDP, the XEP's own
//!   recommended mechanism, has no implementation in this codebase).
//!
//! This module defines the wire shapes and the [`IsrTokenStore`] trait.
//! `waddle-server`'s WebSocket layer owns the actual dispatch (parsing
//! `<enable>`/`<authenticate>`, calling the store, building replies).

mod store;
mod wire;

pub use store::{
    InMemoryIsrTokenStore, IsrConsumeOutcome, IsrTokenStore, IsrTokenStoreError, IssuedIsrToken,
};
pub use wire::{
    inst_resume_failed_element, inst_resumed_element, isr_enabled_element,
    isr_stream_feature_element, InstResume, IsrEnable,
};

/// XEP-0397 Instant Stream Resumption namespace. Re-exported from
/// [`crate::ns::ISR`] so callers do not need two import paths for the same
/// value — see that constant's doc comment for the `htpps`-typo fact-check.
pub const ISR_NS: &str = crate::ns::ISR;

/// The only SASL mechanism this implementation pins ISR tokens to
/// (deviation from XEP-0397's own `HT-SHA-256-ENDP` recommendation — this
/// codebase implements PLAIN/SCRAM-SHA-256/OAUTHBEARER, none of which is
/// `HT-SHA-256-ENDP`; PLAIN is the only one of the three whose wire shape
/// carries a bare password field the ISR token can stand in for).
pub const ISR_PINNED_MECHANISM: &str = "PLAIN";

/// Generate a cryptographically random ISR token with at least 128 bits of
/// entropy (XEP-0397's requirement is "MUST contain at least 128 bit of
/// entropy" — this generates 256 bits for margin), URL-safe base64 encoded.
/// `pub`: both [`InMemoryIsrTokenStore`] (this crate) and
/// `waddle-server::clustering::isr::PostgresIsrTokenStore` (downstream)
/// mint tokens with this same generator, so there is exactly one source of
/// token-entropy truth.
pub fn generate_isr_token() -> String {
    let bytes: [u8; 32] = rand::random();
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes)
}

#[cfg(test)]
mod tests;
