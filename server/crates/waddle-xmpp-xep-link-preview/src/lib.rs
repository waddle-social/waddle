//! Waddle Link Preview (Custom XEP)
//!
//! Server-side message enrichment: detects HTTP(S) URLs in XMPP message
//! bodies, fetches OpenGraph / Twitter Card / HTML metadata, and appends
//! a structured `<reference type='data'><preview/>` payload to the stanza
//! before fan-out. Every recipient — including carbons and MAM history —
//! sees the same enriched message.
//!
//! The enricher is designed to be called **once before fan-out**, after
//! the GitHub enricher (which wins for recognised GitHub URLs). Fail-open:
//! all errors are logged but never block delivery.
//!
//! ## XML wire format
//!
//! ```xml
//! <message type='groupchat'>
//!   <body>see https://example.com/article</body>
//!   <reference xmlns='urn:xmpp:reference:0' type='data'
//!              begin='4' end='30' uri='https://example.com/article'>
//!     <preview xmlns='urn:waddle:link-preview:0' url='https://example.com/article'>
//!       <title>Example Article</title>
//!       <description>Short summary.</description>
//!       <site-name>Example</site-name>
//!       <image src='https://example.com/og.png' width='1200' height='630'/>
//!       <type>article</type>
//!     </preview>
//!   </reference>
//! </message>
//! ```
//!
//! Senders can suppress enrichment per-message by including a top-level
//! `<no-preview xmlns='urn:waddle:link-preview:0'/>` hint.

pub mod cache;
pub mod circuit;
pub mod detect;
pub mod embed;
pub mod enrich;
pub mod fetch;
pub mod html;
pub mod rate;
pub mod ssrf;

pub use enrich::LinkPreviewEnricher;

/// XML namespace for the Waddle link preview extension.
pub const NS_WADDLE_PREVIEW: &str = "urn:waddle:link-preview:0";

/// XML namespace for the XEP-0372 reference wrapper.
pub const NS_REFERENCE: &str = "urn:xmpp:reference:0";

/// Maximum number of previews to attach per message.
pub const MAX_PREVIEWS_PER_MESSAGE: usize = 3;

/// Length caps for embedded text fields (plaintext, truncated on overflow).
pub const TITLE_MAX: usize = 200;
pub const DESCRIPTION_MAX: usize = 300;
pub const SITE_NAME_MAX: usize = 100;
pub const TYPE_MAX: usize = 50;

/// A sanitized, ready-to-embed link preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPreview {
    pub url: String,
    pub canonical_url: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub site_name: Option<String>,
    pub type_: Option<String>,
    pub image: Option<LinkPreviewImage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPreviewImage {
    pub src: String,
    pub width: Option<String>,
    pub height: Option<String>,
}
