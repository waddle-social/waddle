//! Waddle GitHub Link Metadata (Custom XEP)
//!
//! Server-side extension that detects GitHub URLs in XMPP message bodies and
//! enriches messages with structured metadata elements. Clients can render
//! these as rich cards while non-Waddle clients still see the plain URL in `<body>`.
//!
//! ## Supported link types
//!
//! - **Repository** — `github.com/{owner}/{repo}`
//! - **Issue** — `github.com/{owner}/{repo}/issues/{number}`
//! - **Pull Request** — `github.com/{owner}/{repo}/pull/{number}`
//!
//! ## XML namespace
//!
//! All elements use `urn:waddle:github:0`. Example:
//!
//! ```xml
//! <message type='chat' to='bob@waddle.social'>
//!   <body>Check out https://github.com/rust-lang/rust</body>
//!   <repo xmlns='urn:waddle:github:0'
//!         url='https://github.com/rust-lang/rust'
//!         owner='rust-lang'
//!         name='rust'>
//!     <description>The Rust programming language</description>
//!     <language name='Rust' bytes='123456789'/>
//!     <language name='Python' bytes='456789'/>
//!     <stars>100000</stars>
//!     <forks>12000</forks>
//!     <default-branch>master</default-branch>
//!     <topic>rust</topic>
//!     <topic>programming-language</topic>
//!     <license>MIT</license>
//!   </repo>
//! </message>
//! ```

pub mod client;
pub mod detect;
pub mod embed;
pub mod enrich;

pub use client::GitHubClient;
pub use detect::GitHubLink;
pub use embed::*;
pub use enrich::MessageEnricher;

/// Namespace for the Waddle GitHub embed extension.
pub const NS_WADDLE_GITHUB: &str = "urn:waddle:github:0";

/// Maximum number of GitHub links to expand per message.
pub const MAX_LINKS_PER_MESSAGE: usize = 3;
