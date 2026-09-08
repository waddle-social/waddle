//! Notification candidate construction and sender-provenance validation.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationCandidate {
    pub(super) recipient_bare_jid: BareJid,
    pub(super) conversation_jid: BareJid,
    pub(super) sender_jid: Jid,
    pub(super) thread_id: NotificationThreadId,
    pub(super) archive_stanza_id: StanzaId,
    pub(super) class: NotificationClass,
    pub(super) reason: NotificationReason,
    pub(super) policy_error_count: i64,
    /// XEP-0513 `<noping/>` mention hint, message-frozen at T0. When
    /// `true`, the T1 evaluator suppresses the candidate with
    /// [`SuppressedReason::Xep0513Noping`] — sender opted the
    /// recipient out of being pinged for this mention.
    pub(super) noping: bool,
    /// XEP-0334 `<no-store/>` hint, message-frozen at T0. When `true`
    /// the body is stripped from the XEP-0357 summary at T1 (the
    /// minimal push still fires).
    pub(super) no_store: bool,
    /// XEP-0334 `<no-permanent-store/>` hint, message-frozen at T0.
    /// When `true` the body is stripped from the XEP-0357 summary at T1
    /// (the minimal push still fires).
    pub(super) no_permanent_store: bool,
    /// Snapshot of the message body, message-frozen at T0, used to build
    /// the optional XEP-0357 §5.4 `last-message-body` field when the
    /// recipient opts in (see [`RichSummary`]). `None` when the message
    /// had no body OR when an XEP-0334 `<no-store/>`/`<no-permanent-store/>`
    /// hint applies — an off-the-record body is never persisted onto the
    /// candidate row, even temporarily (XEP-0334 §3 storage conformance).
    pub(super) last_message_body: Option<String>,
    /// XEP-0444 reaction-only hint, message-frozen at T0 (#780).
    /// `true` when the originating message carried `<reactions/>` and
    /// no substantive body — the T1 evaluator suppresses with
    /// [`SuppressedReason::Xep0444Reaction`]; "Alice reacted 👍" is
    /// archived (MAM is untouched) but never fires an OS push.
    pub(super) reaction: bool,
}

/// Message-frozen suppression hints carried on a
/// [`NotificationCandidate`] from T0 emission to T1 dispatch.
///
/// Per locked Q3 (see #506), T0 declines candidates only for
/// structural-validity reasons (self-DM). Message-frozen suppression
/// hints like XEP-0513 `<noping/>` and XEP-0334 storage hints are
/// recipient-level signals from the sender — the candidate is still
/// constructed and persisted, and the T1 evaluator reads the hint
/// back and suppresses with the typed `SuppressedReason`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NotificationMessageHints {
    pub noping: bool,
    pub no_store: bool,
    pub no_permanent_store: bool,
    /// XEP-0444 reaction-only message (#780) — reactions payload with
    /// no substantive body.
    pub reaction: bool,
}

impl NotificationMessageHints {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn with_noping(mut self, noping: bool) -> Self {
        self.noping = noping;
        self
    }

    pub fn with_xep0334(mut self, no_store: bool, no_permanent_store: bool) -> Self {
        self.no_store = no_store;
        self.no_permanent_store = no_permanent_store;
        self
    }

    pub fn with_reaction(mut self, reaction: bool) -> Self {
        self.reaction = reaction;
        self
    }
}

impl NotificationCandidate {
    pub fn direct_message(
        recipient_bare_jid: BareJid,
        sender_jid: Jid,
        archive_stanza_id: StanzaId,
        is_mention: bool,
    ) -> Result<Self, NotificationOutboxError> {
        Self::direct_message_with_hints(
            recipient_bare_jid,
            sender_jid,
            archive_stanza_id,
            is_mention,
            NotificationMessageHints::none(),
        )
    }

    pub fn direct_message_with_hints(
        recipient_bare_jid: BareJid,
        sender_jid: Jid,
        archive_stanza_id: StanzaId,
        is_mention: bool,
        hints: NotificationMessageHints,
    ) -> Result<Self, NotificationOutboxError> {
        require_full_sender_jid(&sender_jid)?;
        // Structural invariant: a notification candidate cannot be
        // self-directed. A self-DM (sender bare JID == recipient bare
        // JID) is not a valid push candidate at all — there is no
        // distinct recipient to notify, so the candidate is malformed
        // by construction. This is *input validation*, not recipient-
        // state suppression, so it lives at the constructor boundary
        // alongside the existing full-sender-JID and archive-id owner
        // checks. T0 emission paths surface this error as a typed
        // emission no-op; no candidate row is persisted. (Per #506 Q3:
        // T0 has no recipient-state reads — sender vs recipient JID
        // comparison is message-intrinsic provenance.)
        if sender_jid.to_bare() == recipient_bare_jid {
            return Err(NotificationOutboxError::SelfDirectedNotificationCandidate(
                recipient_bare_jid,
            ));
        }
        let expected_by = Jid::from(recipient_bare_jid.clone());
        if archive_stanza_id.by != expected_by {
            return Err(NotificationOutboxError::ArchiveStanzaIdOwnerMismatch {
                expected: expected_by,
                actual: archive_stanza_id.by,
            });
        }
        let (class, reason) = if is_mention {
            (
                NotificationClass::DirectMessageMention,
                NotificationReason::OfflineDirectMessageMention,
            )
        } else {
            (
                NotificationClass::DirectMessage,
                NotificationReason::OfflineDirectMessage,
            )
        };
        Ok(Self {
            recipient_bare_jid,
            conversation_jid: sender_jid.to_bare(),
            sender_jid,
            thread_id: NotificationThreadId::root(),
            archive_stanza_id,
            class,
            reason,
            policy_error_count: 0,
            noping: hints.noping,
            no_store: hints.no_store,
            no_permanent_store: hints.no_permanent_store,
            last_message_body: None,
            reaction: hints.reaction,
        })
    }

    pub fn groupchat(
        recipient_bare_jid: BareJid,
        conversation_jid: BareJid,
        sender_jid: Jid,
        thread_id: NotificationThreadId,
        archive_stanza_id: StanzaId,
        class: NotificationClass,
    ) -> Result<Self, NotificationOutboxError> {
        Self::groupchat_with_hints(
            recipient_bare_jid,
            conversation_jid,
            sender_jid,
            thread_id,
            archive_stanza_id,
            class,
            NotificationMessageHints::none(),
        )
    }

    pub fn groupchat_with_hints(
        recipient_bare_jid: BareJid,
        conversation_jid: BareJid,
        sender_jid: Jid,
        thread_id: NotificationThreadId,
        archive_stanza_id: StanzaId,
        class: NotificationClass,
        hints: NotificationMessageHints,
    ) -> Result<Self, NotificationOutboxError> {
        require_full_sender_jid(&sender_jid)?;
        require_sender_matches_conversation(&sender_jid, &conversation_jid)?;
        let expected_by = Jid::from(conversation_jid.clone());
        if archive_stanza_id.by != expected_by {
            return Err(NotificationOutboxError::ArchiveStanzaIdOwnerMismatch {
                expected: expected_by,
                actual: archive_stanza_id.by,
            });
        }
        let reason = match class {
            NotificationClass::PersonalMention => NotificationReason::GroupchatPersonalMention,
            NotificationClass::ChannelMention => NotificationReason::GroupchatChannelMention,
            NotificationClass::ActiveChannelMention => {
                NotificationReason::GroupchatActiveChannelMention
            }
            NotificationClass::NotifyAll => NotificationReason::GroupchatNotifyAll,
            NotificationClass::DirectMessage | NotificationClass::DirectMessageMention => {
                return Err(NotificationOutboxError::InvalidClass(
                    class.as_db_value().to_string(),
                ));
            }
        };
        Ok(Self {
            recipient_bare_jid,
            conversation_jid,
            sender_jid,
            thread_id,
            archive_stanza_id,
            class,
            reason,
            policy_error_count: 0,
            noping: hints.noping,
            no_store: hints.no_store,
            no_permanent_store: hints.no_permanent_store,
            last_message_body: None,
            reaction: hints.reaction,
        })
    }

    /// Snapshot the message body for the optional XEP-0357 §5.4
    /// `last-message-body` field.
    ///
    /// XEP-0334 §3 storage conformance: when this candidate carries a
    /// `<no-store/>` or `<no-permanent-store/>` hint, the body is dropped
    /// here so an off-the-record body is never persisted onto the
    /// candidate row — not even temporarily. The T1 evaluator applies the
    /// same hint precedence again when resolving the [`RichSummary`]
    /// (defense in depth + the XEP-defined T1 decision point).
    pub fn with_last_message_body(mut self, body: Option<String>) -> Self {
        self.last_message_body = if self.no_store || self.no_permanent_store {
            None
        } else {
            body
        };
        self
    }

    pub fn last_message_body(&self) -> Option<&str> {
        self.last_message_body.as_deref()
    }

    pub fn recipient_bare_jid(&self) -> &BareJid {
        &self.recipient_bare_jid
    }

    pub fn conversation_jid(&self) -> &BareJid {
        &self.conversation_jid
    }

    pub fn sender_jid(&self) -> &Jid {
        &self.sender_jid
    }

    pub fn thread_id(&self) -> &NotificationThreadId {
        &self.thread_id
    }

    pub fn archive_stanza_id(&self) -> &StanzaId {
        &self.archive_stanza_id
    }

    /// Rebind a planned candidate to the committed identity from the same authority.
    pub(crate) fn restamp_archive_id(&mut self, recorded: &StanzaId) {
        if self.archive_stanza_id.by == recorded.by {
            self.archive_stanza_id = recorded.clone();
        }
    }

    pub fn class(&self) -> NotificationClass {
        self.class
    }

    pub fn reason(&self) -> NotificationReason {
        self.reason
    }

    pub fn noping(&self) -> bool {
        self.noping
    }

    pub fn no_store(&self) -> bool {
        self.no_store
    }

    pub fn no_permanent_store(&self) -> bool {
        self.no_permanent_store
    }

    pub fn reaction(&self) -> bool {
        self.reaction
    }
}

pub(super) fn require_full_sender_jid(sender_jid: &Jid) -> Result<(), NotificationOutboxError> {
    if sender_jid.resource().is_some() {
        Ok(())
    } else {
        Err(NotificationOutboxError::SenderJidMissingResource(
            sender_jid.clone(),
        ))
    }
}

pub(super) fn require_full_sender_jid_set(
    sender_jids: &[Jid],
) -> Result<(), NotificationOutboxError> {
    if sender_jids.is_empty() {
        return Err(NotificationOutboxError::MissingSenderJidSet);
    }
    sender_jids.iter().try_for_each(require_full_sender_jid)
}

pub(super) fn require_sender_matches_conversation(
    sender_jid: &Jid,
    conversation_jid: &BareJid,
) -> Result<(), NotificationOutboxError> {
    if sender_jid.to_bare() == *conversation_jid {
        Ok(())
    } else {
        Err(NotificationOutboxError::SenderConversationMismatch {
            sender: sender_jid.clone(),
            conversation: conversation_jid.clone(),
        })
    }
}

pub(super) fn require_sender_set_matches_conversation(
    sender_jids: &[Jid],
    conversation_jid: &BareJid,
) -> Result<(), NotificationOutboxError> {
    sender_jids.iter().try_for_each(|sender_jid| {
        require_sender_matches_conversation(sender_jid, conversation_jid)
    })
}

pub(super) fn require_sender_set_contains_scalar(
    sender_jids: &[Jid],
    sender_jid: &Jid,
) -> Result<(), NotificationOutboxError> {
    if sender_jids.iter().any(|candidate| candidate == sender_jid) {
        Ok(())
    } else {
        Err(NotificationOutboxError::SenderJidSetMissingScalar(
            sender_jid.clone(),
        ))
    }
}
