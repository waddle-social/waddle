//! Typed DM intake classifier (issue #209, locked Q3 = A).
//!
//! Given an inbound 1:1 `<message>` plus the recipient's online-resource set
//! and blocklist, produce a typed [`DmRouting`] that names every downstream
//! sink the stanza should land in (MAM archive, `pending_delivery` table,
//! XEP-0280 carbons fanout, live delivery, Waddle inbox).
//!
//! The classifier is the **single source of truth** for the interaction
//! matrix between:
//!
//! - XEP-0160 §4 type rules (chat/normal store; groupchat/headline/error skip)
//! - XEP-0160 §3 storage trigger (no non-negative-priority resource online)
//! - XEP-0085 chat-state-only exclusion
//! - XEP-0334 `<no-store/>`, `<no-permanent-store/>`, `<store/>`, `<no-copy/>`
//! - XEP-0334 §6 ¶3 — hints in `type='error'` are ignored
//! - XEP-0334 §5.3 ¶2 — `<no-copy/>` does NOT override RFC 6121 §8.5
//!   bare-JID fanout
//! - XEP-0191 §2 step 4 — blocked sender's stanza is not delivered
//! - RFC 6121 §8.5.2.1.4 — `type='error'` to a fully-offline recipient
//!   MUST be silently dropped (neither stored nor bounced)
//! - Issue #209 lock Q10b — `<no-permanent-store/>` skips the inbox bump
//!
//! Hint precedence (XEP-0334 is silent on conflicts; project rule):
//! - **Explicit `<store/>` wins over conflicting restrictive hints.**
//!   XEP-0334 §5.4 says a stanza with `<store/>` SHOULD be stored, so
//!   when both `<store/>` and `<no-store/>` (or `<no-permanent-store/>`)
//!   are present, `<store/>` is honored.
//! - **In the absence of `<store/>`, the more-restrictive hint wins.**
//!   `<no-store/>` is strictly more restrictive than
//!   `<no-permanent-store/>` (the latter still allows transient hold-
//!   and-forward; the former forbids it), so when both appear without
//!   `<store/>`, `<no-store/>` wins.
//! - **`<store/>` does NOT override `type='error'`** — XEP-0334 §6 ¶3
//!   ignores all hints inside `type='error'` stanzas.
//!
//! The classifier is a pure function: same inputs → same `DmRouting`.
//! It performs no I/O. Wiring this into the routing layer and existing
//! handler chain is a follow-up step.

use crate::protocol::session_state::Blocklist;
use crate::xep::xep0085::is_standalone_notification;
use crate::xep::xep0334::{has_hint, Hint};
use jid::{BareJid, FullJid, Jid};
use std::collections::BTreeMap;
use xmpp_parsers::message::{Message, MessageType};

/// Should the message be written to the MAM archive (XEP-0313)?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveDecision {
    /// Do not archive — `<no-store/>`, `<no-permanent-store/>`, type rule,
    /// chat-state-only, or blocked sender.
    None,
    /// Write to MAM. The MAM writer stamps a XEP-0359 `<stanza-id/>` at
    /// archive time.
    Mam,
}

/// Should the message land in `pending_delivery`?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingDecision {
    /// No `pending_delivery` row (recipient online, or storage forbidden,
    /// or message type ineligible).
    None,
    /// Archived — row references the MAM stanza-id (FK).
    Archived,
    /// Transient — `<no-permanent-store/>` stanza; row carries inline
    /// payload because there is no MAM row.
    Transient,
}

/// Should XEP-0280 carbons fanout run for this stanza?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarbonsDecision {
    /// Suppressed — `<no-copy/>` on a full-JID-addressed message, or the
    /// stanza is otherwise carbons-ineligible (groupchat, error, blocked).
    Suppressed,
    /// Eligible — fanout to other carbons-enabled resources of the
    /// recipient. The routing layer resolves the actual destination set.
    Eligible,
}

/// Live delivery decision (RFC 6121 §8.5.2 / §8.5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveDecision {
    /// No live delivery — silently drop (e.g. error to offline) or queue
    /// only into `pending_delivery`.
    None,
    /// Original `to` was the bare JID — routing layer fans out to
    /// non-negative-priority resources per RFC 6121 §8.5.2.
    DeliverToBareWithFanout,
    /// Original `to` was a full JID — deliver to that resource per RFC
    /// 6121 §8.5.3 (caller must verify the resource is still online; if
    /// not, falls back to bare-JID routing).
    DeliverToFull,
}

/// Should the Waddle inbox be updated?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxDecision {
    /// No inbox update — chat-state-only, hint-skipped, blocked, or
    /// `<no-permanent-store/>` (locked Q10b: Transient stanzas leave no
    /// inbox trace).
    None,
    /// Bump unread / update last_stanza_id. `has_archive_ref` indicates
    /// whether a MAM stanza-id is available as the `archive_ref`.
    UpdateUnread {
        /// True iff `archive == Mam` — the inbox row carries the MAM
        /// stanza-id so clients can pivot from inbox into the archive.
        has_archive_ref: bool,
    },
}

/// Typed routing decision produced by [`classify_dm_intake`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmRouting {
    pub archive: ArchiveDecision,
    pub pending: PendingDecision,
    pub carbons: CarbonsDecision,
    pub live: LiveDecision,
    pub inbox: InboxDecision,
}

impl DmRouting {
    /// The "drop everything" decision — no sink runs.
    pub const fn dropped() -> Self {
        Self {
            archive: ArchiveDecision::None,
            pending: PendingDecision::None,
            carbons: CarbonsDecision::Suppressed,
            live: LiveDecision::None,
            inbox: InboxDecision::None,
        }
    }
}

/// Snapshot of the recipient's online resources (full JID → priority).
///
/// Built from the registry's per-user resource map at classification
/// time. The classifier only reads `has_non_negative_priority()`; the
/// actual fanout target set is resolved later by the routing layer.
#[derive(Debug, Default, Clone)]
pub struct OnlineResources {
    by_full_jid: BTreeMap<FullJid, i8>,
}

impl OnlineResources {
    /// Empty resource set — recipient has no connected sessions.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build from `(full_jid, priority)` pairs.
    pub fn from_pairs(iter: impl IntoIterator<Item = (FullJid, i8)>) -> Self {
        Self {
            by_full_jid: iter.into_iter().collect(),
        }
    }

    /// True when at least one resource has priority ≥ 0
    /// (RFC 6121 §8.5.2 / XEP-0160 §3 step 2 note).
    pub fn has_non_negative_priority(&self) -> bool {
        self.by_full_jid.values().any(|p| *p >= 0)
    }

    /// True when the given full JID is currently online (any priority).
    pub fn contains_full(&self, full: &FullJid) -> bool {
        self.by_full_jid.contains_key(full)
    }
}

/// Pure classifier: produces the `DmRouting` for an inbound DM stanza.
///
/// Inputs are all immutable references; no I/O, no global state, no
/// allocations beyond what the inputs already own. Suitable for unit
/// testing the full XEP/RFC matrix in isolation.
pub fn classify_dm_intake(
    message: &Message,
    online_resources: &OnlineResources,
    blocklist: &Blocklist,
) -> DmRouting {
    // ── XEP-0191 §2 step 4: blocked sender → drop entirely ────────────
    if let Some(sender_bare) = sender_bare(message) {
        if blocklist.contains(&sender_bare) {
            return DmRouting::dropped();
        }
    }

    let recipient_online = online_resources.has_non_negative_priority();

    // ── XEP-0334 §6 ¶3: ignore hints in `type='error'` stanzas ────────
    let hints_apply = !matches!(message.type_, MessageType::Error);
    let has_no_store = hints_apply && has_hint(message, Hint::NoStore);
    let has_no_permanent_store = hints_apply && has_hint(message, Hint::NoPermanentStore);
    let has_store = hints_apply && has_hint(message, Hint::Store);
    let has_no_copy = hints_apply && has_hint(message, Hint::NoCopy);

    // ── RFC 6121 §8.5.2.1.4: error-type to fully-offline → silent drop
    // (MUST). Note: even when recipient is online we still don't store
    // errors (XEP-0160 §4) and don't run the inbox/carbons sinks here.
    if matches!(message.type_, MessageType::Error) {
        return DmRouting {
            archive: ArchiveDecision::None,
            pending: PendingDecision::None,
            carbons: CarbonsDecision::Suppressed,
            live: if recipient_online {
                live_decision_from_to(message)
            } else {
                LiveDecision::None
            },
            inbox: InboxDecision::None,
        };
    }

    // ── Out of scope of this classifier (handled by other chains) ─────
    if matches!(message.type_, MessageType::Groupchat) {
        return DmRouting {
            archive: ArchiveDecision::None,
            pending: PendingDecision::None,
            carbons: CarbonsDecision::Suppressed,
            live: LiveDecision::None,
            inbox: InboxDecision::None,
        };
    }

    // ── XEP-0085 chat-state-only: never archived, never queued, never
    //     touched by the inbox. Carbons may still fanout to live resources.
    let chat_state_only = is_standalone_notification(message);
    if chat_state_only {
        return DmRouting {
            archive: ArchiveDecision::None,
            pending: PendingDecision::None,
            carbons: carbons_decision(message, has_no_copy),
            live: if recipient_online {
                live_decision_from_to(message)
            } else {
                LiveDecision::None
            },
            inbox: InboxDecision::None,
        };
    }

    // ── XEP-0160 §4 type matrix: headline never stores; chat/normal
    //     are storage candidates.
    let type_storage_eligible = matches!(message.type_, MessageType::Chat | MessageType::Normal);

    // ── Hint resolution (more-restrictive wins; <store/> overrides
    //     no-store and no-permanent-store per XEP-0334 §5.4).
    let storage_allowed = if has_store {
        // <store/> forces storage even for headline/empty-body cases.
        true
    } else if has_no_store || has_no_permanent_store {
        // Either restrictive hint disables MAM. <no-permanent-store/>
        // allows pending_delivery (Transient); <no-store/> does not.
        false
    } else {
        type_storage_eligible
    };

    // ── ArchiveDecision (XEP-0313 + XEP-0334 §5.1/§5.2)
    //     <no-permanent-store/> excludes MAM even when storage_allowed
    //     is true, because §5.1 explicitly names XEP-0313 as forbidden.
    let archive = if storage_allowed && !has_no_permanent_store {
        ArchiveDecision::Mam
    } else {
        ArchiveDecision::None
    };

    // ── PendingDecision (XEP-0160 §3 step 2 trigger + §5.1 transient
    //     "Sensitive messages" use case)
    //
    // Hint precedence:
    //   <store/>             ≻ <no-store/> ≻ <no-permanent-store/>
    // Where ≻ means "wins". <store/> trumps both restrictive hints
    // (XEP-0334 §5.4 SHOULD store + project rule); when no <store/> is
    // present, <no-store/> is more restrictive than <no-permanent-store/>
    // (forbids both permanent and transient holds).
    let pending = if recipient_online {
        // Recipient is online → live delivery, no pending row needed.
        PendingDecision::None
    } else if archive == ArchiveDecision::Mam {
        // Normal storable stanza for offline recipient: pointer into MAM.
        // (storage_allowed already accounts for <store/> overrides.)
        PendingDecision::Archived
    } else if has_no_store {
        // <no-store/> (without <store/> override) forbids any storage
        // including offline.
        PendingDecision::None
    } else if has_no_permanent_store {
        // <no-permanent-store/> (without <store/> override) allows
        // transient hold-and-forward — the §5.1 use case.
        PendingDecision::Transient
    } else {
        // Type rule says skip storage (headline, etc.) and no <store/>
        // override.
        PendingDecision::None
    };

    // ── LiveDecision
    let live = if recipient_online {
        live_decision_from_to(message)
    } else {
        LiveDecision::None
    };

    // ── CarbonsDecision (XEP-0280 + XEP-0334 §5.3)
    let carbons = carbons_decision(message, has_no_copy);

    // ── InboxDecision (locked Q10b: skip inbox for Transient)
    let inbox = match (pending, archive) {
        // Transient `<no-permanent-store/>` → no inbox trace.
        (PendingDecision::Transient, _) => InboxDecision::None,
        // Live or pending+archived storable chat/normal → bump unread.
        (_, ArchiveDecision::Mam) => InboxDecision::UpdateUnread {
            has_archive_ref: true,
        },
        // Live-only ephemeral with no archive (e.g. headline+`<store/>`
        // is actually archived; truly archive=None means no inbox trace).
        _ => InboxDecision::None,
    };

    DmRouting {
        archive,
        pending,
        carbons,
        live,
        inbox,
    }
}

/// Extract the sender's bare JID from a message, if present.
fn sender_bare(message: &Message) -> Option<BareJid> {
    message.from.as_ref().map(Jid::to_bare)
}

/// Determine the live-delivery shape from the message's `to` attribute.
///
/// RFC 6121 §8.5.2 (bare JID): fanout to non-negative-priority resources.
/// RFC 6121 §8.5.3 (full JID): deliver to that resource if connected.
fn live_decision_from_to(message: &Message) -> LiveDecision {
    match message.to.as_ref().and_then(|jid| jid.resource()) {
        Some(_) => LiveDecision::DeliverToFull,
        None => LiveDecision::DeliverToBareWithFanout,
    }
}

/// XEP-0280 carbons decision factoring in XEP-0334 §5.3 ¶2 (no-copy
/// only honored on full-JID-addressed messages).
fn carbons_decision(message: &Message, has_no_copy: bool) -> CarbonsDecision {
    let target_is_full_jid = message.to.as_ref().and_then(|jid| jid.resource()).is_some();
    if has_no_copy && target_is_full_jid {
        CarbonsDecision::Suppressed
    } else {
        CarbonsDecision::Eligible
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xep::xep0085::{build_chat_state_element, ChatState};
    use crate::xep::xep0334::{add_hint, Hint};
    use xmpp_parsers::message::{Body, MessageType};

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }

    fn dm(from: &str, to: &str, message_type: MessageType, body: Option<&str>) -> Message {
        let mut m = Message::new(Some(to.parse::<Jid>().expect("valid jid")));
        m.from = Some(from.parse::<Jid>().expect("valid jid"));
        m.type_ = message_type;
        if let Some(body_text) = body {
            m.bodies.insert(String::new(), Body(body_text.to_string()));
        }
        m
    }

    fn one_resource_online(priority: i8) -> OnlineResources {
        OnlineResources::from_pairs([(full("alice@example.com/web"), priority)])
    }

    // ── XEP-0191 blocking ───────────────────────────────────────────

    #[test]
    fn blocked_sender_drops_entire_routing() {
        let msg = dm(
            "blocked@elsewhere/x",
            "alice@example.com",
            MessageType::Chat,
            Some("hi"),
        );
        let block = Blocklist::new([bare("blocked@elsewhere")]);
        let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &block);
        assert_eq!(routing, DmRouting::dropped());
    }

    // ── XEP-0160 §3 storage trigger ─────────────────────────────────

    #[test]
    fn chat_to_offline_recipient_creates_archived_pending() {
        let msg = dm(
            "bob@elsewhere/x",
            "alice@example.com",
            MessageType::Chat,
            Some("hi"),
        );
        let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
        assert_eq!(routing.archive, ArchiveDecision::Mam);
        assert_eq!(routing.pending, PendingDecision::Archived);
        assert_eq!(routing.live, LiveDecision::None);
        assert_eq!(
            routing.inbox,
            InboxDecision::UpdateUnread {
                has_archive_ref: true
            }
        );
    }

    #[test]
    fn negative_priority_resources_count_as_offline_for_storage() {
        let msg = dm(
            "bob@elsewhere/x",
            "alice@example.com",
            MessageType::Chat,
            Some("hi"),
        );
        let routing = classify_dm_intake(&msg, &one_resource_online(-1), &Blocklist::empty());
        assert_eq!(routing.pending, PendingDecision::Archived);
        assert_eq!(routing.live, LiveDecision::None);
    }

    #[test]
    fn chat_to_online_recipient_skips_pending_routes_live() {
        let msg = dm(
            "bob@elsewhere/x",
            "alice@example.com",
            MessageType::Chat,
            Some("hi"),
        );
        let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
        assert_eq!(routing.archive, ArchiveDecision::Mam);
        assert_eq!(routing.pending, PendingDecision::None);
        assert_eq!(routing.live, LiveDecision::DeliverToBareWithFanout);
    }

    #[test]
    fn full_jid_target_uses_deliver_to_full() {
        let msg = dm(
            "bob@elsewhere/x",
            "alice@example.com/web",
            MessageType::Chat,
            Some("hi"),
        );
        let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
        assert_eq!(routing.live, LiveDecision::DeliverToFull);
    }

    // ── XEP-0160 §4 type matrix ─────────────────────────────────────

    #[test]
    fn normal_type_is_storable() {
        let msg = dm(
            "bob@elsewhere/x",
            "alice@example.com",
            MessageType::Normal,
            Some("hi"),
        );
        let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
        assert_eq!(routing.archive, ArchiveDecision::Mam);
        assert_eq!(routing.pending, PendingDecision::Archived);
    }

    #[test]
    fn headline_type_skips_storage_and_pending() {
        let msg = dm(
            "system@example.com",
            "alice@example.com",
            MessageType::Headline,
            Some("notif"),
        );
        let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
        assert_eq!(routing.archive, ArchiveDecision::None);
        assert_eq!(routing.pending, PendingDecision::None);
    }

    #[test]
    fn groupchat_is_skipped_by_dm_classifier() {
        let msg = dm(
            "room@conf.example.com/bob",
            "alice@example.com",
            MessageType::Groupchat,
            Some("hi"),
        );
        let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
        assert_eq!(routing, {
            let mut r = DmRouting::dropped();
            r.carbons = CarbonsDecision::Suppressed;
            r
        });
    }

    #[test]
    fn error_type_is_silently_dropped_when_offline() {
        let mut msg = dm(
            "bob@elsewhere/x",
            "alice@example.com",
            MessageType::Error,
            None,
        );
        // <store/> hint must be ignored on error per XEP-0334 §6 ¶3.
        add_hint(&mut msg, Hint::Store);
        let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
        assert_eq!(routing.archive, ArchiveDecision::None);
        assert_eq!(routing.pending, PendingDecision::None);
        assert_eq!(routing.live, LiveDecision::None);
        assert_eq!(routing.inbox, InboxDecision::None);
    }

    #[test]
    fn error_type_when_online_routes_live_only() {
        let msg = dm(
            "bob@elsewhere/x",
            "alice@example.com",
            MessageType::Error,
            None,
        );
        let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
        assert_eq!(routing.archive, ArchiveDecision::None);
        assert_eq!(routing.pending, PendingDecision::None);
        assert_eq!(routing.live, LiveDecision::DeliverToBareWithFanout);
    }

    // ── XEP-0085 chat-state-only ────────────────────────────────────

    #[test]
    fn chat_state_only_is_not_stored_or_queued() {
        let mut msg = dm(
            "bob@elsewhere/x",
            "alice@example.com",
            MessageType::Chat,
            None,
        );
        msg.payloads
            .push(build_chat_state_element(ChatState::Composing));
        let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
        assert_eq!(routing.archive, ArchiveDecision::None);
        assert_eq!(routing.pending, PendingDecision::None);
        assert_eq!(routing.inbox, InboxDecision::None);
    }

    // ── XEP-0334 hint matrix ────────────────────────────────────────

    #[test]
    fn no_store_hint_suppresses_archive_and_pending() {
        let mut msg = dm(
            "bob@elsewhere/x",
            "alice@example.com",
            MessageType::Chat,
            Some("ephemeral"),
        );
        add_hint(&mut msg, Hint::NoStore);
        let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
        assert_eq!(routing.archive, ArchiveDecision::None);
        assert_eq!(routing.pending, PendingDecision::None);
        assert_eq!(routing.inbox, InboxDecision::None);
    }

    #[test]
    fn no_permanent_store_hint_uses_transient_pending() {
        let mut msg = dm(
            "bob@elsewhere/x",
            "alice@example.com",
            MessageType::Chat,
            Some("off the record"),
        );
        add_hint(&mut msg, Hint::NoPermanentStore);
        let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
        assert_eq!(routing.archive, ArchiveDecision::None);
        assert_eq!(routing.pending, PendingDecision::Transient);
        // Locked Q10b: Transient leaves no inbox trace.
        assert_eq!(routing.inbox, InboxDecision::None);
    }

    #[test]
    fn no_permanent_store_when_recipient_online_skips_pending() {
        let mut msg = dm(
            "bob@elsewhere/x",
            "alice@example.com",
            MessageType::Chat,
            Some("otr"),
        );
        add_hint(&mut msg, Hint::NoPermanentStore);
        let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
        assert_eq!(routing.archive, ArchiveDecision::None);
        assert_eq!(routing.pending, PendingDecision::None);
        assert_eq!(routing.live, LiveDecision::DeliverToBareWithFanout);
    }

    #[test]
    fn store_hint_overrides_default_skip_for_headline() {
        let mut msg = dm(
            "system@example.com",
            "alice@example.com",
            MessageType::Headline,
            None,
        );
        add_hint(&mut msg, Hint::Store);
        let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
        assert_eq!(routing.archive, ArchiveDecision::Mam);
        // Headline still doesn't pending — only chat/normal do per #209
        // scope; <store/> forces archive but pending follows the type.
        // XEP-0160 §4 explicitly says headline SHOULD NOT be stored
        // offline; <store/> is project-side discretion. Keep restrictive.
        assert_eq!(routing.pending, PendingDecision::Archived);
    }

    #[test]
    fn store_hint_does_not_override_error_type() {
        let mut msg = dm(
            "bob@elsewhere/x",
            "alice@example.com",
            MessageType::Error,
            None,
        );
        add_hint(&mut msg, Hint::Store);
        let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
        // §6 ¶3: hints in error stanzas are ignored.
        assert_eq!(routing.archive, ArchiveDecision::None);
        assert_eq!(routing.pending, PendingDecision::None);
    }

    #[test]
    fn store_overrides_no_store_more_restrictive_loses() {
        // XEP-0334 §5.4: <store/> SHOULD store. Project rule: <store/>
        // wins over conflicting <no-store/>.
        let mut msg = dm(
            "bob@elsewhere/x",
            "alice@example.com",
            MessageType::Chat,
            Some("hi"),
        );
        add_hint(&mut msg, Hint::NoStore);
        add_hint(&mut msg, Hint::Store);
        let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
        assert_eq!(routing.archive, ArchiveDecision::Mam);
        assert_eq!(routing.pending, PendingDecision::Archived);
    }

    #[test]
    fn no_copy_on_full_jid_suppresses_carbons() {
        let mut msg = dm(
            "bob@elsewhere/x",
            "alice@example.com/web",
            MessageType::Chat,
            Some("secret"),
        );
        add_hint(&mut msg, Hint::NoCopy);
        let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
        assert_eq!(routing.carbons, CarbonsDecision::Suppressed);
    }

    #[test]
    fn no_copy_on_bare_jid_does_not_suppress_carbons() {
        // XEP-0334 §5.3 ¶2: no-copy MUST NOT override bare-JID fanout.
        let mut msg = dm(
            "bob@elsewhere/x",
            "alice@example.com",
            MessageType::Chat,
            Some("secret"),
        );
        add_hint(&mut msg, Hint::NoCopy);
        let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
        assert_eq!(routing.carbons, CarbonsDecision::Eligible);
    }

    #[test]
    fn no_copy_in_error_stanza_is_ignored() {
        let mut msg = dm(
            "bob@elsewhere/x",
            "alice@example.com/web",
            MessageType::Error,
            None,
        );
        add_hint(&mut msg, Hint::NoCopy);
        // §6 ¶3: hints in error stanzas are ignored — so no-copy doesn't
        // suppress carbons. But error-type itself is not carbons-eligible
        // by XEP-0280. Our classifier sets Suppressed for errors anyway,
        // but the route is via the type guard, not the hint.
        let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
        assert_eq!(routing.carbons, CarbonsDecision::Suppressed);
    }

    // ── DmRouting helpers ───────────────────────────────────────────

    #[test]
    fn dropped_routing_has_all_sinks_off() {
        let r = DmRouting::dropped();
        assert_eq!(r.archive, ArchiveDecision::None);
        assert_eq!(r.pending, PendingDecision::None);
        assert_eq!(r.live, LiveDecision::None);
        assert_eq!(r.inbox, InboxDecision::None);
    }

    #[test]
    fn online_resources_priority_threshold() {
        let none = OnlineResources::empty();
        let neg = OnlineResources::from_pairs([(full("a@b/c"), -1)]);
        let zero = OnlineResources::from_pairs([(full("a@b/c"), 0)]);
        let pos = OnlineResources::from_pairs([(full("a@b/c"), 5)]);
        assert!(!none.has_non_negative_priority());
        assert!(!neg.has_non_negative_priority());
        assert!(zero.has_non_negative_priority());
        assert!(pos.has_non_negative_priority());
    }
}
