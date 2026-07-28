package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.store.MessageMutation
import social.waddle.android.client.store.SessionStores
import social.waddle.client.ffi.WaddleChatState
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleLinkPreviewLookup
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleNotifyMode
import social.waddle.client.ffi.WaddleSendMessageOutcome
import social.waddle.client.ffi.WaddleSetDmNotificationModeOutcome
import social.waddle.client.ffi.WaddleSetRoomNotificationModeOutcome
import social.waddle.client.ffi.WaddleThreadTarget
import social.waddle.client.ffi.WaddleUploadSlot

/**
 * Per-conversation UI passthroughs (M1): the app module never touches
 * the FFI client directly — these forward to the live attempt's client
 * and keep the stores/prefs consistent. Each returns a "not connected"
 * shape when no session is ready instead of throwing.
 */
internal class ConversationVerbs(
    private val activeSession: ActiveSession,
    private val stores: SessionStores,
    private val sessionPrefs: SessionPrefs,
) {
    /** Serializes reaction send+rollback pairs (see [toggleReaction]). */
    private val reactionMutex = Mutex()

    /**
     * Join a MUC room on the live connection; on success the room is
     * marked joined in the room store and the joined set is persisted.
     *
     * With no live session yet (e.g. a channel tapped during the 1-3s
     * connect window — the shell is interactive before `SessionReady`)
     * the join INTENT is still persisted so the rejoin on the next
     * ready session fires it; silently dropping it left a live channel
     * that never received messages.
     */
    suspend fun joinRoom(roomJid: String, nick: String): VerbResult {
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotReady
        val joined = try {
            when (val result = activeSession.invokeIfCurrent(lease) { it.joinRoom(roomJid, nick) }) {
                ActiveSession.LeaseInvocation.Stale,
                ActiveSession.LeaseInvocation.NotConnected,
                -> false
                is ActiveSession.LeaseInvocation.Completed -> true
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return VerbResult.Rejected
        }
        if (!joined) {
            if (!activeSession.applyIfCurrent(lease) {
                    stores.roomStore.markJoined(roomJid)
                    persistQuietly { sessionPrefs.setJoinedRooms(stores.roomStore.joinedRooms.value) }
                }
            ) return VerbResult.NotConnected
            return VerbResult.NotConnected
        }
        if (!activeSession.applyIfCurrent(lease) {
                stores.roomStore.markJoined(roomJid)
                persistQuietly { sessionPrefs.setJoinedRooms(stores.roomStore.joinedRooms.value) }
            }
        ) return VerbResult.NotConnected
        return VerbResult.Ok
    }

    /**
     * Fetch a MAM page for a room and fan it into the timeline store
     * (dedupe by stanza id keeps replays collapsed). `null` when no
     * session is ready or the query failed.
     */
    suspend fun fetchRoomHistory(roomJid: String, maxMessages: UInt, beforeId: String?): WaddleMamPage? {
        val lease = activeSession.captureOwnerLease() ?: return null
        return fetchRoomHistory(lease, roomJid, maxMessages, beforeId)
    }

    internal suspend fun fetchRoomHistory(
        lease: ActiveSession.OwnerLease,
        roomJid: String,
        maxMessages: UInt,
        beforeId: String?,
    ): WaddleMamPage? = fetchHistory(lease) { client -> client.fetchRoomHistory(roomJid, maxMessages, beforeId) }

    /** DM twin of [fetchRoomHistory]. */
    suspend fun fetchDmHistory(peerJid: String, maxMessages: UInt, beforeId: String?): WaddleMamPage? {
        val lease = activeSession.captureOwnerLease() ?: return null
        return fetchDmHistory(lease, peerJid, maxMessages, beforeId)
    }

    internal suspend fun fetchDmHistory(
        lease: ActiveSession.OwnerLease,
        peerJid: String,
        maxMessages: UInt,
        beforeId: String?,
    ): WaddleMamPage? = fetchHistory(lease) { client -> client.fetchDmHistory(peerJid, maxMessages, beforeId) }

    /**
     * MAM full-text search over a room's archive (XEP-0313 with the
     * full-text `query=` form field). Unlike [fetchRoomHistory] the
     * results are ephemeral and never fan into the timeline store — a
     * search page is a disjoint slice of the archive and splicing it
     * into the conversation would fabricate adjacency. `null` when no
     * session is ready or the query failed.
     */
    suspend fun searchRoomHistory(roomJid: String, query: String, maxResults: UInt): WaddleMamPage? =
        activeSession.captureOwnerLease()?.let { lease ->
            activeSession.fetchIfCurrent(lease) { client -> client.searchRoomHistory(roomJid, query, maxResults) }
        }

    /** DM twin of [searchRoomHistory]. */
    suspend fun searchDmHistory(peerJid: String, query: String, maxResults: UInt): WaddleMamPage? =
        activeSession.captureOwnerLease()?.let { lease ->
            activeSession.fetchIfCurrent(lease) { client -> client.searchDmHistory(peerJid, query, maxResults) }
        }

    /**
     * XEP-0363: request an upload slot from the account's upload
     * service (discovered once per attempt). `null` when offline, no
     * service exists, or the service refused (e.g. size over quota).
     */
    suspend fun requestUploadSlot(
        filename: String,
        sizeBytes: ULong,
        contentType: String,
    ): WaddleUploadSlot? {
        val lease = activeSession.captureOwnerLease() ?: return null
        return try {
            when (val result = activeSession.invokeIfCurrent(
                lease,
                { client ->
                    val service = activeSession.uploadService
                        ?: client.discoverUploadService()?.also { activeSession.uploadService = it }
                        ?: return@invokeIfCurrent null
                    client.requestUploadSlot(service, filename, sizeBytes, contentType)
                },
            )) {
                ActiveSession.LeaseInvocation.Stale,
                ActiveSession.LeaseInvocation.NotConnected,
                -> null
                is ActiveSession.LeaseInvocation.Completed -> result.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            null
        }
    }

    /**
     * XEP-0444 toggle: flip [emoji] in the account's CURRENT reaction
     * set for a message and send the complete replacement set (empty =
     * clear), applied optimistically — a DM send never echoes back to
     * this client. The current set is resolved INSIDE the mutex: a
     * caller-computed set would read a stale base whenever a prior
     * send still holds the lock, and the full-set replace semantics
     * would silently erase the queued toggle.
     */
    suspend fun toggleReaction(
        conversationJid: String,
        isGroupchat: Boolean,
        targetStanzaId: String,
        emoji: String,
    ): VerbResult = reactionMutex.withLock {
        // NOTE (deferred, needs an FFI signature change): the reaction
        // stanza does not yet echo the target's XEP-0201 <thread/> like
        // the web client does — send_reaction takes no options today.
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotReady
        val sender = ownMutationSender(conversationJid, isGroupchat, lease) ?: return VerbResult.NotReady
        val base = ownReactionSet(conversationJid, targetStanzaId) ?: emptyList()
        val next = if (emoji in base) base - emoji else base + emoji
        if (!activeSession.applyIfCurrent(lease) {
                applyOwnReaction(conversationJid, isGroupchat, sender, targetStanzaId, next)
            }
        ) return VerbResult.NotConnected
        var result: VerbResult = VerbResult.Rejected
        try {
            result = activeSession.verbCallIfCurrent(lease) {
                it.sendReaction(bareJid(conversationJid), targetStanzaId, next, isGroupchat)
            }
        } finally {
            // Also runs on cancellation (screen closed mid-send): the
            // optimistic apply must never outlive a send that did not
            // happen — in a DM nothing on the wire would ever correct
            // the phantom chip. Owner-gated: a rollback racing logout
            // must not park pre-logout state into the next session.
            if (result != VerbResult.Ok) activeSession.applyIfCurrent(lease) {
                applyOwnReaction(conversationJid, isGroupchat, sender, targetStanzaId, base)
            }
        }
        return result
    }

    /** XEP-0308: replace an own message's body; applies locally on a
     *  successful send (no DM echo). [threadId] repeats the corrected
     *  message's XEP-0201 `<thread/>` (web parity) so the edit stays in
     *  its thread. */
    suspend fun sendCorrection(
        conversationJid: String,
        isGroupchat: Boolean,
        targetId: String,
        newBody: String,
        threadId: String? = null,
    ): VerbResult {
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotReady
        val sender = ownMutationSender(conversationJid, isGroupchat, lease) ?: return VerbResult.NotReady
        val options = threadId?.let {
            sendOptionsFor(newClientStanzaId()).copy(thread = WaddleThreadTarget(id = it, parent = null))
        }
        val outcome = try {
            when (val result = activeSession.invokeIfCurrent(
                lease,
                { it.sendCorrection(bareJid(conversationJid), targetId, newBody, isGroupchat, options) },
            )) {
                ActiveSession.LeaseInvocation.Stale,
                ActiveSession.LeaseInvocation.NotConnected,
                -> return VerbResult.NotConnected
                is ActiveSession.LeaseInvocation.Completed -> result.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return VerbResult.Rejected
        }
        if (outcome !is WaddleSendMessageOutcome.Sent) return VerbResult.Rejected
        // DM only: `Sent` means stream-accepted, and a room can still
        // reject the correction — MUC state waits for the reflection
        // (web parity). A DM has no reflection to wait for. Owner-gated
        // so a completion racing logout cannot park stale state.
        if (!isGroupchat && !activeSession.applyIfCurrent(lease) {
                stores.timelineStore.applyLocalMutation(
                conversationJid,
                MessageMutation.Correction(targetId = targetId, from = sender, newBody = newBody),
                isGroupchat,
                )
            }
        ) return VerbResult.NotConnected
        return VerbResult.Ok
    }

    /** XEP-0424: retract an own message; tombstones locally on success. */
    suspend fun sendRetraction(
        conversationJid: String,
        isGroupchat: Boolean,
        targetStanzaId: String,
    ): VerbResult {
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotReady
        val sender = ownMutationSender(conversationJid, isGroupchat, lease) ?: return VerbResult.NotReady
        val result = activeSession.verbCallIfCurrent(lease) {
            it.sendRetraction(bareJid(conversationJid), targetStanzaId, isGroupchat)
        }
        // DM only (see sendCorrection): a room rejection after stream
        // accept would leave an irreversible local tombstone; the MUC
        // reflection drives room state instead. Owner-gated like the
        // reaction rollback.
        if (result == VerbResult.Ok && !isGroupchat && !activeSession.applyIfCurrent(lease) {
                stores.timelineStore.applyLocalMutation(
                conversationJid,
                MessageMutation.Retraction(targetId = targetStanzaId, from = sender),
                isGroupchat,
                )
            }
        ) return VerbResult.NotConnected
        return result
    }

    /**
     * XEP-0425: ask the room to moderate (remove) another user's
     * message. Moderator/admin-gated server-side — a `forbidden`
     * reply surfaces as [VerbResult.Rejected] plus an `Error` event.
     * No optimistic tombstone: the room broadcasts the §4.3 retraction
     * to every occupant (including this one) on success.
     */
    suspend fun sendModeration(roomJid: String, targetStanzaId: String, reason: String?): VerbResult {
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotReady
        return activeSession.verbCallIfCurrent(lease) { client ->
            client.sendModeration(bareJid(roomJid), targetStanzaId, reason)
        }
    }

    /**
     * `urn:waddle:pin:0` room pin/unpin. No optimistic pin-set write —
     * the room broadcasts a `<pin-event/>` that lands in the pin store
     * (and a forbidden reply for non-admins surfaces via `on_error`).
     */
    suspend fun pinRoomMessage(roomJid: String, targetStanzaId: String, pin: Boolean): VerbResult {
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotReady
        return activeSession.verbCallIfCurrent(lease) { client ->
            if (pin) {
                client.pinMessage(bareJid(roomJid), targetStanzaId)
            } else {
                client.unpinMessage(bareJid(roomJid), targetStanzaId)
            }
        }
    }

    /**
     * `urn:waddle:link-preview:0` composer lookup: resolve [url] into a
     * send-time token scoped to [scopeJid] (DM peer or room bare JID).
     * `null` when no session is live or the transport broke — the send
     * proceeds without a preview, never blocks on one.
     */
    suspend fun lookupLinkPreview(url: String, scopeJid: String): WaddleLinkPreviewLookup? =
        activeSession.captureOwnerLease()?.let { lease ->
            activeSession.fetchIfCurrent(lease) { it.lookupLinkPreview(url, bareJid(scopeJid)) }
        }

    /**
     * Seed the pin store with the room's current pin list (room open).
     * The snapshot is injected into the serialized event stream —
     * applying it here would race live pin events and clobber updates
     * that arrived while the fetch was in flight.
     */
    suspend fun refreshRoomPins(roomJid: String) {
        val lease = activeSession.captureOwnerLease() ?: return
        val room = bareJid(roomJid)
        val fetchedAtVersion = stores.pinStore.eventVersion(room)
        val entries = try {
            when (val result = activeSession.invokeIfCurrent(lease) { it.fetchRoomPins(room) }) {
                ActiveSession.LeaseInvocation.Stale,
                ActiveSession.LeaseInvocation.NotConnected,
                -> return
                is ActiveSession.LeaseInvocation.Completed -> result.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return
        }
        activeSession.applyIfCurrent(lease) {
            activeSession.bridge?.submit(XmppEvent.RoomPins(room, entries, fetchedAtVersion))
        }
    }

    /**
     * XEP-0492: set the per-room notification mode by merging into the
     * room's XEP-0402 bookmark. The current rich-payload opt-in from
     * the store is repeated so the merge never clobbers it; the store
     * reconciles from the returned item on success (no refetch).
     */
    suspend fun setRoomNotificationMode(
        roomJid: String,
        mode: WaddleNotifyMode,
        name: String? = null,
    ): NotifySettingsResult {
        val lease = activeSession.captureOwnerLease() ?: return NotifySettingsResult.NotConnected
        val room = bareJid(roomJid)
        val outcome = try {
            when (val result = activeSession.invokeIfCurrent(
                lease,
                {
                    it.setRoomNotificationMode(
                        room,
                        mode,
                        name,
                        stores.notifySettingsStore.richPayloadOptIn(room),
                    )
                },
            )) {
                ActiveSession.LeaseInvocation.Stale,
                ActiveSession.LeaseInvocation.NotConnected,
                -> return NotifySettingsResult.NotConnected
                is ActiveSession.LeaseInvocation.Completed -> result.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return NotifySettingsResult.Rejected
        }
        return when (outcome) {
            is WaddleSetRoomNotificationModeOutcome.Ok -> {
                if (activeSession.applyIfCurrent(lease) {
                        stores.notifySettingsStore.applyRoomUpdate(outcome.item)
                    }
                ) NotifySettingsResult.Ok else NotifySettingsResult.NotConnected
            }
            WaddleSetRoomNotificationModeOutcome.NodeConfigMismatch -> NotifySettingsResult.NodeConfigMismatch
            WaddleSetRoomNotificationModeOutcome.Error -> NotifySettingsResult.Rejected
        }
    }

    /**
     * XEP-0492 DM twin of [setRoomNotificationMode], over the sparse
     * `urn:waddle:dm-bookmarks:0` carrier: returning the DM to the §3
     * direct-chat default retracts its item (`Removed`), which the
     * store reconciles by dropping the entry.
     */
    suspend fun setDmNotificationMode(peerJid: String, mode: WaddleNotifyMode): NotifySettingsResult {
        val lease = activeSession.captureOwnerLease() ?: return NotifySettingsResult.NotConnected
        val peer = bareJid(peerJid)
        val outcome = try {
            when (val result = activeSession.invokeIfCurrent(
                lease,
                { it.setDmNotificationMode(peer, mode, stores.notifySettingsStore.richPayloadOptIn(peer)) },
            )) {
                ActiveSession.LeaseInvocation.Stale,
                ActiveSession.LeaseInvocation.NotConnected,
                -> return NotifySettingsResult.NotConnected
                is ActiveSession.LeaseInvocation.Completed -> result.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return NotifySettingsResult.Rejected
        }
        return when (outcome) {
            is WaddleSetDmNotificationModeOutcome.Ok -> {
                if (activeSession.applyIfCurrent(lease) {
                        stores.notifySettingsStore.applyDmUpdate(outcome.item)
                    }
                ) NotifySettingsResult.Ok else NotifySettingsResult.NotConnected
            }
            is WaddleSetDmNotificationModeOutcome.Removed -> {
                if (activeSession.applyIfCurrent(lease) {
                        stores.notifySettingsStore.applyDmRemoved(outcome.jid)
                    }
                ) NotifySettingsResult.Ok else NotifySettingsResult.NotConnected
            }
            WaddleSetDmNotificationModeOutcome.NodeConfigMismatch -> NotifySettingsResult.NodeConfigMismatch
            WaddleSetDmNotificationModeOutcome.Error -> NotifySettingsResult.Rejected
        }
    }

    /**
     * XEP-0085 typing notification: best-effort and live-session-only —
     * a stale typing state must never replay from a queue, so a
     * disconnected send is simply dropped (web parity).
     */
    suspend fun sendChatState(
        conversationJid: String,
        isGroupchat: Boolean,
        state: WaddleChatState,
    ): VerbResult {
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotReady
        return activeSession.verbCallIfCurrent(lease) {
            it.sendChatState(bareJid(conversationJid), state, isGroupchat)
        }
    }

    /** The account's current reaction set on a row, from the store. */
    private fun ownReactionSet(conversationJid: String, targetId: String): List<String>? {
        val row = stores.timelineStore.timeline(bareJid(conversationJid)).value
            .firstOrNull { targetId in it.identityIds } ?: return null
        return row.reactions.filter { it.mine }.map { it.emoji }
    }

    private fun applyOwnReaction(
        conversationJid: String,
        isGroupchat: Boolean,
        sender: String,
        targetStanzaId: String,
        emojis: List<String>,
    ) {
        stores.timelineStore.applyLocalMutation(
            conversationJid,
            MessageMutation.Reaction(
                targetId = targetStanzaId,
                from = sender,
                senderKey = sender,
                mine = true,
                emojis = emojis,
            ),
            isGroupchat,
        )
    }

    /**
     * The account's mutation identity in a conversation: the occupant
     * JID (room/nick) in a MUC, the bare account JID in 1:1 — matching
     * how [conversationKeyOf] classifies own incoming copies.
     */
    private fun ownMutationSender(
        conversationJid: String,
        isGroupchat: Boolean,
        lease: ActiveSession.OwnerLease,
    ): String? {
        val own = lease.ownerBareJid
        return if (isGroupchat) {
            "${bareJid(conversationJid)}/${own.substringBefore('@')}"
        } else {
            own
        }
    }

    private suspend fun fetchHistory(
        lease: ActiveSession.OwnerLease,
        fetch: suspend (WaddleClientInterface) -> WaddleMamPage,
    ): WaddleMamPage? {
        val page = activeSession.fetchIfCurrent(lease, fetch) ?: return null
        // Per-message guard: one malformed archived stanza must not kill
        // the caller's paging coroutine (and crash-loop on every reopen
        // of the conversation, since the archive re-serves it).
        page.messages.forEach { message ->
            activeSession.applyIfCurrent(lease) {
                runCatching { stores.timelineStore.onArchivedMessage(message) }
            }
        }
        return page
    }
}
