package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleMood
import social.waddle.client.ffi.WaddlePepProfile
import java.security.MessageDigest

/** Profile-publishing verbs (XEP-0292/0084/0107/0108/0118) through the manager. */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerProfileTest {
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

    private val own = "icepuma@waddle.test"

    @Test
    fun `loadSelfProfile fetches vcard, status, and avatar for the account`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.vcard4 = testVcard4(fullName = "Ice Puma", nickname = "ice")
        harness.client.pepProfile = WaddlePepProfile(
            mood = WaddleMood(kind = "happy", text = "hi"),
            activity = null,
            tune = testTune(),
        )
        harness.client.avatar = testAvatar(jid = own, id = "id-1")

        assertEquals(VerbResult.Ok, harness.manager.loadSelfProfile())

        assertEquals(listOf(own), harness.client.fetchVcard4Calls)
        assertEquals(listOf(own), harness.client.fetchPepProfileCalls)
        assertEquals(listOf(own), harness.client.requestAvatarCalls.map { it.first })
        assertEquals("Ice Puma", harness.manager.profileStore.selfVcard.value?.fullName)
        assertEquals("happy", harness.manager.profileStore.selfMood.value?.kind)
        assertEquals("Come Together", harness.manager.profileStore.selfTune.value?.title)
        assertEquals("id-1", harness.manager.profileStore.avatars.value[own]?.id)
        harness.manager.logout()
    }

    @Test
    fun `a failed vcard fetch rejects the load without wiping status`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.fetchVcard4Failure = RuntimeException("boom")

        assertEquals(VerbResult.Rejected, harness.manager.loadSelfProfile())
        assertTrue(harness.client.fetchPepProfileCalls.isEmpty())
        harness.manager.logout()
    }

    @Test
    fun `publishProfile applies optimistically and keeps the value on success`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val vcard = testVcard4(fullName = "New Name")

        assertEquals(VerbResult.Ok, harness.manager.publishProfile(vcard))

        assertEquals(listOf(RecordedProfileVerb.PublishVcard4(vcard)), harness.client.profileVerbs)
        assertEquals("New Name", harness.manager.profileStore.selfVcard.value?.fullName)
        harness.manager.logout()
    }

    @Test
    fun `a failed publishProfile rolls the store back to the last persisted vcard`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.manager.profileStore.setSelfVcard(testVcard4(fullName = "Old Name"))
        harness.client.profileVerbFailure = RuntimeException("rejected")

        assertEquals(VerbResult.Rejected, harness.manager.publishProfile(testVcard4(fullName = "New Name")))

        assertEquals("Old Name", harness.manager.profileStore.selfVcard.value?.fullName)
        harness.manager.logout()
    }

    @Test
    fun `publishAvatar caches the bytes at their SHA-1 item id on success`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val bytes = byteArrayOf(1, 2, 3, 4)

        assertEquals(
            VerbResult.Ok,
            harness.manager.publishAvatar(bytes, "image/png", width = 64u, height = 64u),
        )

        assertEquals(
            listOf(RecordedProfileVerb.PublishAvatar(4, "image/png", 64u, 64u)),
            harness.client.profileVerbs,
        )
        val expectedId = MessageDigest.getInstance("SHA-1").digest(bytes)
            .joinToString(separator = "") { "%02x".format(it) }
        val current = harness.manager.profileStore.avatars.value[own]
        assertEquals(expectedId, current?.id)
        assertEquals("image/png", current?.mimeType)
        harness.manager.logout()
    }

    @Test
    fun `a failed publishAvatar leaves the current avatar untouched`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.profileVerbFailure = RuntimeException("too large")

        assertEquals(
            VerbResult.Rejected,
            harness.manager.publishAvatar(byteArrayOf(1), "image/png", width = 0u, height = 0u),
        )
        assertNull(harness.manager.profileStore.avatars.value[own])
        harness.manager.logout()
    }

    @Test
    fun `disableAvatar drops the account's current avatar`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.manager.profileStore.onAvatar(testAvatar(jid = own, id = "id-1"))

        assertEquals(VerbResult.Ok, harness.manager.disableAvatar())

        assertEquals(listOf(RecordedProfileVerb.DisableAvatar), harness.client.profileVerbs)
        assertNull(harness.manager.profileStore.avatars.value[own])
        harness.manager.logout()
    }

    @Test
    fun `fetchAvatar with a cached item id never touches the wire`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.avatar = testAvatar(jid = "alice@waddle.test", id = "id-1")

        val first = harness.manager.fetchAvatar("alice@waddle.test")
        assertEquals("id-1", first?.id)
        assertEquals(1, harness.client.requestAvatarCalls.size)

        // XEP-0084 §4.2: a known, already-cached item id MUST NOT be
        // re-retrieved — the second fetch answers from the cache.
        val second = harness.manager.fetchAvatar("alice@waddle.test", knownId = "id-1")
        assertEquals("id-1", second?.id)
        assertEquals(1, harness.client.requestAvatarCalls.size)

        // An unknown id is a real change and fetches again.
        harness.client.avatar = testAvatar(jid = "alice@waddle.test", id = "id-2")
        assertEquals("id-2", harness.manager.fetchAvatar("alice@waddle.test", knownId = "id-2")?.id)
        assertEquals(2, harness.client.requestAvatarCalls.size)
        harness.manager.logout()
    }

    @Test
    fun `fetchAvatar hands the store's known ids to the wire fetch and skips the data on a hit`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.avatar = testAvatar(jid = "alice@waddle.test", id = "id-1")

        // First fetch: nothing cached, so no known ids ride along and
        // the bytes come back over the wire.
        val first = harness.manager.fetchAvatar("alice@waddle.test")
        assertEquals("id-1", first?.id)
        assertEquals(emptyList<String>(), harness.client.requestAvatarCalls[0].second)

        // Second wire fetch (no local knownId shortcut): the cached
        // id-1 rides along, the fake answers id-only (§4.2 data-IQ
        // skip), and the cached bytes are re-marked current.
        harness.manager.profileStore.clearAvatar("alice@waddle.test")
        val second = harness.manager.fetchAvatar("alice@waddle.test")
        assertEquals("id-1", second?.id)
        assertTrue("id-1" in harness.client.requestAvatarCalls[1].second)
        assertEquals(
            "id-1",
            harness.manager.profileStore.avatars.value["alice@waddle.test"]?.id,
        )
        harness.manager.logout()
    }

    @Test
    fun `a publish ack that lands after a same-account relogin is dropped`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.profileVerbDelayMillis = 1_000L

        // The publish is in flight when the account signs out and
        // signs back IN AS THE SAME account: a bare-JID owner gate
        // would falsely pass here, the generation gate must not.
        val pending = async { harness.manager.setMood("happy", null) }
        runCurrent()
        harness.manager.logout()
        harness.loginReady(this)

        advanceTimeBy(1_001L)
        runCurrent()
        assertEquals(VerbResult.Ok, pending.await())
        assertNull(harness.manager.profileStore.selfMood.value)
        harness.manager.logout()
    }

    @Test
    fun `mood and activity verbs record and mirror into the store`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        assertEquals(VerbResult.Ok, harness.manager.setMood("happy", "hi"))
        assertEquals("happy", harness.manager.profileStore.selfMood.value?.kind)
        assertEquals(VerbResult.Ok, harness.manager.setActivity("working", "coding", null))
        assertEquals("coding", harness.manager.profileStore.selfActivity.value?.specific)
        assertEquals(VerbResult.Ok, harness.manager.clearMood())
        assertNull(harness.manager.profileStore.selfMood.value)
        assertEquals(VerbResult.Ok, harness.manager.clearActivity())
        assertNull(harness.manager.profileStore.selfActivity.value)

        assertEquals(
            listOf(
                RecordedProfileVerb.PublishMood("happy", "hi"),
                RecordedProfileVerb.PublishActivity("working", "coding", null),
                RecordedProfileVerb.RetractMood,
                RecordedProfileVerb.RetractActivity,
            ),
            harness.client.profileVerbs,
        )
        harness.manager.logout()
    }

    @Test
    fun `a refused mood publish leaves the store untouched`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.profileVerbFailure = RuntimeException("bad kind")

        assertEquals(VerbResult.Rejected, harness.manager.setMood("happy", null))
        assertNull(harness.manager.profileStore.selfMood.value)
        harness.manager.logout()
    }

    @Test
    fun `setTune publishes immediately and returns the wire result`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        // User-initiated submit: no debounce — the verb goes out now
        // and the caller sees the real result (web parity).
        assertEquals(VerbResult.Ok, harness.manager.setTune(testTune(title = "Come Together")))

        val published = harness.client.profileVerbs.filterIsInstance<RecordedProfileVerb.PublishTune>()
        assertEquals(listOf("Come Together"), published.map { it.tune.title })
        assertEquals("Come Together", harness.manager.profileStore.selfTune.value?.title)
        harness.manager.logout()
    }

    @Test
    fun `a refused setTune surfaces Rejected and leaves the store untouched`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.profileVerbFailure = RuntimeException("not-acceptable")

        assertEquals(VerbResult.Rejected, harness.manager.setTune(testTune(title = "Nope")))
        assertNull(harness.manager.profileStore.selfTune.value)
        harness.manager.logout()
    }

    @Test
    fun `clearTune retracts and clears the store`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.manager.profileStore.setSelfTune(testTune())

        assertEquals(VerbResult.Ok, harness.manager.clearTune())

        assertEquals(listOf(RecordedProfileVerb.RetractTune), harness.client.profileVerbs)
        assertNull(harness.manager.profileStore.selfTune.value)
        harness.manager.logout()
    }

    @Test
    fun `logout wipes the profile store`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.manager.profileStore.setSelfVcard(testVcard4())
        harness.manager.profileStore.onAvatar(testAvatar(jid = own, id = "id-1"))

        harness.manager.logout()

        assertNull(harness.manager.profileStore.selfVcard.value)
        assertTrue(harness.manager.profileStore.avatars.value.isEmpty())
    }
}
