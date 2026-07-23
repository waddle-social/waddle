package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.store.SessionStores
import social.waddle.client.ffi.WaddleException
import java.time.Instant

/**
 * XEP-0472 community feed passthroughs against the
 * `urn:xmpp:pubsub-social-feed:1` node on `community.<domain>` (the
 * FFI resolves the service from the session domain). Same shape rules
 * as [ConversationVerbs]: never throws past the seam — every call
 * collapses to a typed [VerbResult] and [SessionStores.feedStore]
 * stays consistent with what the server delivered.
 *
 * Every post-ack store write is generation-gated (the [ProfileVerbs]
 * pattern): the [ActiveSession.generation] captured before the wire
 * call must still be current when the reply lands, so a slow fetch
 * racing a logout (or a relogin into the same account) is dropped
 * instead of parked into the next session's stores.
 */
internal class FeedVerbs(
    private val activeSession: ActiveSession,
    private val stores: SessionStores,
) {
    /**
     * Fetch the latest feed page into the store (replacing the cached
     * list — the feed is pull-only, web `useSocialFeed.refresh`
     * parity).
     */
    suspend fun refreshFeed(): VerbResult {
        val client = activeSession.client ?: return VerbResult.NotConnected
        val generation = activeSession.generation
        stores.feedStore.beginLoading()
        val entries = try {
            client.fetchFeed(FEED_PAGE_SIZE)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            if (activeSession.generation == generation) stores.feedStore.applyLoadFailed()
            return VerbResult.Rejected
        }
        if (activeSession.generation != generation) return VerbResult.NotConnected
        stores.feedStore.applyLoaded(entries.map { it.toDomain() })
        return VerbResult.Ok
    }

    /**
     * Publish a post, applied optimistically on the server's accept:
     * the local entry (authored as the own bare JID, the value the
     * server stamps anyway) heads the list immediately, then a refresh
     * reconciles with the server's view (web `useSocialFeed.post`
     * parity plus the pull for other authors' posts landed since the
     * last fetch).
     */
    suspend fun publishFeedPost(body: String, title: String?): VerbResult {
        val trimmedBody = body.trim()
        if (trimmedBody.isEmpty()) return VerbResult.NotReady
        val client = activeSession.client ?: return VerbResult.NotConnected
        val generation = activeSession.generation
        val trimmedTitle = title?.trim()?.takeIf { it.isNotEmpty() }
        val itemId = try {
            client.publishFeedPost(trimmedBody, trimmedTitle)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: WaddleException.NotConnected) {
            return VerbResult.NotConnected
        } catch (_: Throwable) {
            return VerbResult.Rejected
        }
        if (activeSession.generation != generation) return VerbResult.NotConnected
        stores.feedStore.prepend(
            FeedEntry(
                id = itemId,
                title = trimmedTitle,
                body = trimmedBody,
                author = activeSession.ownBareJid,
                published = Instant.now(),
            ),
        )
        refreshFeed()
        return VerbResult.Ok
    }

    companion object {
        /** Web parity: `useSocialFeed`'s default page size. */
        const val FEED_PAGE_SIZE: UInt = 50u
    }
}
