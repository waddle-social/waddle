package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddlePackSticker
import social.waddle.client.ffi.WaddleStickerHash
import social.waddle.client.ffi.WaddleStickerPack

/** XEP-0449 sticker-pack verbs and their session cache through the manager. */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerStickersTest {
    private class Harness(testScope: TestScope) {
        val factory = FakeClientFactory()
        val manager = XmppSessionManager(
            sessionPrefs = SessionPrefs(InMemoryPreferencesDataStore()),
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
        )

        suspend fun loginReady(scope: TestScope) {
            manager.login(testSessionInfo())
            scope.runCurrent()
            factory.emit(WaddleClientEvent.Connected)
            scope.runCurrent()
        }

        val client get() = factory.clients.last()
    }

    private fun ffiSticker(desc: String = "🐧") = WaddlePackSticker(
        desc = desc,
        mediaType = "image/webp",
        size = 1234u,
        width = 512u,
        height = 256u,
        hashes = listOf(WaddleStickerHash(algo = "sha-256", valueB64 = "aGFzaA==")),
        sources = listOf("https://upload.waddle.test/penguin.webp"),
        suggests = listOf(":penguin:"),
    )

    private fun ffiPack(id: String = "pack-1", stickers: List<WaddlePackSticker> = listOf(ffiSticker())) =
        WaddleStickerPack(id = id, name = "Penguins", summary = "waddling", restricted = false, stickers = stickers)

    private fun domainSticker(desc: String = "🐧") = StickerItem(
        desc = desc,
        mediaType = "image/webp",
        sizeBytes = 1234L,
        width = 512,
        height = 256,
        hashes = listOf(StickerHash(algo = "sha-256", valueB64 = "aGFzaA==")),
        sources = listOf("https://upload.waddle.test/penguin.webp"),
        suggests = listOf(":penguin:"),
    )

    @Test
    fun `load maps the fetched packs into the ready state and caches it`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.stickerPacks = listOf(ffiPack())

        harness.manager.loadStickerPacks()

        assertEquals(
            StickerPacksResult.Ready(
                listOf(
                    StickerPack(
                        id = "pack-1",
                        name = "Penguins",
                        summary = "waddling",
                        restricted = false,
                        stickers = listOf(domainSticker()),
                    ),
                ),
            ),
            harness.manager.stickerPackStore.packs.value,
        )
        assertEquals(listOf<String?>(null), harness.client.stickerFetchCalls)

        // Cached: a second load never refetches the PEP node.
        harness.manager.loadStickerPacks()
        assertEquals(1, harness.client.stickerFetchCalls.size)
        harness.manager.logout()
    }

    @Test
    fun `load with no packs is the empty first-run state and stays cached`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        harness.manager.loadStickerPacks()
        harness.manager.loadStickerPacks()

        assertEquals(StickerPacksResult.Empty, harness.manager.stickerPackStore.packs.value)
        assertEquals(1, harness.client.stickerFetchCalls.size)
        harness.manager.logout()
    }

    @Test
    fun `load failure degrades to unavailable and the next load retries`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.stickerFetchFailure = RuntimeException("boom")

        harness.manager.loadStickerPacks()
        assertEquals(StickerPacksResult.Unavailable, harness.manager.stickerPackStore.packs.value)

        harness.client.stickerFetchFailure = null
        harness.client.stickerPacks = listOf(ffiPack())
        harness.manager.loadStickerPacks()
        assertTrue(harness.manager.stickerPackStore.packs.value is StickerPacksResult.Ready)
        assertEquals(2, harness.client.stickerFetchCalls.size)
        harness.manager.logout()
    }

    @Test
    fun `load with no live session is unavailable`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        harness.manager.loadStickerPacks()

        assertEquals(StickerPacksResult.Unavailable, harness.manager.stickerPackStore.packs.value)
        harness.manager.logout()
    }

    @Test
    fun `publish upserts the pack under the server-derived id`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        // Loaded first (the picker always loads before creating): a
        // publish into an unloaded cache deliberately stays unloaded.
        harness.manager.loadStickerPacks()
        harness.client.stickerPublishedId = "server-pack-id"

        val result = harness.manager.publishStickerPack(
            name = "  Penguins  ",
            summary = " waddling ",
            items = listOf(domainSticker()),
        )

        assertEquals(VerbResult.Ok, result)
        val published = harness.client.stickerPublishCalls.single()
        assertEquals("", published.id)
        assertEquals("Penguins", published.name)
        assertEquals("waddling", published.summary)
        assertEquals(listOf(ffiSticker()), published.stickers)
        assertEquals(
            StickerPacksResult.Ready(
                listOf(
                    StickerPack(
                        id = "server-pack-id",
                        name = "Penguins",
                        summary = "waddling",
                        restricted = false,
                        stickers = listOf(domainSticker()),
                    ),
                ),
            ),
            harness.manager.stickerPackStore.packs.value,
        )
        harness.manager.logout()
    }

    @Test
    fun `publish failure leaves the store untouched`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.manager.loadStickerPacks()
        harness.client.stickerPublishFailure = RuntimeException("boom")

        val result = harness.manager.publishStickerPack("Penguins", null, listOf(domainSticker()))

        assertEquals(VerbResult.Rejected, result)
        assertEquals(StickerPacksResult.Empty, harness.manager.stickerPackStore.packs.value)
        harness.manager.logout()
    }

    @Test
    fun `publish with no items fails locally without touching the wire`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        val result = harness.manager.publishStickerPack("Penguins", null, emptyList())

        assertEquals(VerbResult.NotReady, result)
        assertTrue(harness.client.stickerPublishCalls.isEmpty())
        harness.manager.logout()
    }

    @Test
    fun `remove drops the pack and the last removal empties the store`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.stickerPacks = listOf(ffiPack())
        harness.manager.loadStickerPacks()

        val result = harness.manager.removeStickerPack("pack-1")

        assertEquals(VerbResult.Ok, result)
        assertEquals(listOf("pack-1"), harness.client.stickerRetractCalls)
        assertEquals(StickerPacksResult.Empty, harness.manager.stickerPackStore.packs.value)
        harness.manager.logout()
    }

    @Test
    fun `retract refusal is rejected and keeps the store`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.stickerPacks = listOf(ffiPack())
        harness.manager.loadStickerPacks()
        harness.client.stickerRetractResult = false

        val result = harness.manager.removeStickerPack("pack-1")

        assertEquals(VerbResult.Rejected, result)
        assertTrue(harness.manager.stickerPackStore.packs.value is StickerPacksResult.Ready)
        harness.manager.logout()
    }

    @Test
    fun `retract failure knob is rejected`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.stickerRetractFailure = RuntimeException("boom")

        assertEquals(VerbResult.Rejected, harness.manager.removeStickerPack("pack-1"))
        harness.manager.logout()
    }

    @Test
    fun `dm sticker send inserts an own echo flagged as sticker`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        harness.manager.sendChatMessage(
            peerJid = "alice@waddle.test",
            body = "🐧",
            extras = MessageSendExtras(
                sticker = StickerSendRef(
                    packId = "pack-1",
                    desc = "🐧",
                    url = "https://upload.waddle.test/penguin.webp",
                    mediaType = "image/webp",
                    hashes = listOf(StickerHash("sha-256", "aGFzaA==")),
                ),
            ),
        )

        val echo = harness.manager.timelineStore.timeline("alice@waddle.test").value.single()
        assertTrue("own DM echo of a sticker send must render as a sticker", echo.isSticker)
        assertEquals("🐧", echo.sharedFiles.single().desc)
        harness.manager.logout()
    }
}
