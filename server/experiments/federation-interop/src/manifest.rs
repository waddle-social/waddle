//! The static manifest: every row of the verification phase's flow table
//! and negative matrix (docs/planning/federation-2047.md), the W10
//! observations and the step 6 and step 8 cases, with the peers each runs
//! against, the slices it needs and whether the harness drives it.
//!
//! A row whose slices have not landed is UNVERIFIED with the slice named.
//! [`LANDED`] is the one table that says which slices are on the tree the
//! harness runs against; `INTEROP_LANDED` adds slices at run time (for
//! example `INTEROP_LANDED=W6a,W6c`) so a run on a later tree needs no
//! code change.

use crate::evidence::Section;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Peer {
    Waddle,
    Prosody,
    Ejabberd,
}

impl Peer {
    pub fn label(self) -> &'static str {
        match self {
            Self::Waddle => "waddle",
            Self::Prosody => "prosody",
            Self::Ejabberd => "ejabberd",
        }
    }
}

pub const ALL: &[Peer] = &[Peer::Waddle, Peer::Prosody, Peer::Ejabberd];
pub const WADDLE: &[Peer] = &[Peer::Waddle];
pub const PROSODY_ONLY: &[Peer] = &[Peer::Prosody];
pub const EJABBERD_ONLY: &[Peer] = &[Peer::Ejabberd];

/// Implementation slices of the plan that flows depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Slice {
    W5d,
    W5e,
    W5r,
    W6a,
    W6b,
    W6c,
    W7a,
    W7b,
    W8,
}

impl Slice {
    pub fn name(self) -> &'static str {
        match self {
            Self::W5d => "W5d",
            Self::W5e => "W5e",
            Self::W5r => "W5r",
            Self::W6a => "W6a",
            Self::W6b => "W6b",
            Self::W6c => "W6c",
            Self::W7a => "W7a",
            Self::W7b => "W7b",
            Self::W8 => "W8",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::W5d => "revocation",
            Self::W5e => "effect-boundary admission for remote principals",
            Self::W5r => "restore incarnation",
            Self::W6a => "inbound MUC",
            Self::W6b => "outbound MUC",
            Self::W6c => "account traffic across S2S",
            Self::W7a => "Spaces over S2S",
            Self::W7b => "MUC fallback for Spaces",
            Self::W8 => "web pilot",
        }
    }
}

/// Whether each slice is on the tree the harness runs against. On
/// integration/2047 every slice the rows depend on is merged (W5d, W5e,
/// W5r, W6a, W6b, W6c, W7a, W7b and W8, on top of W0–W4d, W5a–W5c, W5i and
/// W5o), so no row is held back for a missing slice; `INTEROP_LANDED` can
/// still add slices to a table that lists some as missing.
pub const LANDED: &[(Slice, bool)] = &[
    (Slice::W5d, true),
    (Slice::W5e, true),
    (Slice::W5r, true),
    (Slice::W6a, true),
    (Slice::W6b, true),
    (Slice::W6c, true),
    (Slice::W7a, true),
    (Slice::W7b, true),
    (Slice::W8, true),
];

pub fn landed(slice: Slice) -> bool {
    let table = LANDED
        .iter()
        .any(|(candidate, landed)| *candidate == slice && *landed);
    let added = std::env::var("INTEROP_LANDED").is_ok_and(|value| {
        value
            .split(',')
            .any(|name| name.trim().eq_ignore_ascii_case(slice.name()))
    });
    table || added
}

/// How a row is (or is not) driven.
#[derive(Debug, Clone, Copy)]
pub enum Drive {
    /// The harness has a driver for it.
    Driven,
    /// The harness cannot drive it; the reason names the evidence that
    /// covers it instead.
    NotDriven(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub struct Row {
    pub id: &'static str,
    pub section: Section,
    pub title: &'static str,
    pub peers: &'static [Peer],
    pub g20: bool,
    /// Slices every column needs.
    pub requires: &'static [Slice],
    /// Further slices the Waddle ↔ Waddle column needs, where the remote
    /// side is also Waddle.
    pub waddle_also_requires: &'static [Slice],
    pub drive: Drive,
}

const fn row(
    id: &'static str,
    section: Section,
    title: &'static str,
    peers: &'static [Peer],
) -> Row {
    Row {
        id,
        section,
        title,
        peers,
        g20: false,
        requires: &[],
        waddle_also_requires: &[],
        drive: Drive::Driven,
    }
}

const fn needs(mut row: Row, requires: &'static [Slice]) -> Row {
    row.requires = requires;
    row
}

const fn waddle_needs(mut row: Row, requires: &'static [Slice]) -> Row {
    row.waddle_also_requires = requires;
    row
}

const fn g20(mut row: Row) -> Row {
    row.g20 = true;
    row
}

const fn not_driven(mut row: Row, reason: &'static str) -> Row {
    row.drive = Drive::NotDriven(reason);
    row
}

use Section::{Flow, Negative, Observation, Restart, S2sDisabled};

pub const ROWS: &[Row] = &[
    // Step 6, first part.
    row("s2s-off.no-listener-dial-read-advertisement", S2sDisabled,
        "S2S disabled: no s2s listener, no dial, no configuration or credential read, no advertisement", WADDLE),
    // The flow table.
    row("flow.s2s-tls-external", Flow,
        "S2S with TLS and SASL EXTERNAL at each TLS version the G1 profile allows, both directions", ALL),
    row("flow.dialback", Flow,
        "Dialback under D2: inbound only for a listed domain; outbound Initiating and Authoritative roles", ALL),
    needs(row("flow.presence", Flow,
        "Presence subscription and presence between accounts, both directions", ALL), &[Slice::W6c]),
    needs(row("flow.direct-messages", Flow,
        "Direct messages between accounts with Carbons on the Waddle side, both directions", ALL), &[Slice::W6c]),
    waddle_needs(needs(row("flow.local-joins-remote-muc", Flow,
        "Local user joins remote MUC, sends, reads host-authorized history with paging", ALL), &[Slice::W6b]), &[Slice::W6a]),
    waddle_needs(needs(row("flow.remote-joins-waddle-muc", Flow,
        "Remote user joins Waddle MUC, sends, reads host-authorized history with paging", ALL), &[Slice::W6a]), &[Slice::W6b]),
    waddle_needs(needs(row("flow.two-resources-remote-room", Flow,
        "Account with two resources in a remote room: self-presence, room traffic and own messages on both", ALL), &[Slice::W6b]), &[Slice::W6a]),
    needs(row("flow.bookmarks-restore", Flow,
        "Fresh device restores remote rooms and Spaces from XEP-0402 bookmarks and joined-space preferences", ALL),
        &[Slice::W6b, Slice::W7a, Slice::W8]),
    waddle_needs(needs(row("flow.remote-moderator-trusted-home", Flow,
        "Room moderation by a remote moderator of a trusted home over a pkix stream", ALL), &[Slice::W6a, Slice::W5e]), &[Slice::W6b]),
    needs(row("flow.community-admin-trusted-home", Flow,
        "Administration of a community by an account of a trusted Waddle home over a pkix stream", WADDLE),
        &[Slice::W6a, Slice::W6b, Slice::W7a, Slice::W5e]),
    row("flow.disco-ping", Flow, "Disco#info, disco#items and ping across S2S, both directions", ALL),
    needs(row("flow.space-on-peer-pubsub", Flow,
        "Waddle client uses a Space hosted on the peer's PubSub service (stock PubSub for third parties)", ALL),
        &[Slice::W6c, Slice::W7a]),
    needs(row("flow.peer-reads-waddle-space", Flow,
        "Peer account reads a Waddle Space with plain XEP-0060", ALL), &[Slice::W7a]),
    waddle_needs(needs(row("flow.child-rooms-by-muc-jid", Flow,
        "Every child room joins by plain MUC JID", ALL), &[Slice::W6a, Slice::W7b]), &[Slice::W6b]),
    needs(row("flow.no-avatar-banner-fetch", Flow,
        "A Space whose avatar and banner items point at a harness URL causes no fetch (D4)", WADDLE), &[Slice::W7a]),
    // W10 observations.
    row("obs.pre-tls-from", Observation, "Whether the peer sends a pre-TLS `from` in its stream header", ALL),
    row("obs.whitespace-before-header", Observation,
        "Whether the peer sends whitespace before a stream header (which Waddle refuses)", ALL),
    row("obs.ejabberd-starttls", Observation, "ejabberd's effective STARTTLS setting as offered on the wire", EJABBERD_ONLY),
    row("obs.mixed-case-to", Observation,
        "A stanza from the peer whose `to` domain is in mixed case is accepted, the domain mapped to lowercase", ALL),
    row("obs.ejabberd-cluster-streams", Observation,
        "Clustered ejabberd with max_s2s_connections above one against Waddle's several inbound streams per directional pair", EJABBERD_ONLY),
    // Negative matrix: certificates and trust.
    g20(row("neg.cert.wrong-name", Negative, "A certificate for another name is refused", ALL)),
    g20(row("neg.cert.unknown-issuer", Negative, "A certificate from an untrusted CA is refused", ALL)),
    g20(row("neg.cert.expired", Negative, "An expired certificate is refused", ALL)),
    g20(row("neg.cert.parent-domain-for-subdomain", Negative,
        "A parent-domain certificate presented for a subdomain is refused", ALL)),
    row("neg.cert.server-auth-only-both-roles", Negative,
        "serverAuth-only certificates authenticate in both roles", WADDLE),
    not_driven(row("neg.cert.public-ca-for-private-profile", Negative,
        "A public-CA certificate for a domain with a private-CA profile fails", WADDLE),
        "needs a publicly trusted certificate, which an offline loopback harness cannot obtain; covered by the s2s_verifier inventory cases"),
    row("neg.cert.private-ca-for-public-profile", Negative,
        "A private-CA certificate for a public-profile domain fails", WADDLE),
    row("neg.cert.tls-version-outside-profile", Negative, "A TLS version outside the G1 profile is refused", WADDLE),
    row("neg.cert.no-pre-tls-from", Negative, "An inbound stream without a pre-TLS `from` is refused", WADDLE),
    row("neg.cert.inbound-direct-tls", Negative, "An inbound direct-TLS connection attempt is refused", WADDLE),
    // Dialback.
    row("neg.dialback.unlisted-domain", Negative, "Dialback from a domain not on the dialback list is refused", WADDLE),
    row("neg.dialback.after-certificate-failure", Negative, "Dialback after an active certificate failure is refused", WADDLE),
    row("neg.dialback.unsolicited", Negative, "Unsolicited or uncorrelated db:result and db:verify are refused", WADDLE),
    row("neg.dialback.extra-domains", Negative,
        "Extra sender or target domains on an EXTERNAL-authenticated or dialback stream are refused", WADDLE),
    row("neg.dialback.verify-over-plaintext", Negative,
        "A peer verifying Waddle's dialback key over a non-TLS connection is refused", WADDLE),
    // Discovery.
    not_driven(row("neg.discovery.stub-resolver", Negative,
        "Blocked address classes, changing answers, SERVFAIL and timeout, \".\" targets, caps, SRV-target-only certificates", WADDLE),
        "the harness reaches peers through configured endpoints by design; covered by crates/waddle-server/tests/xep0368_s2s_discovery.rs (step 2)"),
    // Addressing.
    row("neg.addr.header-refused-forms", Negative,
        "A remote domain in each refused form in an inbound stream header is refused", WADDLE),
    row("neg.addr.outbound-destination", Negative,
        "A remote domain in each refused form as an outbound destination is refused without dialling", WADDLE),
    row("neg.addr.stanza-address", Negative,
        "A remote domain in each refused form in a stanza address is refused", WADDLE),
    row("neg.addr.see-other-host", Negative,
        "A see-other-host stream error before and after TLS is not followed", WADDLE),
    // Routing.
    g20(row("neg.route.spoofed-from", Negative, "A stanza with a spoofed `from` is refused", ALL)),
    g20(row("neg.route.forged-iq-result", Negative, "A forged IQ result reaches no local resource", ALL)),
    g20(row("neg.route.replayed-iq-result", Negative, "A replayed IQ result is delivered at most once", ALL)),
    not_driven(row("neg.route.iq-reply-other-replica", Negative,
        "An IQ reply arriving on a different replica from the request", WADDLE),
        "needs two replicas of one deployment; covered by crates/waddle-server/tests/s2s_iq_correlation.rs"),
    g20(row("neg.route.relay-unserved-domain", Negative, "Relay through an unserved domain is refused", ALL)),
    row("neg.route.no-error-to-error", Negative, "No error is sent in reply to an error stanza", WADDLE),
    row("neg.route.account-handler-refused", Negative,
        "Remote traffic to a local-account handler outside W6c's reviewed set is refused as if the account did not exist", ALL),
    // Local services (D10).
    row("neg.d10.server-domain-services", Negative,
        "Remote requests outside the enabled set receive service-unavailable; presence to them is ignored", WADDLE),
    // Admission.
    row("neg.admit.denylisted-with-open-stream", Negative,
        "A denylisted domain with an open stream and queued work is refused after the snapshot changes", WADDLE),
    row("neg.admit.denylisted-subdomain", Negative, "A subdomain of a denylisted domain is refused", WADDLE),
    row("neg.admit.denylisted-spellings", Negative,
        "A denylisted domain with a trailing dot, in its other label form and in different case is refused", WADDLE),
    row("neg.admit.denylisted-each-mode", Negative, "A denylisted domain is refused in each admission mode", WADDLE),
    row("neg.admit.denylisted-listed-fails-to-load", Negative,
        "A configuration listing a denylisted domain, or a subdomain of one, as allowed, dialback or trusted fails to load", WADDLE),
    row("neg.admit.listed-subdomain-not-covered", Negative,
        "A subdomain of an allowlisted, dialback-listed or trusted-home domain is not covered", WADDLE),
    row("neg.admit.removal-and-closed-mode", Negative,
        "Allowlist removal and a switch to closed with open streams and queued work", WADDLE),
    row("neg.admit.open-mode-slot-exhaustion", Negative,
        "Many peers exhausting slots in open mode: global caps, eviction and backoff hold", WADDLE),
    // Authority.
    needs(row("neg.authority.privileged-pkix-vs-dialback", Negative,
        "A trusted home's privileged grant is honored on pkix and refused on a dialback-only stream", WADDLE),
        &[Slice::W5e, Slice::W6a]),
    needs(row("neg.authority.untrusted-home", Negative,
        "Privileged issuance for, and moderation by, a JID of a home not on the trusted-home list are refused", ALL),
        &[Slice::W5e, Slice::W6a]),
    needs(row("neg.authority.home-removed-from-trusted", Negative,
        "Removing a trusted home suspends moderation and archive access through its stored admin grant; restoring trust re-enables both without reissuing the grant", WADDLE),
        &[Slice::W5e, Slice::W6a]),
    not_driven(needs(row("neg.authority.moderation-dialback-only", Negative,
        "Room moderation by a remote moderator of a trusted home is refused on a dialback-only stream", WADDLE),
        &[Slice::W5e, Slice::W6a]),
        "no driver yet: needs the harness raw peer authenticated by dialback for a trusted home's domain sending a MUC moderation request, with a pkix control on the same room; to be written when W5e and W6a land"),
    not_driven(needs(row("neg.authority.grant-outside-canonical-path", Negative,
        "A privileged grant for a JID of an untrusted home, stored outside the canonical path, is ignored", WADDLE),
        &[Slice::W5e, Slice::W6a]),
        "no driver yet: needs a grant written to SpiceDB directly, bypassing the PostgreSQL canonical path, then a moderation attempt by that JID; to be written when W5e and W6a land"),
    needs(row("neg.authority.own-edit-delete", Negative,
        "A message author edits and deletes their own message regardless of moderation rules", ALL),
        &[Slice::W6a]),
    not_driven(needs(row("neg.authority.replica-races-and-repair", Negative,
        "Grant and revoke races across two replicas; stale repair; scoped administration; outcast precedence", WADDLE),
        &[Slice::W5d]),
        "needs two replicas of one deployment and direct grant-store manipulation; covered by the W5b–W5d PostgreSQL sets and the grant-fence experiment (step 4)"),
    needs(row("neg.authority.private-discovery-and-mam", Negative,
        "Denied private discovery and MAM for a remote occupant", ALL), &[Slice::W6a]),
    // Identity.
    waddle_needs(needs(row("neg.identity.same-room-names", Negative,
        "Identically named rooms on both sides stay distinct", WADDLE), &[Slice::W6a]), &[Slice::W6b]),
    not_driven(needs(row("neg.identity.collisions-and-reassignment", Negative,
        "Identity collisions, malicious localparts, colliding Unicode IDs and reassigned JIDs", WADDLE),
        &[Slice::W5d, Slice::W6c]),
        "covered by the W5i identity-profile suites and W5d's reuse fence; the harness cannot provision colliding remote accounts on stock servers"),
    // Resources.
    g20(row("neg.res.oversized-stanza", Negative, "An oversized stanza is refused", ALL)),
    g20(row("neg.res.deep-nesting", Negative, "A deeply nested stanza is refused", ALL)),
    row("neg.res.preauth-large-element", Negative,
        "An unauthenticated peer's large first-level element is refused when its head closes", WADDLE),
    g20(row("neg.res.peer-queue-exhaustion", Negative,
        "Per-peer queue exhaustion is bounded and leaves local traffic alone", ALL)),
    // Step 8.
    row("restart.each-side", Restart, "Each side restarted while the other stays up; S2S recovers", ALL),
    row("restart.partition", Restart,
        "Partition: a remote outage is distinct from denial, local traffic keeps flowing, the path recovers", ALL),
    row("restart.crash", Restart, "A deployment killed without shutdown recovers its S2S paths", WADDLE),
];
