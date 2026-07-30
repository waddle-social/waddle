package social.waddle.android.client

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.store.SessionStores

/**
 * Successor-account regression coverage for the Android operation seams.
 * These tests intentionally use no timing hook: the old lease is parked,
 * then both same-account and different-account successors are made ready.
 * Every operation must reject before selecting either successor transport.
 */
class AndroidAttemptFencingTest {
    @Test
    fun `retired lease cannot redirect conversation room sticker extension or MDS work`() = runTest {
        listOf("alice@waddle.test", "bob@waddle.test").forEach { successorOwner ->
            val active = ActiveSession()
            val stores = SessionStores()
            val oldClient = FakeWaddleClient()
            active.advanceGeneration()
            active.activateOwner("alice@waddle.test")
            val oldAttempt = checkNotNull(active.beginAttempt())
            assertTrue(active.publishReady(oldAttempt, oldClient, "alice@waddle.test/phone") {})
            val retiredLease = checkNotNull(active.captureOwnerLease())

            active.revokeOutboundAuthority()
            active.advanceGeneration()
            active.activateOwner(successorOwner)
            val successor = FakeWaddleClient()
            val successorAttempt = checkNotNull(active.beginAttempt())
            assertTrue(active.publishReady(successorAttempt, successor, "$successorOwner/tablet") {})

            val conversations = ConversationVerbs(
                active,
                stores,
                SessionPrefs(InMemoryPreferencesDataStore()),
            )
            val rooms = RoomAdminVerbs(active, stores)
            val stickers = StickerVerbs(active, stores)
            val extensions = ExtensionCommandVerbs(active, stores)
            val reads = ReadStateCoordinator(
                active,
                stores,
                UserPrefs(InMemoryPreferencesDataStore()),
            ) {}

            assertEquals(null, conversations.fetchRoomHistory(retiredLease, "room@muc.waddle.test", 50u, null))
            assertFalse(rooms.refreshTopology(retiredLease))
            stickers.loadStickerPacks(retiredLease)
            assertEquals(emptyList<ExtensionCommand>(), extensions.runDiscovery(retiredLease))
            reads.bootstrapMdsDisplayed(retiredLease)

            assertEquals(
                ActiveSession.LeaseInvocation.Stale,
                active.invokeIfCurrent(retiredLease) {
                it.sendReaction("room@muc.waddle.test", "reaction-1", listOf("👍"), true)
            }
            )
            assertEquals(
                ActiveSession.LeaseInvocation.Stale,
                active.invokeIfCurrent(retiredLease) {
                it.sendCorrection("peer@waddle.test", "correction-1", "fixed", false, null)
            }
            )

            assertTrue(oldClient.fetchHistoryCalls.isEmpty())
            assertTrue(successor.fetchHistoryCalls.isEmpty())
            assertEquals(0, successor.topology.calls)
            assertTrue(successor.stickers.fetchCalls.isEmpty())
            assertEquals(0, successor.extensionCommands.discoverCalls)
            assertEquals(0, successor.mdsSubscribeCalls)
            assertTrue(successor.reactionCalls.isEmpty())
            assertTrue(successor.correctionCalls.isEmpty())
        }
    }
}
