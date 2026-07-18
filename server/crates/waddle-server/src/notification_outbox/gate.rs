//! Push dispatch gate: staged (T0 emit / T1 drain) typed evaluation of
//! XEP-0492 levels, XEP-0513 filters, XEP-0334 hints, XEP-0191 blocks,
//! and Waddle DND, with per-batch caches.

use super::*;

/// Typed outcome of `evaluate_push_gate_at_dispatch`.
///
/// Extends [`crate::notification_settings_projection::PushDispatchDecision`]
/// with a third state — `DeferUnknownRoomPolicy` — that surfaces the
/// "room actor not currently live" signal as a retry rather than
/// silently defaulting to public. Slice 1 has no durable T1
/// projection of MUC `members_only`; if the actor lookup returns
/// `Ok(None)` we cannot know whether the room is private (default
/// `Always` level → `NotifyAll` candidates SHOULD push) or public
/// (default `OnMention` level → `NotifyAll` candidates SHOULD NOT
/// push), and silently picking either would either drop legitimate
/// private-room pushes or fan out unwanted public-room pushes. Slice
/// 2 will replace the live actor lookup with a durable projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum T1PushDispatchOutcome {
    /// Push gate decided to fan out; enqueue the push job. `rich`
    /// carries the T1-resolved XEP-0357 §5.4 summary fields (minimal at
    /// T0Emit; resolved from the recipient's XEP-0492 opt-in and the
    /// candidate's XEP-0334 hints at T1Drain).
    Deliver { rich: RichSummary },
    /// Push gate decided to suppress; mark candidate outboxed without
    /// enqueueing a job. `reason` is the typed audit reason that
    /// caused suppression (XEP-0492 `<never/>` / `<on-mention/>` miss,
    /// XEP-0191 blocking, XEP-0513 `<noping/>`, or Waddle DnD).
    Suppressed { reason: SuppressedReason },
    /// MUC config could not be resolved (room actor unavailable or
    /// failed). Defer with policy-error backoff so the next drain
    /// pass can retry once the actor (or, slice 2, the durable
    /// projection) is available.
    DeferUnknownRoomPolicy,
}

/// Typed per-batch cache entry for the [`RoomPolicyStore`] lookup.
///
/// `Unknown` is deliberately distinct from `Public` — see
/// [`T1PushDispatchOutcome::DeferUnknownRoomPolicy`] for the
/// reasoning. Once a room resolves to `Unknown` for a given batch,
/// every candidate for that room in the same batch reuses that
/// outcome to avoid retrying the same failing actor 100×.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoomPolicyCacheEntry {
    Public,
    Private,
    /// MUC policy could not be resolved. Wrapped source distinguishes
    /// the expected/normal `Ok(None)` (room not currently live) case
    /// from the actionable `Err(_)` (actor transport / lookup failure)
    /// case so production debugging and alert triage can act on the
    /// distinction. Both still defer identically at the dispatch site
    /// (the typed `T1PushDispatchOutcome::DeferUnknownRoomPolicy`).
    Unknown(UnknownRoomPolicySource),
}

/// Why a [`RoomPolicyCacheEntry::Unknown`] was produced. Logged at
/// most once per (drain batch, room) thanks to the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnknownRoomPolicySource {
    /// `RoomPolicyStore::room_members_only` returned `Ok(None)` —
    /// the room actor is not currently live. Expected/normal on
    /// restart windows or for rooms with no recent activity.
    NotLive,
    /// `RoomPolicyStore::room_members_only` returned `Err(_)` —
    /// an actor transport failure or other lookup error. Actionable;
    /// surfaces the underlying error string at cache-miss time so
    /// operators can correlate without needing every per-candidate
    /// log line.
    LookupError,
}

/// Push-dispatch gate evaluator.
///
/// Single typed entry point that decides publish/suppress/defer for a
/// [`NotificationCandidate`]. The function name was previously
/// `evaluate_xep0492_at_dispatch`; the responsibility has since grown
/// to cover the full XEP/Waddle suppressor matrix consulted at push
/// dispatch — XEP-0492 (`<never/>` / `<on-mention/>`), XEP-0191
/// (blocklist), XEP-0513 (`<noping/>`), XEP-0334 (`<no-store/>` /
/// `<no-permanent-store/>`), and Waddle DnD — so the name now
/// reflects the actual gate, not just one of its inputs.
///
/// **Same typed evaluator called at two invocation moments**:
///
/// - **T0 (candidate emission gate, compliance)** — DM
///   ([`crate::server::routes::interpret::offline_delivery`]) and
///   groupchat
///   ([`crate::server::routes::interpret::groupchat_inbox`])
///   emission paths invoke this on a constructed-but-not-inserted
///   [`NotificationCandidate`] before persisting it. A `Suppressed`
///   outcome short-circuits emission entirely — no row is written.
///   This satisfies the compliance rule that suppressed candidates
///   leave no audit trail in `notification_candidates`.
/// - **T1 (drain re-evaluator, race-window guard)** — the same
///   function runs again inside
///   [`NotificationOutboxStore::drain_pending_candidates_into_outbox`]
///   against fresh recipient state. If the projection changed
///   between T0 and T1 (e.g. the user flipped XEP-0492 to
///   `<never/>` mid-flight), the drain marks the candidate outboxed
///   without enqueuing a job. The brief race window where a row
///   exists then gets retroactively suppressed is acceptable per
///   the locked Q2 design.
///
/// Derives `(level, is_mention)` from the recorded candidate class +
/// recipient state and feeds them into the shared pure reducer
/// [`crate::notification_settings_projection::PushDispatchDecision::evaluate`].
///
/// - DM classes encode the mention bit directly
///   ([`NotificationClass::DirectMessage`] vs
///   [`NotificationClass::DirectMessageMention`]). DM evaluation
///   never consults `room_policy` and may pass any [`RoomPolicyStore`]
///   (e.g. [`NoopRoomPolicy`]) at the T0 call site.
/// - Groupchat classes encode both mention scope and
///   live-occupant scope; the room is private/public per the
///   [`RoomPolicyStore`] lookup, cached per call through
///   `room_policy_cache` so a 100-member groupchat does not produce
///   100 actor round-trips at T1, and a single-message emission at
///   T0 trivially hits one entry. When the lookup yields
///   `Ok(None)`/`Err(_)`, the evaluator returns
///   [`T1PushDispatchOutcome::DeferUnknownRoomPolicy`] — slice 1 has
///   no durable T1 projection of MUC config yet, so an unknown
///   policy must defer rather than default-to-public.
///
/// Which leg of the push pipeline is invoking the evaluator.
///
/// The single typed function runs at two moments per #506 Q3 — T0
/// emission gate (compliance: no row for XEP-0492 suppressed) and T1
/// drain (race-window guard + durable audit). The two legs DO NOT
/// share the full suppressor set: message-frozen suppressors (XEP-0513
/// `<noping/>`, XEP-0334 `<no-store/>` / `<no-permanent-store/>`) and
/// Waddle DnD are deliberately skipped at T0 so the candidate row
/// persists with its hint bits and the typed `suppressed_reason`
/// audit fires at T1 — without this split, hinted candidates would
/// be silently filtered at T0 with no audit trail, contradicting the
/// [`NotificationMessageHints`] contract.
///
/// T0 still applies recipient-state suppressors that compliance
/// requires to leave no row at all (XEP-0492 `<never/>`/`<on-mention/>`
/// miss). Those persist their suppression intent via metric counters
/// at the T0 emission site; the row itself is the audit surface for
/// everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PushEvalStage {
    /// Called synchronously from `enqueue_xep0357_notification_candidate_*`
    /// before `insert_candidate`. A `Suppressed` outcome here means
    /// the row will NOT be persisted — reserve this stage for
    /// compliance-required suppressors only.
    T0Emit,
    /// Called from `drain_pending_candidates_into_outbox` against an
    /// already-persisted candidate. `Suppressed` outcomes here are
    /// recorded as `suppressed_reason` on the row and counted via
    /// `increment_push_suppressed` — full audit + observability.
    T1Drain,
}

/// Typed bundle of recipient-state readers consulted by
/// [`evaluate_push_gate_at_dispatch`].
///
/// Both T0 emission sites and the T1 drain loop construct this once
/// per dispatch call. Bundling keeps the evaluator argument count
/// below the clippy `too_many_arguments` floor without resorting to
/// `#[allow]` — every field stays a typed trait object so the caller
/// supplies the production impl or a typed test double.
#[derive(Copy, Clone)]
pub(crate) struct PushEvalDeps<'a> {
    pub settings_projection:
        &'a crate::notification_settings_projection::NotificationSettingsProjectionStore,
    pub room_policy: &'a dyn RoomPolicyStore,
    pub dnd_reader: &'a dyn DndReader,
    pub activity_reader: &'a dyn NotificationActivityReader,
    /// Active-mention TTL window in milliseconds. The T1 evaluator
    /// suppresses [`NotificationClass::ActiveChannelMention`]
    /// candidates whose recipient's
    /// [`crate::notification_activity::NotificationActivity::last_active_at_ms`]
    /// is older than `now - active_mention_ttl_ms`.
    pub active_mention_ttl_ms: i64,
}

/// Mutable per-drain-pass caches threaded through
/// [`evaluate_push_gate_at_dispatch`]. Bundling keeps the argument
/// count down and gives callers a single allocation site for the
/// three caches.
pub(crate) struct PushEvalCaches<'a> {
    pub room_policy: &'a mut std::collections::BTreeMap<BareJid, RoomPolicyCacheEntry>,
    pub dnd: &'a mut std::collections::BTreeMap<BareJid, DndState>,
    pub activity:
        &'a mut std::collections::BTreeMap<(BareJid, BareJid), Option<NotificationActivity>>,
}

pub(crate) async fn evaluate_push_gate_at_dispatch(
    stage: PushEvalStage,
    deps: PushEvalDeps<'_>,
    candidate: &NotificationCandidate,
    caches: &mut PushEvalCaches<'_>,
) -> Result<T1PushDispatchOutcome, NotificationOutboxError> {
    let PushEvalDeps {
        settings_projection,
        room_policy,
        dnd_reader,
        activity_reader,
        active_mention_ttl_ms,
    } = deps;
    let room_policy_cache = &mut *caches.room_policy;
    let dnd_cache = &mut *caches.dnd;
    let activity_cache = &mut *caches.activity;
    // Message-frozen suppressor (`<noping/>`) runs ONLY at T1Drain so
    // the candidate row is persisted with its hint bits and the typed
    // `suppressed_reason` audit can fire. At T0Emit the row doesn't
    // exist yet — suppressing here would leave no audit trail,
    // defeating the whole purpose of snapshotting the hint bits onto
    // `NotificationCandidate` in the first place.
    //
    // XEP-0334 `<no-store/>`/`<no-permanent-store/>` are NOT push
    // suppressors. Per XEP-0334 §3 they scope to message *storage*
    // (archives, offline queues, logs), and §8 cautions that hints
    // MUST NOT be relied on for any particular purpose — a transient
    // push notification is not "storage". They instead strip the
    // `last-message-body` from the rich XEP-0357 summary (the body, not
    // the notification, is what would become a semi-permanent record at
    // the push gateway). That stripping is resolved alongside the
    // recipient's opt-in in `resolve_rich_summary` below; the minimal
    // push still fires.
    if stage == PushEvalStage::T1Drain && candidate.noping() {
        return Ok(T1PushDispatchOutcome::Suppressed {
            reason: SuppressedReason::Xep0513Noping,
        });
    }

    // #780: XEP-0444 reaction-only messages are archived (MAM is the
    // right place for them) but never fire an OS push — matching the
    // wider XMPP client ecosystem (Conversations, Snikket, Movim).
    // Message-frozen like `<noping/>`, so it runs ONLY at T1Drain for
    // the same audit-row reason documented above.
    if stage == PushEvalStage::T1Drain && candidate.reaction() {
        return Ok(T1PushDispatchOutcome::Suppressed {
            reason: SuppressedReason::Xep0444Reaction,
        });
    }

    let (conversation_kind, is_mention) = match candidate.class() {
        NotificationClass::DirectMessage => (
            crate::notification_settings_projection::ConversationKind::Direct,
            false,
        ),
        NotificationClass::DirectMessageMention => (
            crate::notification_settings_projection::ConversationKind::Direct,
            true,
        ),
        NotificationClass::PersonalMention
        | NotificationClass::ChannelMention
        | NotificationClass::ActiveChannelMention => {
            match resolve_cached_room_policy(
                room_policy,
                candidate.conversation_jid(),
                room_policy_cache,
            )
            .await
            {
                RoomPolicyCacheEntry::Private => (
                    crate::notification_settings_projection::ConversationKind::PrivateGroup,
                    true,
                ),
                RoomPolicyCacheEntry::Public => (
                    crate::notification_settings_projection::ConversationKind::PublicGroup,
                    true,
                ),
                RoomPolicyCacheEntry::Unknown(_) => {
                    return Ok(T1PushDispatchOutcome::DeferUnknownRoomPolicy);
                }
            }
        }
        NotificationClass::NotifyAll => {
            match resolve_cached_room_policy(
                room_policy,
                candidate.conversation_jid(),
                room_policy_cache,
            )
            .await
            {
                RoomPolicyCacheEntry::Private => (
                    crate::notification_settings_projection::ConversationKind::PrivateGroup,
                    false,
                ),
                RoomPolicyCacheEntry::Public => (
                    crate::notification_settings_projection::ConversationKind::PublicGroup,
                    false,
                ),
                RoomPolicyCacheEntry::Unknown(_) => {
                    return Ok(T1PushDispatchOutcome::DeferUnknownRoomPolicy);
                }
            }
        }
    };
    // Waddle DnD is a recipient-state read, fresh-at-T1 alongside
    // XEP-0492. The per-batch cache keys on (user → state) so a
    // recipient with many candidates in the same drain pass only
    // reads DnD once.
    //
    // Skipped at T0Emit so a hinted candidate from a DnD'd recipient
    // still persists (T1 then records DnD or hint reason as
    // appropriate). DnD also moves with the recipient between T0
    // and T1; the T1 re-evaluation is the authoritative read.
    if stage == PushEvalStage::T1Drain {
        let dnd_state =
            resolve_cached_dnd_state(dnd_reader, candidate.recipient_bare_jid(), dnd_cache).await?;
        if matches!(dnd_state, DndState::Active) {
            return Ok(T1PushDispatchOutcome::Suppressed {
                reason: SuppressedReason::WaddleDnd,
            });
        }
    }
    // XEP-0513 `<active/>` filter — only `ActiveChannelMention`
    // class candidates consult the per-(recipient, conversation)
    // activity projection. Other classes (DM, personal/channel
    // mention, notify-all) are unaffected: the `<active/>` filter is
    // a class-specific gate.
    //
    // Skipped at T0Emit per the recipient-state / fresh-read T1
    // contract: current activity is a T1 read, and consulting it at
    // T0 would conflate "active now" with "active at message-frozen
    // time". The candidate row persists through T0 and the T1 drain
    // either delivers or records the typed `Xep0513ActiveMiss`
    // suppression — same audit trail shape as the other T1-only
    // suppressors.
    if stage == PushEvalStage::T1Drain
        && matches!(candidate.class(), NotificationClass::ActiveChannelMention)
    {
        let activity = resolve_cached_activity(
            activity_reader,
            candidate.recipient_bare_jid(),
            candidate.conversation_jid(),
            activity_cache,
        )
        .await?;
        let now_ms = crate::time::now_ms();
        let is_active = match activity {
            None => false,
            Some(activity) => {
                // `crate::time::now_ms` is `chrono::Utc::now()` — wall-clock,
                // not monotonic. A projection row written by a writer whose
                // clock is ahead of the evaluator's (NTP skew, replica
                // drift, an ingestion path that stamped a future time)
                // would otherwise produce a *negative* `age`, which the
                // `age <= TTL` predicate silently treats as "active" until
                // the wall clock catches up — quietly extending the
                // configured TTL window. Clamp the stored timestamp to
                // `now_ms` before subtracting so the predicate operates on
                // a non-negative `age`; a future-stamped row is treated as
                // "active at `now_ms`" and ages naturally from there.
                let last_active = activity.last_active_at_ms.min(now_ms);
                let age = now_ms.saturating_sub(last_active);
                age <= active_mention_ttl_ms
            }
        };
        if !is_active {
            return Ok(T1PushDispatchOutcome::Suppressed {
                reason: SuppressedReason::Xep0513ActiveMiss,
            });
        }
    }
    // One projection read yields both the XEP-0492 level and the
    // rich-payload opt-in — the delivery path needs both, and fetching
    // the row twice would double projection-store IO per delivering
    // candidate on a channel fan-out.
    let (level, rich_opt_in) = settings_projection
        .effective_setting_and_rich_opt_in(
            candidate.recipient_bare_jid(),
            candidate.conversation_jid(),
            conversation_kind,
        )
        .await?;
    let decision =
        crate::notification_settings_projection::PushDispatchDecision::evaluate(level, is_mention);
    Ok(match decision {
        crate::notification_settings_projection::PushDispatchDecision::Deliver => {
            let rich = resolve_rich_summary(stage, rich_opt_in, candidate);
            T1PushDispatchOutcome::Deliver { rich }
        }
        crate::notification_settings_projection::PushDispatchDecision::Suppressed { reason } => {
            T1PushDispatchOutcome::Suppressed {
                reason: suppressed_reason_for_level(reason),
            }
        }
    })
}

/// Resolve the XEP-0357 §5.4 rich summary fields for a delivering
/// candidate.
///
/// The rich summary is a T1 concern: the recipient's XEP-0492
/// `<advanced/>` opt-in is recipient state read fresh at drain (passed
/// in as `opt_in`), and the minimal default (no rich fields) is correct
/// at T0Emit, where the candidate-persistence decision does not need it.
///
/// When the recipient has opted in:
/// - `last-message-sender` is the candidate's full sender JID — routing
///   metadata present in any delivery, preserved even when a hint
///   strips the body. For groupchat this is the room-occupant JID
///   (`room@muc/nick`), never a real JID: the candidate constructor
///   enforces `sender_jid.to_bare() == conversation_jid` via
///   `require_sender_matches_conversation`.
/// - `last-message-body` is included only when no XEP-0334
///   `<no-store/>`/`<no-permanent-store/>` hint applies. The hint always
///   wins over the opt-in: shipping the body to a third-party push
///   gateway is a semi-permanent store of the message. (The body is
///   already `None` on hinted candidates — it was never persisted at T0
///   — but the explicit check keeps the XEP-defined precedence visible
///   and testable at the T1 decision point.)
fn resolve_rich_summary(
    stage: PushEvalStage,
    opt_in: bool,
    candidate: &NotificationCandidate,
) -> RichSummary {
    if stage != PushEvalStage::T1Drain || !opt_in {
        return RichSummary::minimal();
    }
    let body = if candidate.no_store() || candidate.no_permanent_store() {
        None
    } else {
        candidate.last_message_body().map(str::to_owned)
    };
    RichSummary {
        sender: Some(candidate.sender_jid().clone()),
        body,
    }
}

/// Translates a XEP-0492 [`waddle_xmpp::xep::NotificationLevel`]
/// suppression outcome into the typed [`SuppressedReason`] audit
/// variant.
///
/// `<never/>` always maps to `Xep0492Never`. `<on-mention/>` maps to
/// `Xep0492OnMentionMiss` because the XEP-0492 evaluator only emits
/// the `Suppressed` outcome when `should_notify(is_mention)` is false
/// — and for `OnMention` that means `is_mention == false`. Called
/// only from the `Suppressed` arm of the upstream XEP-0492 reducer
/// (`PushDispatchDecision::evaluate`), which never yields
/// `Suppressed` for `<always/>` — so `Always` is unreachable here
/// and the typed contract makes the missing arm a compile-time
/// error if the reducer ever drifts.
fn suppressed_reason_for_level(level: waddle_xmpp::xep::NotificationLevel) -> SuppressedReason {
    match level {
        waddle_xmpp::xep::NotificationLevel::Never => SuppressedReason::Xep0492Never,
        waddle_xmpp::xep::NotificationLevel::OnMention => SuppressedReason::Xep0492OnMentionMiss,
        waddle_xmpp::xep::NotificationLevel::Always => unreachable!(
            "suppressed_reason_for_level called with NotificationLevel::Always; \
             the XEP-0492 reducer never yields Suppressed for <always/>"
        ),
    }
}

/// Looks up `(owner, conversation)` in the per-batch activity cache,
/// populating on miss. The cached `Option` distinguishes "no row in
/// the projection" (`None`) from "row present" (`Some(activity)`) so
/// the XEP-0513 evaluator can branch on the typed shape without
/// re-querying the database for repeats within the same drain pass.
async fn resolve_cached_activity(
    activity_reader: &dyn NotificationActivityReader,
    owner: &BareJid,
    conversation: &BareJid,
    cache: &mut std::collections::BTreeMap<(BareJid, BareJid), Option<NotificationActivity>>,
) -> Result<Option<NotificationActivity>, NotificationOutboxError> {
    let key = (owner.clone(), conversation.clone());
    if let Some(entry) = cache.get(&key) {
        return Ok(entry.clone());
    }
    let activity = activity_reader.read_activity(owner, conversation).await?;
    cache.insert(key, activity.clone());
    Ok(activity)
}

async fn resolve_cached_dnd_state(
    dnd_reader: &dyn DndReader,
    user: &BareJid,
    cache: &mut std::collections::BTreeMap<BareJid, DndState>,
) -> Result<DndState, NotificationOutboxError> {
    if let Some(state) = cache.get(user) {
        return Ok(*state);
    }
    let state = dnd_reader.dnd_state(user).await?;
    cache.insert(user.clone(), state);
    Ok(state)
}

/// Looks up `room` in the per-batch policy cache, populating on miss.
///
/// On miss the raw `room_members_only` result is handled explicitly:
///
/// - `Ok(Some(true/false))` → cache `Private`/`Public`.
/// - `Ok(None)` → cache `Unknown(NotLive)` — expected/normal.
/// - `Err(error)` → emit a `tracing::warn!` with the error string,
///   then cache `Unknown(LookupError)`. Because the result is cached,
///   the warn fires at most once per (drain batch, room) — every
///   subsequent candidate for the same room in this batch hits the
///   cache silently.
async fn resolve_cached_room_policy(
    room_policy: &dyn RoomPolicyStore,
    room: &BareJid,
    cache: &mut std::collections::BTreeMap<BareJid, RoomPolicyCacheEntry>,
) -> RoomPolicyCacheEntry {
    if let Some(entry) = cache.get(room) {
        return *entry;
    }
    let entry = match room_policy.room_members_only(room).await {
        Ok(Some(true)) => RoomPolicyCacheEntry::Private,
        Ok(Some(false)) => RoomPolicyCacheEntry::Public,
        Ok(None) => RoomPolicyCacheEntry::Unknown(UnknownRoomPolicySource::NotLive),
        Err(error) => {
            tracing::warn!(
                %room,
                %error,
                "RoomPolicyStore::room_members_only failed; deferring T1 candidates for this room in the current drain batch"
            );
            RoomPolicyCacheEntry::Unknown(UnknownRoomPolicySource::LookupError)
        }
    };
    cache.insert(room.clone(), entry);
    entry
}

pub(super) async fn xep0191_blocks_notification_job(
    job: &NotificationOutboxJob,
    blocking_storage: &dyn BlockingStorage,
) -> Result<bool, BlockingStorageError> {
    let blocked = blocking_storage
        .list_blocked_jid_entries(job.recipient_bare_jid())
        .await?;
    Ok(blocked
        .into_iter()
        .any(|blocked_jid| xep0191_block_entry_matches_outbox_job(&blocked_jid, job)))
}

pub(super) async fn xep0191_blocks_notification_candidate(
    candidate: &NotificationCandidate,
    blocking_storage: &dyn BlockingStorage,
) -> Result<bool, BlockingStorageError> {
    let blocked = blocking_storage
        .list_blocked_jid_entries(candidate.recipient_bare_jid())
        .await?;
    Ok(blocked.into_iter().any(|blocked_jid| {
        xep0191_block_entry_matches_sender(&blocked_jid, candidate.sender_jid())
    }))
}

fn xep0191_block_entry_matches_outbox_job(blocked_jid: &Jid, job: &NotificationOutboxJob) -> bool {
    if blocked_jid.resource().is_some() {
        job.sender_jids()
            .iter()
            .any(|sender_jid| blocked_jid == sender_jid)
    } else if blocked_jid.node().is_some() {
        blocked_jid.to_bare() == *job.conversation_jid()
    } else {
        blocked_jid.domain() == job.conversation_jid().domain()
    }
}

fn xep0191_block_entry_matches_sender(blocked_jid: &Jid, sender_jid: &Jid) -> bool {
    if blocked_jid.resource().is_some() {
        blocked_jid == sender_jid
    } else if blocked_jid.node().is_some() {
        blocked_jid.to_bare() == sender_jid.to_bare()
    } else {
        blocked_jid.domain() == sender_jid.domain()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notification_outbox::test_support::*;

    /// A `RoomPolicyStore::room_members_only` returning a typed
    /// `RoomPolicyLookup` error MUST classify the cache entry as
    /// `LookupError`, not `NotLive`. The dispatch outcome remains
    /// `DeferUnknownRoomPolicy` either way, but the source split is
    /// what gives operators an actionable signal vs routine dormancy.
    /// Caching still applies: a single failing lookup populates one
    /// `Unknown(LookupError)` entry and every subsequent candidate
    /// for that room reuses it.
    #[tokio::test]
    async fn room_policy_lookup_error_classifies_as_lookup_error_and_caches() {
        let room_policy = ErroringRoomPolicy::new();
        let room = bare("team@muc.example.com");
        let mut cache = std::collections::BTreeMap::<BareJid, RoomPolicyCacheEntry>::new();

        let first = resolve_cached_room_policy(&room_policy, &room, &mut cache).await;
        assert_eq!(
            first,
            RoomPolicyCacheEntry::Unknown(UnknownRoomPolicySource::LookupError),
            "typed RoomPolicyLookup error must classify as LookupError, not NotLive"
        );

        let second = resolve_cached_room_policy(&room_policy, &room, &mut cache).await;
        assert_eq!(
            second, first,
            "second lookup MUST hit the cache and return the same typed entry"
        );

        assert_eq!(
            room_policy.call_count(),
            1,
            "cache MUST short-circuit subsequent lookups — failing actor is never re-asked in the same batch",
        );
    }

    /// A `RoomPolicyStore::room_members_only` returning `Ok(None)`
    /// MUST classify the cache entry as `NotLive`, not `LookupError`.
    /// Distinguishing these is the whole point of the typed source —
    /// `NotLive` is routine dormancy and stays at `debug!` level in
    /// the drain loop, whereas `LookupError` triggers the once-per-
    /// batch `warn!` for operators to triage.
    #[tokio::test]
    async fn room_policy_ok_none_classifies_as_not_live() {
        let room_policy = UnknownRoomPolicy::new();
        let room = bare("team@muc.example.com");
        let mut cache = std::collections::BTreeMap::<BareJid, RoomPolicyCacheEntry>::new();

        let entry = resolve_cached_room_policy(&room_policy, &room, &mut cache).await;
        assert_eq!(
            entry,
            RoomPolicyCacheEntry::Unknown(UnknownRoomPolicySource::NotLive),
            "Ok(None) must classify as NotLive, distinct from LookupError"
        );
    }

    /// Stage-split contract: the same hinted candidate MUST yield
    /// `Deliver` when evaluated at [`PushEvalStage::T0Emit`] (so the
    /// row gets persisted with its hint bits) and `Suppressed` at
    /// [`PushEvalStage::T1Drain`] (where the typed `suppressed_reason`
    /// audit fires). Without this split, hinted candidates would
    /// disappear at T0 with no audit trail.
    #[tokio::test]
    async fn evaluator_stage_split_defers_hint_suppressors_to_t1() {
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let recipient = bare("alice@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");
        let noping_candidate = NotificationCandidate::direct_message_with_hints(
            recipient.clone(),
            sender_jid.clone(),
            StanzaId::new("stage-split-noping", Jid::from(recipient.clone())),
            false,
            NotificationMessageHints::none().with_noping(true),
        )
        .expect("candidate");
        let activity_reader = noop_activity_reader();
        let eval_deps = eval_deps_for_test(&projection, &room_policy, &dnd_reader, activity_reader);
        let (mut room_policy_cache, mut dnd_cache, mut activity_cache) = fresh_eval_caches();
        let mut eval_caches = PushEvalCaches {
            room_policy: &mut room_policy_cache,
            dnd: &mut dnd_cache,
            activity: &mut activity_cache,
        };

        // T0Emit MUST NOT suppress on noping — the row must persist
        // so T1 records the audit.
        let t0 = evaluate_push_gate_at_dispatch(
            PushEvalStage::T0Emit,
            eval_deps,
            &noping_candidate,
            &mut eval_caches,
        )
        .await
        .expect("t0 eval");
        assert!(
            matches!(t0, T1PushDispatchOutcome::Deliver { .. }),
            "T0Emit must NOT suppress on message-frozen `<noping/>` so the candidate persists; got {t0:?}"
        );

        // T1Drain MUST suppress with the typed Xep0513Noping reason.
        let t1 = evaluate_push_gate_at_dispatch(
            PushEvalStage::T1Drain,
            eval_deps,
            &noping_candidate,
            &mut eval_caches,
        )
        .await
        .expect("t1 eval");
        assert!(
            matches!(
                t1,
                T1PushDispatchOutcome::Suppressed {
                    reason: SuppressedReason::Xep0513Noping
                }
            ),
            "T1Drain must suppress noping with the typed Xep0513Noping reason; got {t1:?}"
        );

        // #780: XEP-0444 reaction-only follows the same stage split —
        // T0 persists the row, T1 suppresses with the typed reason.
        let reaction_candidate = NotificationCandidate::direct_message_with_hints(
            recipient.clone(),
            "bob@example.com/web".parse().expect("full sender"),
            StanzaId::new("stage-split-reaction", Jid::from(recipient.clone())),
            false,
            NotificationMessageHints::none().with_reaction(true),
        )
        .expect("candidate");
        let t0 = evaluate_push_gate_at_dispatch(
            PushEvalStage::T0Emit,
            eval_deps,
            &reaction_candidate,
            &mut eval_caches,
        )
        .await
        .expect("t0 eval");
        assert!(
            matches!(t0, T1PushDispatchOutcome::Deliver { .. }),
            "T0Emit must NOT suppress on message-frozen reaction-only so the candidate persists; got {t0:?}"
        );
        let t1 = evaluate_push_gate_at_dispatch(
            PushEvalStage::T1Drain,
            eval_deps,
            &reaction_candidate,
            &mut eval_caches,
        )
        .await
        .expect("t1 eval");
        assert!(
            matches!(
                t1,
                T1PushDispatchOutcome::Suppressed {
                    reason: SuppressedReason::Xep0444Reaction
                }
            ),
            "T1Drain must suppress reaction-only with the typed Xep0444Reaction reason; got {t1:?}"
        );

        // Contrast: XEP-0334 storage hints are NOT push suppressors.
        // Per XEP-0334 §3/§8 they scope to message storage, not push
        // delivery, so a `<no-store/>` candidate delivers a (minimal)
        // push at both stages — the hint only strips the rich body.
        let no_store_candidate = NotificationCandidate::direct_message_with_hints(
            recipient.clone(),
            sender_jid,
            StanzaId::new("stage-split-no-store", Jid::from(recipient.clone())),
            false,
            NotificationMessageHints::none().with_xep0334(true, false),
        )
        .expect("candidate");
        let t0_no_store = evaluate_push_gate_at_dispatch(
            PushEvalStage::T0Emit,
            eval_deps,
            &no_store_candidate,
            &mut eval_caches,
        )
        .await
        .expect("t0 eval");
        assert!(matches!(t0_no_store, T1PushDispatchOutcome::Deliver { .. }));
        let t1_no_store = evaluate_push_gate_at_dispatch(
            PushEvalStage::T1Drain,
            eval_deps,
            &no_store_candidate,
            &mut eval_caches,
        )
        .await
        .expect("t1 eval");
        assert!(
            matches!(t1_no_store, T1PushDispatchOutcome::Deliver { .. }),
            "XEP-0334 <no-store/> must not suppress the push; got {t1_no_store:?}"
        );
    }

    // ---------------------------------------------------------------
    // Storage-preservation regressions for slice 2a suppressors
    // ---------------------------------------------------------------
    //
    // Contract: when push is suppressed at T0 (compliance gate) or T1
    // (audit gate) by ANY suppressor — XEP-0191 blocking, XEP-0492
    // `<never/>`/`<on-mention/>` miss, XEP-0513 `<noping/>`, XEP-0334
    // hints, or Waddle DnD — the suppressor only affects the XEP-0357
    // push fanout. The message MUST still be archived (XEP-0313 MAM),
    // projected into the recipient's XEP-0430 inbox, queued in
    // XEP-0160 offline storage when applicable, and delivered to
    // online resources per RFC 6121. None of those upstream writes
    // belong to the notification-outbox layer; the candidate
    // emission code path only writes to `notification_candidates`
    // and `notification_outbox`. The tests below pre-seed an
    // inbox-storage witness BEFORE the candidate emission and verify
    // the witness is byte-identical afterwards — proving the outbox
    // layer never rolls back or mutates upstream artifacts. By
    // symmetry, MAM and pending_delivery (likewise written upstream,
    // never by this layer) are preserved by the same invariant. The
    // websocket-integration test `xep0357_suppression_preserves_mam_inbox_and_audit`
    // in `server::routes::websocket::tests::messages` covers the
    // full upstream surface (MAM + inbox + pending_delivery) in one
    // wire-level shot for the dominant DM `<never/>` path.

    /// XEP-0492 `<never/>` is a compliance-required suppressor that
    /// runs at T0Emit: the candidate row MUST NOT be persisted (per
    /// the existing T0 contract in `enqueue_xep0357_notification_candidate_for_message`),
    /// but any upstream artifact (here: an inbox row the recipient
    /// already has for this conversation) MUST be untouched. The
    /// typed metric counter MUST tick once for the suppression audit.
    #[tokio::test]
    async fn xep0492_never_suppression_preserves_pending_delivery_and_audit_via_metric() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let projection = settings_projection().await;
        let room_policy = NoopRoomPolicy;
        let dnd_reader = NoopDndReader;
        let recipient = bare("alice@example.com");
        let sender = bare("bob@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");

        // Seed XEP-0430 inbox witness BEFORE candidate emission.
        let (inbox, witness) =
            seed_inbox_witness(&recipient, &sender, "archive-never-witness", 42, 3).await;

        // Recipient has explicitly muted this conversation.
        projection
            .upsert(&crate::notification_settings_projection::NotificationSettingsProjection {
                owner_bare_jid: recipient.clone(),
                conversation_jid: sender.clone(),
                conversation_kind:
                    crate::notification_settings_projection::ConversationKind::Direct,
                mode: waddle_xmpp::xep::NotificationLevel::Never,
                source:
                    crate::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
                source_item_jid: sender.clone(),
                updated_at_ms: 1,
                rich_payload_opt_in: false,
                source_version: 1,
            })
            .await
            .expect("seed never level");

        // Drive the T0 evaluator the same way
        // `enqueue_xep0357_notification_candidate_for_message` does.
        let candidate = NotificationCandidate::direct_message(
            recipient.clone(),
            sender_jid,
            StanzaId::new("never-t0", Jid::from(recipient.clone())),
            false,
        )
        .expect("candidate");
        let activity_reader = noop_activity_reader();
        let eval_deps = eval_deps_for_test(&projection, &room_policy, &dnd_reader, activity_reader);
        let (mut room_policy_cache, mut dnd_cache, mut activity_cache) = fresh_eval_caches();
        let mut eval_caches = PushEvalCaches {
            room_policy: &mut room_policy_cache,
            dnd: &mut dnd_cache,
            activity: &mut activity_cache,
        };
        let outcome = evaluate_push_gate_at_dispatch(
            PushEvalStage::T0Emit,
            eval_deps,
            &candidate,
            &mut eval_caches,
        )
        .await
        .expect("t0 eval");
        assert!(
            matches!(
                outcome,
                T1PushDispatchOutcome::Suppressed {
                    reason: SuppressedReason::Xep0492Never
                }
            ),
            "T0 MUST suppress <never/> with the typed Xep0492Never audit; got {outcome:?}"
        );
        // Mirror the T0 emission contract: tick the metric, do NOT
        // persist a candidate row.
        waddle_xmpp::telemetry::reliability::increment_push_suppressed(
            SuppressedReason::Xep0492Never.telemetry_reason(),
        );

        // Push surface invariants: no candidate row, no outbox job.
        let candidates = store.count_all_candidates().await.expect("count");
        assert_eq!(
            candidates, 0,
            "T0 <never/> MUST NOT persist a candidate row"
        );
        assert!(
            store.pending_outbox_jobs().await.expect("jobs").is_empty(),
            "T0 <never/> MUST NOT enqueue a job",
        );

        // Upstream-storage invariant: the inbox witness survives.
        assert_inbox_witness_unchanged(&inbox, &recipient, &witness).await;

        let rendered = waddle_xmpp::prometheus::render_metrics();
        assert!(
            rendered.contains("waddle_push_suppressed_total{reason=\"xep0492_never\"} 1"),
            "T0 suppression metric must tick exactly once; rendered={rendered}",
        );
    }

    /// XEP-0492 `<on-mention/>` for a non-mention DM is the second
    /// T0 compliance suppressor. Same upstream-preservation contract
    /// as `<never/>`: no candidate row, inbox witness intact.
    #[tokio::test]
    async fn xep0492_on_mention_miss_preserves_pending_delivery_for_non_mention_dm() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let projection = settings_projection().await;
        let room_policy = NoopRoomPolicy;
        let dnd_reader = NoopDndReader;
        let recipient = bare("alice@example.com");
        let sender = bare("bob@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");

        let (inbox, witness) =
            seed_inbox_witness(&recipient, &sender, "archive-on-mention-witness", 7, 1).await;

        projection
            .upsert(&crate::notification_settings_projection::NotificationSettingsProjection {
                owner_bare_jid: recipient.clone(),
                conversation_jid: sender.clone(),
                conversation_kind:
                    crate::notification_settings_projection::ConversationKind::Direct,
                mode: waddle_xmpp::xep::NotificationLevel::OnMention,
                source:
                    crate::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
                source_item_jid: sender.clone(),
                updated_at_ms: 1,
                rich_payload_opt_in: false,
                source_version: 1,
            })
            .await
            .expect("seed on-mention level");

        // `is_mention = false` matches the dispatch path for a plain
        // DM that does NOT name the recipient via XEP-0513.
        let candidate = NotificationCandidate::direct_message(
            recipient.clone(),
            sender_jid,
            StanzaId::new("on-mention-miss-t0", Jid::from(recipient.clone())),
            false,
        )
        .expect("candidate");
        let activity_reader = noop_activity_reader();
        let eval_deps = eval_deps_for_test(&projection, &room_policy, &dnd_reader, activity_reader);
        let (mut room_policy_cache, mut dnd_cache, mut activity_cache) = fresh_eval_caches();
        let mut eval_caches = PushEvalCaches {
            room_policy: &mut room_policy_cache,
            dnd: &mut dnd_cache,
            activity: &mut activity_cache,
        };
        let outcome = evaluate_push_gate_at_dispatch(
            PushEvalStage::T0Emit,
            eval_deps,
            &candidate,
            &mut eval_caches,
        )
        .await
        .expect("t0 eval");
        assert!(
            matches!(
                outcome,
                T1PushDispatchOutcome::Suppressed {
                    reason: SuppressedReason::Xep0492OnMentionMiss,
                }
            ),
            "T0 MUST suppress <on-mention/> miss with typed Xep0492OnMentionMiss; got {outcome:?}"
        );
        waddle_xmpp::telemetry::reliability::increment_push_suppressed(
            SuppressedReason::Xep0492OnMentionMiss.telemetry_reason(),
        );

        assert_eq!(
            store.count_all_candidates().await.expect("count"),
            0,
            "T0 <on-mention/> miss MUST NOT persist a candidate row",
        );
        assert!(
            store.pending_outbox_jobs().await.expect("jobs").is_empty(),
            "T0 <on-mention/> miss MUST NOT enqueue a job",
        );
        assert_inbox_witness_unchanged(&inbox, &recipient, &witness).await;

        let rendered = waddle_xmpp::prometheus::render_metrics();
        assert!(
            rendered.contains("waddle_push_suppressed_total{reason=\"xep0492_on_mention_miss\"} 1"),
            "metric counter for xep0492_on_mention_miss must increment; rendered={rendered}",
        );
    }

    /// Stage-split contract: at `PushEvalStage::T0Emit` the XEP-0513
    /// `<active/>` filter MUST NOT consult the activity reader.
    /// Exercises the stage split with a counting reader fixture.
    #[tokio::test]
    async fn t0_active_channel_mention_does_not_consult_activity_reader() {
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let counting = CountingActivityReader::new().await;

        let recipient = bare("alice@example.com");
        let room = bare("room@muc.example.com");
        let sender = bare("bob@example.com");
        let candidate =
            active_channel_mention_candidate_for(&recipient, &room, &sender, "t0-no-touch");

        let eval_deps = eval_deps_for_test(&projection, &room_policy, &dnd_reader, &counting);
        let (mut room_policy_cache, mut dnd_cache, mut activity_cache) = fresh_eval_caches();
        let mut eval_caches = PushEvalCaches {
            room_policy: &mut room_policy_cache,
            dnd: &mut dnd_cache,
            activity: &mut activity_cache,
        };

        let outcome = evaluate_push_gate_at_dispatch(
            PushEvalStage::T0Emit,
            eval_deps,
            &candidate,
            &mut eval_caches,
        )
        .await
        .expect("t0 eval");
        // T0Emit must NOT suppress on the XEP-0513 filter (skipped at
        // T0). Class falls through to the XEP-0492 evaluator with the
        // public-group `OnMention` default → mention bit on
        // `ActiveChannelMention` is `true`, so the gate Delivers.
        assert!(
            matches!(outcome, T1PushDispatchOutcome::Deliver { .. }),
            "T0Emit MUST NOT suppress with Xep0513ActiveMiss; got {outcome:?}"
        );
        assert_eq!(
            counting.call_count(),
            0,
            "T0Emit MUST NOT consult the activity reader",
        );

        // Same candidate at T1Drain DOES consult the reader.
        let outcome_t1 = evaluate_push_gate_at_dispatch(
            PushEvalStage::T1Drain,
            eval_deps,
            &candidate,
            &mut eval_caches,
        )
        .await
        .expect("t1 eval");
        assert!(
            matches!(
                outcome_t1,
                T1PushDispatchOutcome::Suppressed {
                    reason: SuppressedReason::Xep0513ActiveMiss
                }
            ),
            "T1Drain MUST suppress when activity is missing; got {outcome_t1:?}"
        );
        assert_eq!(
            counting.call_count(),
            1,
            "T1Drain MUST consult the activity reader exactly once",
        );
    }

    #[tokio::test]
    async fn publish_worker_applies_xep0191_to_groupchat_notifications() {
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let recipient = bare("alice@example.com");
        let room = bare("team@muc.example.com");
        let sender: Jid = "team@muc.example.com/bob"
            .parse()
            .expect("room occupant sender");
        blocking.set_blocklist_jids(recipient.clone(), vec![Jid::from(room.clone())]);
        let job = NotificationOutboxJob {
            job_id: NotificationOutboxJobId::from("groupchat-blocked-job".to_string()),
            recipient_bare_jid: recipient,
            push_service_jid: bare("push.example.com"),
            node: PushServiceNodeName::new("web-node").expect("node"),
            conversation_jid: room,
            sender_jid: sender.clone(),
            sender_jids: vec![sender],
            thread_id: NotificationThreadId::root(),
            class: NotificationClass::ChannelMention,
            message_count: 1,
            context: Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH).build(),
            rich_summary: RichSummary::minimal(),
            status: NotificationOutboxStatus::Queued,
            attempt_count: 0,
            policy_error_count: 0,
            claim_token: None,
        };

        assert!(
            xep0191_blocks_notification_job(&job, &blocking)
                .await
                .expect("block check"),
            "publish-time XEP-0191 checks must apply to groupchat notification classes"
        );
    }
}
