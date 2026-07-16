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
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleSendMessageOutcome
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
    suspend fun joinRoom(roomJid: String, nick: String): Boolean {
        val client = activeSession.client
        if (client == null) {
            stores.roomStore.markJoined(roomJid)
            persistQuietly { sessionPrefs.setJoinedRooms(stores.roomStore.joinedRooms.value) }
            return false
        }
        try {
            client.joinRoom(roomJid, nick)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return false
        }
        stores.roomStore.markJoined(roomJid)
        persistQuietly { sessionPrefs.setJoinedRooms(stores.roomStore.joinedRooms.value) }
        return true
    }

    /**
     * Fetch a MAM page for a room and fan it into the timeline store
     * (dedupe by stanza id keeps replays collapsed). `null` when no
     * session is ready or the query failed.
     */
    suspend fun fetchRoomHistory(roomJid: String, maxMessages: UInt, beforeId: String?): WaddleMamPage? =
        fetchHistory { client -> client.fetchRoomHistory(roomJid, maxMessages, beforeId) }

    /** DM twin of [fetchRoomHistory]. */
    suspend fun fetchDmHistory(peerJid: String, maxMessages: UInt, beforeId: String?): WaddleMamPage? =
        fetchHistory { client -> client.fetchDmHistory(peerJid, maxMessages, beforeId) }

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
        val client = activeSession.client ?: return null
        val service = activeSession.uploadService ?: run {
            val discovered = runCatching { client.discoverUploadService() }.getOrNull() ?: return null
            activeSession.uploadService = discovered
            discovered
        }
        return try {
            client.requestUploadSlot(service, filename, sizeBytes, contentType)
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
    ): Boolean = reactionMutex.withLock {
        // NOTE (deferred, needs an FFI signature change): the reaction
        // stanza does not yet echo the target's XEP-0201 <thread/> like
        // the web client does — send_reaction takes no options today.
        val owner = activeSession.ownBareJid ?: return false
        val sender = ownMutationSender(conversationJid, isGroupchat) ?: return false
        val base = ownReactionSet(conversationJid, targetStanzaId) ?: emptyList()
        val next = if (emoji in base) base - emoji else base + emoji
        applyOwnReaction(conversationJid, isGroupchat, sender, targetStanzaId, next)
        var sent = false
        try {
            sent = activeSession.clientCall {
                it.sendReaction(bareJid(conversationJid), targetStanzaId, next, isGroupchat)
            }
        } finally {
            // Also runs on cancellation (screen closed mid-send): the
            // optimistic apply must never outlive a send that did not
            // happen — in a DM nothing on the wire would ever correct
            // the phantom chip. Owner-gated: a rollback racing logout
            // must not park pre-logout state into the next session.
            if (!sent && activeSession.ownBareJid == owner) {
                applyOwnReaction(conversationJid, isGroupchat, sender, targetStanzaId, base)
            }
        }
        return sent
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
    ): Boolean {
        val client = activeSession.client ?: return false
        val sender = ownMutationSender(conversationJid, isGroupchat) ?: return false
        val options = threadId?.let {
            sendOptionsFor(newClientStanzaId()).copy(thread = WaddleThreadTarget(id = it, parent = null))
        }
        val outcome = try {
            client.sendCorrection(bareJid(conversationJid), targetId, newBody, isGroupchat, options)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return false
        }
        if (outcome !is WaddleSendMessageOutcome.Sent) return false
        // DM only: `Sent` means stream-accepted, and a room can still
        // reject the correction — MUC state waits for the reflection
        // (web parity). A DM has no reflection to wait for. Owner-gated
        // so a completion racing logout cannot park stale state.
        if (!isGroupchat && activeSession.ownBareJid != null) {
            stores.timelineStore.applyLocalMutation(
                conversationJid,
                MessageMutation.Correction(targetId = targetId, from = sender, newBody = newBody),
                isGroupchat,
            )
        }
        return true
    }

    /** XEP-0424: retract an own message; tombstones locally on success. */
    suspend fun sendRetraction(
        conversationJid: String,
        isGroupchat: Boolean,
        targetStanzaId: String,
    ): Boolean {
        val sender = ownMutationSender(conversationJid, isGroupchat) ?: return false
        val sent = activeSession.clientCall {
            it.sendRetraction(bareJid(conversationJid), targetStanzaId, isGroupchat)
        }
        // DM only (see sendCorrection): a room rejection after stream
        // accept would leave an irreversible local tombstone; the MUC
        // reflection drives room state instead. Owner-gated like the
        // reaction rollback.
        if (sent && !isGroupchat && activeSession.ownBareJid != null) {
            stores.timelineStore.applyLocalMutation(
                conversationJid,
                MessageMutation.Retraction(targetId = targetStanzaId, from = sender),
                isGroupchat,
            )
        }
        return sent
    }

    /**
     * `urn:waddle:pin:0` room pin/unpin. No optimistic pin-set write —
     * the room broadcasts a `<pin-event/>` that lands in the pin store
     * (and a forbidden reply for non-admins surfaces via `on_error`).
     */
    suspend fun pinRoomMessage(roomJid: String, targetStanzaId: String, pin: Boolean): Boolean =
        activeSession.clientCall { client ->
            if (pin) {
                client.pinMessage(bareJid(roomJid), targetStanzaId)
            } else {
                client.unpinMessage(bareJid(roomJid), targetStanzaId)
            }
        }

    /**
     * Seed the pin store with the room's current pin list (room open).
     * The snapshot is injected into the serialized event stream —
     * applying it here would race live pin events and clobber updates
     * that arrived while the fetch was in flight.
     */
    suspend fun refreshRoomPins(roomJid: String) {
        val client = activeSession.client ?: return
        val room = bareJid(roomJid)
        val fetchedAtVersion = stores.pinStore.eventVersion(room)
        val entries = try {
            client.fetchRoomPins(room)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return
        }
        activeSession.bridge?.submit(XmppEvent.RoomPins(room, entries, fetchedAtVersion))
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
    ): Boolean {
        val client = activeSession.client ?: return false
        return try {
            client.sendChatState(conversationJid, state, isGroupchat)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            false
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
    private fun ownMutationSender(conversationJid: String, isGroupchat: Boolean): String? {
        val own = activeSession.ownBareJid ?: return null
        return if (isGroupchat) {
            "${bareJid(conversationJid)}/${own.substringBefore('@')}"
        } else {
            own
        }
    }

    private suspend fun fetchHistory(
        fetch: suspend (WaddleClientInterface) -> WaddleMamPage,
    ): WaddleMamPage? {
        val page = activeSession.fetch(fetch) ?: return null
        // Per-message guard: one malformed archived stanza must not kill
        // the caller's paging coroutine (and crash-loop on every reopen
        // of the conversation, since the archive re-serves it).
        page.messages.forEach { message ->
            runCatching { stores.timelineStore.onArchivedMessage(message) }
        }
        return page
    }
}
