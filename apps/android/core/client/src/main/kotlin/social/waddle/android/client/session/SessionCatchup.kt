package social.waddle.android.client.session

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.first
import social.waddle.android.client.ConversationVerbs
import social.waddle.android.client.OutboundMessenger
import social.waddle.android.client.ReadStateCoordinator
import social.waddle.android.client.XmppEvent
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.persistQuietly
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.store.SessionStores
import social.waddle.client.ffi.WaddleTopology

/**
 * What happens after `SessionReady`: room topology discovery and the
 * sequential ready pipeline (rejoin → queue drain → MAM catch-up →
 * MDS bootstrap → pending-displayed drain).
 */
internal class SessionCatchup(
    private val sessionPrefs: SessionPrefs,
    private val stores: SessionStores,
    private val resume: ResumePersistence,
    private val verbs: ConversationVerbs,
    private val messenger: OutboundMessenger,
    private val readState: ReadStateCoordinator,
    private val activeSession: ActiveSession,
) {
    /**
     * Discover the spaces/channels topology into [SessionStores.roomStore]
     * and hand it back so the rejoin step can derive the XEP-0402
     * autojoin join set. Best-effort: a failed discovery returns `null`
     * and the rejoin degrades to the persisted-intent rooms only.
     */
    private suspend fun refreshTopology(lease: ActiveSession.OwnerLease): WaddleTopology? = try {
        when (val result = activeSession.invokeIfCurrent(lease) { it.discoverTopology() }) {
            ActiveSession.LeaseInvocation.Stale,
            ActiveSession.LeaseInvocation.NotConnected,
            -> null
            is ActiveSession.LeaseInvocation.Completed -> result.value.takeIf {
                activeSession.applyIfCurrent(lease) { stores.roomStore.setTopology(it) }
            }
        }
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
        null
    }

    /**
     * One sequential pipeline, deliberately not parallel: the rejoin's
     * join set is derived from the freshly discovered topology's
     * bookmark autojoin flags, queued groupchat sends need the rejoin's
     * join presence first, and the bounded catch-up must not race the
     * replay or hammer the server.
     */
    suspend fun onSessionReady(
        session: WaddleSessionInfo,
        freshStream: Boolean,
    ) {
        // Best-effort pipeline: prefs reads and queue writes inside
        // can raise IOException, and an escaped throw on this root
        // coroutine would kill the process ("never throw" contract).
        persistQuietly {
            val lease = activeSession.captureOwnerLease() ?: return@persistQuietly
            val topology = refreshTopology(lease)
            rejoinRooms(lease, session, topology)
            messenger.drainOutboundQueue()
            // Every ready session, resumed streams included (web
            // parity): the inbox is the server-authoritative unread
            // baseline the live pushes then patch.
            hydrateInbox(lease)
            if (freshStream) {
                catchUpConversations(lease)
                hydrateNotifySettings(lease)
            }
            // After catch-up so fetched cursors can resolve against
            // the freshly loaded newest pages.
            readState.bootstrapMdsDisplayed(lease)
            readState.drainPendingDisplayed()
        }
    }

    /**
     * XEP-0430 hydrate (web session-ready parity): fetch the server's
     * authoritative per-conversation unread state and INJECT the page
     * into the serialized event stream (the MDS handshake pattern) so
     * absolute-set reconciles never interleave with live increments.
     * `noMessages = true` deviates from the web's default deliberately:
     * the FFI page carries no message bodies, so the embedded MAM
     * `<result/>` payloads the web consumes would be dead weight here.
     * Best-effort: a failed fetch keeps the previous counts.
     */
    private suspend fun hydrateInbox(lease: ActiveSession.OwnerLease) {
        val result = try {
            when (val invocation = activeSession.invokeIfCurrent(lease) {
                it.fetchInbox(onlyUnread = false, noMessages = true)
            }) {
                ActiveSession.LeaseInvocation.Stale,
                ActiveSession.LeaseInvocation.NotConnected,
                -> return
                is ActiveSession.LeaseInvocation.Completed -> invocation.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return
        }
        if (result.conversations.isEmpty()) return
        val applied = Job()
        if (!activeSession.applyIfCurrent(lease) {
                activeSession.bridge?.submit(XmppEvent.InboxEntries(result.conversations, applied))
            }
        ) return
        // Bare join (bootstrapMdsDisplayed parity): swallowing a
        // cancellation here would resume a cancelled pipeline.
        applied.join()
    }

    /**
     * XEP-0492 hydrate (web `runSessionBootstrap` parity): re-fetch
     * both bookmark carriers on every FRESH stream only — a XEP-0198
     * resume preserves the session, and there is no PEP `+notify`
     * subscription yet, so the fresh-stream refetch is the sync point.
     * Best-effort: a failed fetch keeps the previous entries (the §3
     * defaults cover conversations that never hydrated).
     */
    private suspend fun hydrateNotifySettings(lease: ActiveSession.OwnerLease) {
        try {
            when (activeSession.invokeIfCurrent(
                lease,
                { client ->
                    stores.notifySettingsStore.hydrate(
                        fetchRoomBookmarks = { client.fetchUserBookmarks() },
                        fetchDmBookmarks = { client.fetchDmBookmarks() },
                    )
                },
            )) {
                ActiveSession.LeaseInvocation.Stale,
                ActiveSession.LeaseInvocation.NotConnected,
                -> return
                is ActiveSession.LeaseInvocation.Completed -> Unit
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            // Keep the pipeline going: notification defaults still apply.
        }
    }

    /**
     * Re-issues MUC join presence on each ready session. Room join
     * state does not survive a non-resumed stream, so without this a
     * reconnect silently stops live channel traffic. The duplicate join
     * presence on a resumed stream is benign (XEP-0045 treats re-joining
     * an occupied nick from the same full JID as a presence update).
     *
     * Bookmark-driven join set (web autojoin fan-out parity): every
     * topology channel whose XEP-0402 `autojoin` flag is set — group
     * DMs INCLUDED, their live messages must flow even though stage 3
     * surfaces them on the DM list instead of the channel list — union
     * the persisted-intent rooms from [SessionPrefs.joinedRooms] (the
     * pre-refresh overlay for rooms joined this session). A failed
     * topology discovery (`topology == null`) degrades to the persisted
     * set only. Successful joins are marked in the room store so the
     * MAM catch-up and screens see them; the prefs stay user-intent
     * only and are NOT widened with autojoin rooms.
     */
    private suspend fun rejoinRooms(
        lease: ActiveSession.OwnerLease,
        session: WaddleSessionInfo,
        topology: WaddleTopology?,
    ) {
        val autojoinRooms = topology?.channels.orEmpty()
            .filter { it.autojoin }
            .map { it.roomJid }
        val joinSet = autojoinRooms.toSet() + sessionPrefs.joinedRooms.first()
        for (roomJid in joinSet) {
            try {
                when (activeSession.invokeIfCurrent(lease) { it.joinRoom(roomJid, session.xmppLocalpart) }) {
                    ActiveSession.LeaseInvocation.Stale,
                    ActiveSession.LeaseInvocation.NotConnected,
                    -> return
                    is ActiveSession.LeaseInvocation.Completed -> Unit
                }
                if (!activeSession.applyIfCurrent(lease) { stores.roomStore.markJoined(roomJid) }) return
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                // Keep going: one unjoinable room must not block the rest.
            }
        }
    }

    /**
     * Fresh-stream MAM catch-up (web reconnect-catchup parity): fetch
     * the newest archive page for every joined room plus the most
     * recently active DMs, so conversations that are NOT on screen also
     * recover messages missed while the stream was down. The FFI only
     * pages with `before_id`, so instead of the web's `after`-cursor
     * query this fetches the newest page and lets the timeline store's
     * identity dedupe collapse the overlap — the same shape as the
     * per-screen refetch. Sequential and bounded (one page each, DMs
     * capped) to avoid hammering the server after every reconnect.
     */
    private suspend fun catchUpConversations(lease: ActiveSession.OwnerLease) {
        val rooms = stores.roomStore.joinedRooms.value
        for (roomJid in rooms) {
            verbs.fetchRoomHistory(lease, roomJid, CATCHUP_PAGE_SIZE, beforeId = null)
        }
        for (peerJid in resume.cursorTracker.newestFirst(excluding = rooms, limit = CATCHUP_DM_LIMIT)) {
            verbs.fetchDmHistory(lease, peerJid, CATCHUP_PAGE_SIZE, beforeId = null)
        }
    }

    companion object {
        /** Newest page per conversation on fresh-stream catch-up. */
        const val CATCHUP_PAGE_SIZE = 50u

        /** Only the most recently active DMs catch up (rooms: all joined). */
        const val CATCHUP_DM_LIMIT = 3
    }
}
