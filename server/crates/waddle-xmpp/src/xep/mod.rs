//! XMPP Extension Protocols (XEPs) Implementation
//!
//! This module contains implementations of various XMPP Extension Protocols
//! that extend the core XMPP functionality.
//!
//! ## Implemented XEPs
//!
//! - **XEP-0004**: Data Forms - Typed data form exchange for configuration,
//!   search, and reporting workflows.
//! - **XEP-0012**: Last Activity - Server uptime and user last activity queries.
//! - **XEP-0047**: In-Band Bytestreams - Base64-encoded data transfer over XMPP.
//! - **XEP-0059**: Result Set Management - Generic pagination for XMPP result
//!   sets via `<set>` elements with max, after, before, index, first, last, count.
//! - **XEP-0085**: Chat State Notifications - Typing indicators and
//!   conversational state (active, composing, paused, inactive, gone).
//! - **XEP-0092**: Software Version - Name and package version reported via
//!   `jabber:iq:version`, with the optional OS field omitted by default.
//! - **XEP-0184**: Message Delivery Receipts - The server routing layer
//!   transparently routes client-generated `<request/>` and `<received/>`
//!   payloads; this module does not generate receipts.
//! - **XEP-0106**: JID Escaping - Escape/unescape special characters
//!   in JID local parts using `\HH` sequences.
//! - **XEP-0172**: User Nickname - Display name via `<nick/>` element
//!   in messages and presence stanzas.
//! - **XEP-0202**: Entity Time - Server time query via IQ get/result
//!   with UTC timestamp and timezone offset.
//! - **XEP-0203**: Delayed Delivery - Timestamps on delayed/offline/history
//!   messages via `<delay/>` element with stamp and from attributes.
//! - **XEP-0292**: vCard4 Over XMPP - Modern user profiles via PEP with
//!   full name, nickname, email, photo, bio, and more.
//! - **XEP-0300**: Cryptographic Hash Functions - Standardized hash algorithm
//!   references with SHA-1/SHA-256/SHA-512 computation and verification.
//! - **XEP-0297**: Stanza Forwarding - Wraps forwarded stanzas in
//!   `<forwarded/>` with optional delay, used by carbons and MAM.
//! - **XEP-0308**: Last Message Correction - Replace previously sent messages
//!   via `<replace/>` element referencing the original message id.
//! - **XEP-0317**: Hats - Role badges (Admin, Moderator, Bot, Owner) for
//!   MUC occupants via `<hats/>` in presence, with well-known URIs.
//! - **XEP-0319**: Last User Interaction in Presence - Idle detection
//!   via `<idle since='...'/>` in presence stanzas.
//! - **XEP-0333**: Displayed Markers - `<markable/>` requests and
//!   `<displayed/>` updates for messages that have been shown.
//! - **XEP-0372**: References - Structured @mentions and data references
//!   via `<reference/>` elements with type, position, and URI.
//! - **XEP-0377**: Spam Reporting - Abuse/spam reports with reasons
//!   attached to blocking actions, with server-side report storage.
//! - **XEP-0392**: Consistent Color Generation - Deterministic HSL colors
//!   from input strings via SHA-1 hue mapping with CVD correction.
//! - **XEP-0393**: Message Styling - Inline text formatting parser for
//!   bold, italic, strikethrough, code, code blocks, and block quotes.
//! - **XEP-0394**: Message Markup - Semantic message formatting metadata,
//!   including block quotes via `<bquote/>`.
//! - **XEP-0359**: Unique and Stable Stanza IDs - Server-assigned `<stanza-id/>`
//!   and client-assigned `<origin-id/>` for stable message referencing.
//! - **XEP-0334**: Message Processing Hints
//! - **XEP-0410**: MUC Self-Ping - Detect room disconnection by pinging
//!   own occupant JID; error response triggers rejoin.
//! - **XEP-0401**: Ad-hoc Account Invitation - Platform invite tokens
//!   with expiry, redemption, and invite store for onboarding.
//! - **XEP-0421**: Occupant Identifiers - Stable opaque IDs for MUC
//!   occupants via HMAC-SHA-256, added by server to messages/presence.
//! - **XEP-0424**: Message Retraction - Retract previously sent messages
//!   with `<retract/>` and `<retracted/>` tombstone elements.
//! - **XEP-0425**: Moderated Message Retraction - Moderator message deletion
//!   via `<apply-to>/<moderate>` with fastening (XEP-0422).
//! - **XEP-0444**: Message Reactions
//! - **XEP-0433**: Extended Channel Search - Room discovery by keyword with
//!   IQ-based search/result and Searchable trait for matching.
//! - **XEP-0445**: Pre-Authenticated In-Band Registration - Invite token
//!   validation during account registration with preauth element.
//! - **XEP-0449**: Stickers - Sticker packs and sticker messages with
//!   pack references, built on XEP-0446/0447 file sharing.
//! - **XEP-0452**: MUC Mention Notifications - @mention alerts with
//!   notification elements and per-room unread mention counter.
//! - **Proto-calendar xCal**: Community events with scheduling, RSVP
//!   tracking (going/interested/not-going), and PubSub storage.
//! - **XEP-0470**: Pubsub Attachments - Reactions/comments on PubSub items
//!   with attachment target and typed payloads.
//! - **XEP-0472**: Pubsub Social Feed - Social posts and activity feeds
//!   via PubSub with title, body, author, and publication metadata.
//! - **XEP-0492**: Chat Notification Settings - Per-chat notification settings
//!   via `<notify/>` children (`<always/>`, `<on-mention/>`, `<never/>`).
//! - **XEP-0469**: Bookmark Pinning - Pin favorite channels via `<pinned/>`
//!   extension on XEP-0402 bookmark elements, with sort helper.
//! - **XEP-0501**: Pubsub Stories - Ephemeral posts with auto-expiry
//!   (default 24h), media support, and active/expired filtering.
//! - **XEP-0502**: MUC Activity Indicator - Lightweight room activity tracking
//!   with subscribe/notify pattern and per-room activity state.
//! - **XEP-0500**: MUC Slow Mode - Per-room rate limiting with configurable
//!   cooldown interval, moderator exemption, and per-occupant tracking.
//! - **XEP-0488**: MUC Token Invite - Shareable invite tokens for MUC rooms
//!   with URI generation and IQ-based token request/response.
//! - **XEP-0447**: Stateless File Sharing - Structured file sharing with
//!   metadata and download sources, building on XEP-0446 and XEP-0363.
//! - **XEP-0446**: File Metadata Element - Structured file info (name, size,
//!   type, dimensions) for file sharing messages. - Emoji reactions via `<reactions/>`
//!   element containing `<reaction>` children. - Control server-side processing
//!   with no-store, no-copy, no-permanent-store, and store hints.
//! - **XEP-0048**: Bookmark Storage (Legacy) - Compatibility layer over XEP-0402.
//! - **XEP-0050**: Ad-Hoc Commands - Multi-step command execution via data forms.
//! - **XEP-0049**: Private XML Storage - Arbitrary per-user XML key-value store.
//! - **XEP-0054**: vcard-temp - User profile information via vCard format.
//! - **XEP-0077**: In-Band Registration - Allows users to register accounts
//!   directly through the XMPP connection before authentication.
//! - **XEP-0084**: User Avatar - PEP-based avatar storage and notifications.
//! - **XEP-0115**: Entity Capabilities - Efficient service discovery caching
//!   via capability hashes included in presence stanzas.
//! - **XEP-0153**: vCard-Based Avatars - Avatar hash in presence stanzas.
//! - **XEP-0191**: Blocking Command - User blocking capability for managing
//!   blocklists and silently dropping messages from blocked JIDs.
//! - **XEP-0199**: XMPP Ping - Simple ping/pong for connection liveness.
//! - **XEP-0223**: Persistent Storage Best Practices - Profile of PubSub.
//! - **XEP-0249**: Direct MUC Invitations - Simple message-based invitations
//!   for inviting users directly to MUC rooms.
//! - **XEP-0272**: Multiparty Jingle (Muji) - MUC group call presence
//!   advertisement (`<muji>` with `<preparing/>` and `<content>` children).
//!   Joining the SFU is a Jingle session-initiate embedding the same
//!   element with a `room` attribute (XEP-0272 §Joining).
//! - **XEP-0363**: HTTP File Upload - Server-side support for HTTP-based
//!   file uploads, returning PUT and GET URLs for file transfer.
//! - **XEP-0402**: PEP Native Bookmarks - MUC room bookmarks stored via PEP.
//! - **XEP-0461**: Message Replies - Reply references and thread metadata.
//! - **XEP-0490**: Message Displayed Synchronization - multi-device "read up
//!   to here" sync via the `urn:xmpp:mds:displayed:0` PEP node.
//! - **XEP-0513**: Explicit Mentions - @everyone, @here, @role, @user
//!   mentions with notification decision logic.
//! - Private Waddle MUC thread metadata, used until official forum support is
//!   implemented through PubSub/XEP-0472 forum nodes.
//! - **XEP-0503**: Server-side Spaces - Community discovery via pubsub.

pub mod xep0004;
pub mod xep0012;
pub mod xep0047;
pub mod xep0048;
pub mod xep0049;
pub mod xep0050;
pub mod xep0054;
pub mod xep0059;
pub mod xep0077;
pub mod xep0084;
pub mod xep0085;
pub mod xep0092;
pub mod xep0106;
pub mod xep0107;
pub mod xep0108;
pub mod xep0115;
pub mod xep0118;
pub mod xep0153;
pub mod xep0166;
pub mod xep0167;
pub mod xep0172;
pub mod xep0191;
pub mod xep0199;
pub mod xep0202;
pub mod xep0203;
pub mod xep0215;
pub mod xep0223;
pub mod xep0249;
pub mod xep0272;
pub mod xep0292;
pub mod xep0297;
pub mod xep0300;
pub mod xep0308;
pub mod xep0317;
pub mod xep0319;
pub mod xep0333;
pub mod xep0334;
pub mod xep0353;
pub mod xep0357;
pub mod xep0363;
pub mod xep0372;
pub mod xep0377;
pub mod xep0393;
pub mod xep0394;
pub mod xep0401;
pub mod xep0402;
pub mod xep0410;
pub mod xep0421;
pub mod xep0424;
pub mod xep0425;
pub mod xep0428;
pub mod xep0430;
pub mod xep0431;
pub mod xep0433;
pub mod xep0437;
pub mod xep0444;
pub mod xep0445;
pub mod xep0446;
pub mod xep0447;
pub mod xep0448;
pub mod xep0449;
pub mod xep0452;
pub mod xep0461;
pub mod xep0469;
pub mod xep0470;
pub mod xep0486;
pub mod xep0488;
pub mod xep0490;
pub mod xep0492;
pub mod xep0500;
pub mod xep0502;
pub mod xep0503;
pub mod xep0511;
pub mod xep0513;
pub mod xep_waddle_call_thread;
pub mod xep_waddle_dm_bookmarks;
pub mod xep_waddle_dnd;
pub mod xep_waddle_forums;
pub mod xep_waddle_group_dm;
pub mod xep_waddle_in_call;
pub mod xep_waddle_link_preview;
pub mod xep_waddle_livekit_transport;
pub mod xep_waddle_pin;

mod exports;

#[cfg(test)]
mod xep_waddle_mam_stanza_id_tests;

pub use exports::*;

pub mod prelude {
    pub use super::xep0249::message_has_direct_invite;
}
