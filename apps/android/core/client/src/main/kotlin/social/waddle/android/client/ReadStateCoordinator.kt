package social.waddle.android.client

import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import social.waddle.android.client.XmppSessionManager.DisplayedTarget
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.store.SessionStores
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleMdsDisplayedEntry
import java.util.concurrent.ConcurrentHashMap

/**
 * XEP-0333 / XEP-0490 read state for the account: dispatches displayed
 * markers and MDS publishes, applies sibling-device cursors, and parks
 * reads that arrive while no session is live for replay on the next
 * ready session.
 */
internal class ReadStateCoordinator(
    private val activeSession: ActiveSession,
    private val stores: SessionStores,
    private val userPrefs: UserPrefs,
    private val emitEvent: (XmppEvent) -> Unit,
) {
    /** Serializes displayed dispatches (see [markConversationDisplayed]). */
    private val displayedMutex = Mutex()

    /** Reads parked while offline, replayed on the next ready session. */
    private val pendingDisplayed = ConcurrentHashMap<String, PendingDisplayed>()

    fun clearPending() {
        pendingDisplayed.clear()
    }

    /**
     * Mark the newest displayable message of a conversation as read:
     * sends the XEP-0333 `<displayed/>` (gated on the read-receipts
     * pref, and on `stanza_id_by == room` for MUCs / an explicit marker
     * request for DMs — web parity) and publishes the XEP-0490 MDS
     * cursor (independent of the pref) so sibling devices retire their
     * badges. Equality-deduped per conversation; safe to call on every
     * timeline change while the conversation is visible.
     *
     * Ordering matters: the local badge clears unconditionally (reading
     * offline is still reading), but the cursor dedupe is consumed ONLY
     * once a live client will carry the dispatch — otherwise an offline
     * read would permanently swallow the marker and the MDS publish.
     *
     * [explicitTarget] lets callers without a loaded timeline (the
     * notification mark-as-read action after process death) name the
     * message directly.
     */
    suspend fun markConversationDisplayed(
        conversationJid: String,
        isGroupchat: Boolean,
        explicitTarget: DisplayedTarget? = null,
    ): Unit = displayedMutex.withLock {
        // Serialized: unguarded concurrent dispatches (timeline
        // collector, visibility hooks, the notification receiver) could
        // interleave across the suspend points and let a STALE call's
        // terminal advance/publish land last, regressing the local
        // cursor and the published MDS node.
        val conversation = bareJid(conversationJid)
        val items = stores.timelineStore.timeline(conversation).value
        val ids = explicitTarget
            ?: newestDisplayedTarget(items, isGroupchat, conversation)
            ?: return
        // The on-screen path clears the badge unconditionally (looking
        // IS reading, even offline). Explicit targets (parked replays,
        // notification actions) must pass the staleness guard first —
        // a parked replay racing a fresh arrival would otherwise wipe
        // the badge for a message the user has never seen (the tap-time
        // clear already happened in the receiver).
        if (explicitTarget == null) {
            stores.unreadStore.clear(conversation)
            // Arm the inbox read-clear barrier at the same moment: a
            // racing XEP-0430 push still naming the read message must
            // not resurrect the badge the user is looking at.
            stores.inboxStore.markReadLocally(conversation)
        }
        val explicitExpectation = explicitTarget?.let {
            expectedCursorFor(items, conversation, it.markerId) ?: return
        }
        // Snapshot the cursor to CAS against below: applyMdsEntry runs
        // OUTSIDE displayedMutex and can advance the cursor to a newer
        // sibling value across our suspend points — an unconditional
        // advance would then regress it to our (older) marker and wedge
        // the sibling entry's CAS forever.
        val cursorBefore = stores.readCursorStore.cursor(conversation)
        if (cursorBefore == ids.markerId) return
        // The cursor (which dedupes future dispatches) is taken only
        // when every attempted send went through — a thrown/refused
        // dispatch stays retryable on the next timeline change.
        val dispatched = when (val result = activeSession.invoke { client ->
            val displayed = dispatchDisplayed(client, conversation, isGroupchat, ids)
            fireInboxMarkRead(client, conversation, threadId = null)
            displayed
        }) {
            ActiveSession.Invocation.NotConnected -> {
                // No live session: park the RESOLVED target and replay it on
                // the next ready (a notification tap during a reconnect gap
                // must not permanently drop the read receipt).
                pendingDisplayed[conversation] = PendingDisplayed(isGroupchat, ids)
                return
            }
            is ActiveSession.Invocation.Completed -> result.value
        }
        if (dispatched) {
            // CAS on both paths: a concurrent applyMdsEntry advance (it
            // does not hold displayedMutex) during our sends must never
            // be clobbered by our older marker.
            val expected =
                if (explicitTarget != null) explicitExpectation?.cursor else cursorBefore
            stores.readCursorStore.compareAndAdvance(conversation, expected, ids.markerId)
        }
    }

    /**
     * Server-side inbox mark-read for one conversation (optionally
     * one room thread): arms the local read-clear barrier, then tells
     * the server. No live session → the local barrier still applies
     * and the next hydrate restores whatever the server last knew.
     */
    suspend fun markInboxRead(conversationJid: String, threadId: String? = null) {
        val conversation = bareJid(conversationJid)
        if (threadId == null) stores.unreadStore.clear(conversation)
        when (activeSession.invoke { client ->
            fireInboxMarkRead(client, conversation, threadId)
        }) {
            ActiveSession.Invocation.NotConnected -> stores.inboxStore.markReadLocally(conversation, threadId)
            is ActiveSession.Invocation.Completed -> Unit
        }
    }

    private suspend fun fireInboxMarkRead(
        client: WaddleClientInterface,
        conversation: String,
        threadId: String?,
    ) {
        stores.inboxStore.markReadLocally(conversation, threadId)
        runCatching { client.markInboxRead(conversation, threadId) }
    }

    /**
     * The newest on-screen read target. Only feed-visible rows count: a
     * thread reply the user has never opened must not advance the read
     * cursor.
     */
    private fun newestDisplayedTarget(
        items: List<TimelineItem>,
        isGroupchat: Boolean,
        conversation: String,
    ): DisplayedTarget? {
        val target = items.lastOrNull {
            !it.isMine && it.tombstone == null && it.isFeedVisible
        } ?: return null
        return displayedTargetOf(target, isGroupchat, conversation)
    }

    /**
     * Staleness guard for explicit targets, which can be stale: never
     * move the cursor BACKWARDS in timeline order, and when the target
     * cannot be ordered against an EXISTING cursor (not in the loaded
     * timeline) treat it as stale rather than risk regressing the
     * published MDS node. `null` = stale, do not dispatch; otherwise
     * carries the cursor value the terminal advance must CAS against.
     */
    private fun expectedCursorFor(
        items: List<TimelineItem>,
        conversation: String,
        markerId: String,
    ): CursorExpectation? {
        val targetIndex = items.indexOfLast { markerId in it.identityIds }
        val current = stores.readCursorStore.cursor(conversation)
        val currentIndex = current?.let { c -> items.indexOfLast { c in it.identityIds } } ?: -1
        if (targetIndex in 0..currentIndex) return null
        if (targetIndex < 0 && current != null) return null
        return CursorExpectation(current)
    }

    /**
     * Send the XEP-0333 marker (when allowed) and the XEP-0490 publish.
     * The MDS pair is authority-verified at construction (room-assigned
     * for MUCs, own-server-assigned for DMs — XEP-0490 §3); a row
     * without a trusted pair simply skips the publish. Returns true
     * only when every attempted send went through.
     */
    private suspend fun dispatchDisplayed(
        client: WaddleClientInterface,
        conversation: String,
        isGroupchat: Boolean,
        ids: DisplayedTarget,
    ): Boolean {
        var failed = false
        if (markerAllowed(ids, isGroupchat, conversation)) {
            val sent = runCatching {
                client.sendDisplayed(conversation, ids.markerId, isGroupchat)
            }.getOrDefault(false)
            failed = failed || !sent
        }
        if (ids.stanzaId != null && ids.stanzaIdBy != null && supportsMdsPublish(client)) {
            val published = runCatching {
                client.publishMdsDisplayed(conversation, ids.stanzaId, ids.stanzaIdBy)
            }.getOrDefault(false)
            failed = failed || !published
        }
        return !failed
    }

    /**
     * XEP-0333 gate: the read-receipts pref, plus `stanza_id_by ==
     * room` for MUCs / an explicit marker request for DMs (web parity).
     */
    private suspend fun markerAllowed(
        ids: DisplayedTarget,
        isGroupchat: Boolean,
        conversation: String,
    ): Boolean = when {
        !readReceiptsEnabled() -> false
        isGroupchat -> ids.stanzaIdBy == conversation && ids.markerId == ids.stanzaId
        else -> ids.markerRequested
    }

    /**
     * Replay reads parked while no session was live (see above).
     * Atomic per-key removal — a snapshot-then-clear would drop a
     * dispatch parked concurrently (notification receiver) between the
     * snapshot and the clear.
     */
    suspend fun drainPendingDisplayed() {
        while (pendingDisplayed.isNotEmpty()) {
            // A dispatch below re-parks when the session dropped mid-
            // drain; bail instead of spinning on it.
            if (!activeSession.hasActiveClient()) return
            for (conversation in pendingDisplayed.keys) {
                val pending = pendingDisplayed.remove(conversation) ?: continue
                markConversationDisplayed(conversation, pending.isGroupchat, pending.target)
            }
        }
    }

    private suspend fun readReceiptsEnabled(): Boolean =
        runCatching { userPrefs.readReceiptsEnabled.first() }.getOrDefault(true)

    /**
     * XEP-0490 §3 publish-options probe, once per session attempt
     * (web parity); a failed probe retries on the next dispatch.
     */
    private suspend fun supportsMdsPublish(client: WaddleClientInterface): Boolean {
        activeSession.mdsPublishSupported?.let { return it }
        val supported = runCatching { client.supportsMdsPublishOptions() }.getOrNull() ?: return false
        activeSession.mdsPublishSupported = supported
        return supported
    }

    /**
     * The ids a displayed dispatch needs, per XEP-0333 id-class rules:
     * a MUC marker targets the ROOM-assigned XEP-0359 stanza id, but a
     * 1:1 marker must carry the AUTHOR-assigned id — the local archive
     * stanza id was stamped by our own server and the peer never saw
     * it. The stanza-id pair rides along strictly for the MDS publish.
     */
    private fun displayedTargetOf(
        item: TimelineItem,
        isGroupchat: Boolean,
        conversation: String,
    ): DisplayedTarget? {
        // Authority-scanned XEP-0359 ids (never the first element, which
        // is sender-controlled): the marker/MDS pair for a MUC is the
        // ROOM-assigned stanza id; the DM MDS pair is the id assigned by
        // the user's OWN server (XEP-0490 §3) — a peer's archive stamp
        // or an injected foreign id must never be republished as the
        // account's read cursor.
        val own = activeSession.ownBareJid
        val mdsPair = if (isGroupchat) {
            item.assignedStanzaId(conversation)
        } else {
            own?.let { item.assignedStanzaId(it, it.substringAfter('@')) }
        }
        val markerId = if (isGroupchat) {
            mdsPair?.id
        } else {
            item.originId ?: item.messageId
        } ?: return null
        val markerRequested = when (val source = item.source) {
            is TimelineSource.Live -> source.message.displayedMarkerRequested
            // The archive does not carry `<request/>`; archived DM rows
            // sync via MDS only.
            is TimelineSource.Archived -> false
        }
        return DisplayedTarget(
            markerId = markerId,
            stanzaId = mdsPair?.id,
            stanzaIdBy = mdsPair?.by,
            markerRequested = markerRequested,
        )
    }

    /**
     * A displayed cursor from ANOTHER device (MDS fetch or live PEP
     * event): advance the local cursor and recompute the badge from the
     * loaded timeline. An entry whose target is not loaded is ignored —
     * the conversation's next open recomputes from scratch anyway.
     */
    fun applyMdsEntry(entry: WaddleMdsDisplayedEntry) {
        val conversation = bareJid(entry.chatId)
        val items = stores.timelineStore.timeline(conversation).value
        val index = items.indexOfLast { entry.stanzaId in it.identityIds }
        if (index < 0) return
        val current = stores.readCursorStore.cursor(conversation)
        val currentIndex = current?.let { c -> items.indexOfLast { c in it.identityIds } } ?: -1
        if (currentIndex >= index) return
        // Compare-and-advance: a local displayed dispatch (UI scope) can
        // move the cursor between the read above and this write — a
        // stale sibling entry must not regress it and resurrect a badge
        // the user just cleared.
        val advanced = stores.readCursorStore
            .compareAndAdvance(conversation, expected = current, stanzaId = entry.stanzaId)
        if (!advanced) return
        // Feed-visible rows only: hidden thread replies never count.
        val unread = items.subList(index + 1, items.size).count {
            !it.isMine && it.tombstone == null && it.isFeedVisible
        }
        // Atomic recompute-unless-active: the on-screen conversation's
        // badge is owned by the local read path.
        stores.unreadStore.setUnlessActive(conversation, unread)
        // A sibling device read everything: the stale notification in
        // the shade must retire too (the notifier owns that side).
        if (unread == 0) emitEvent(XmppEvent.ReadSynced(conversation))
    }

    /**
     * XEP-0490 connect bootstrap: seed cursors from the account's MDS
     * node, then subscribe for live sibling-device updates. Fetched
     * entries are INJECTED into the event stream rather than applied
     * here — the unread recompute must serialize with live-message
     * increments on the single event consumer, or a concurrent arrival
     * could have its badge erased by a stale recompute.
     */
    suspend fun bootstrapMdsDisplayed() {
        val entries = try {
            when (val result = activeSession.invoke { it.fetchMdsDisplayed() }) {
                ActiveSession.Invocation.NotConnected -> return
                is ActiveSession.Invocation.Completed -> result.value
            }
        } catch (cancellation: kotlinx.coroutines.CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return
        }
        entries.let { entries ->
            if (entries.isNotEmpty()) {
                // Handshake: the pipeline continues (pending-displayed
                // drain!) only after the consumer APPLIED the fetched
                // cursors — a parked read replayed before them could
                // publish an older MDS cursor over a sibling's newer one.
                val applied = Job()
                activeSession.bridge?.submit(XmppEvent.MdsEntries(entries, applied))
                // Bare join: swallowing a cancellation here (attempt
                // teardown drops unconsumed events) would resume a
                // cancelled pipeline into the drain.
                applied.join()
            }
        }
        try {
            activeSession.invoke { it.subscribeMdsDisplayed() }
        } catch (cancellation: kotlinx.coroutines.CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            // Best-effort subscription; the next ready bootstrap retries.
        }
    }

    /** A displayed dispatch waiting for a live session. */
    private data class PendingDisplayed(
        val isGroupchat: Boolean,
        val target: DisplayedTarget,
    )

    /** A non-stale explicit dispatch: the cursor to CAS against. */
    private data class CursorExpectation(val cursor: String?)
}
