//! Client state shared by every Waddle client (ADR-0019).
//!
//! Stores here are sans-IO: they take plain inputs (what arrived, what the
//! user did, what time it is) and return the list diffs a UI applies. They
//! never touch a socket, a clock or a runtime, so the same code runs under
//! tokio on Linux, behind UniFFI on Android and Apple, and in wasm.
//!
//! This first slice holds the conversation timeline. The connection
//! controller, outbox, presence and read state follow ADR-0019's order.

pub mod timeline;

pub use timeline::{
    strip_fallback, with_reply_fallback, Attachment, Delivery, Encryption, IncomingMessage,
    ItemKey, LocalEcho, Reaction, ReactionUpdate, ReplyPreview, ReplyTo, Timeline, TimelineDiff,
    TimelineItem, GROUPING_WINDOW_MS,
};
