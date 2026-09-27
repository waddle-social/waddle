package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.store.ProfileStore

/** The shared peer-avatar refresh policy, against a scripted resolver. */
@OptIn(ExperimentalCoroutinesApi::class)
class PeerAvatarRepositoryTest {
    private data class Call(val jid: String, val knownId: String?)

    /**
     * Scripted resolver: every call parks until the test answers it, so
     * dedupe, concurrency, and supersession are observable.
     */
    private class ScriptedResolver(private val store: ProfileStore) : AvatarResolver {
        val calls = mutableListOf<Call>()
        private val pending = ArrayDeque<Pair<Call, CompletableDeferred<Unit>>>()
        val inFlight get() = pending.size
        var maxInFlight = 0
        private val outcomes = HashMap<String, AvatarLookup>()
        val commits = mutableListOf<Boolean>()

        fun answer(jid: String, outcome: AvatarLookup) {
            outcomes[jid] = outcome
        }

        fun releaseAll() {
            while (pending.isNotEmpty()) pending.removeFirst().second.complete(Unit)
        }

        fun release(jid: String) {
            val index = pending.indexOfFirst { it.first.jid == jid }
            pending.removeAt(index).second.complete(Unit)
        }

        override suspend fun resolve(
            jid: String,
            knownId: String?,
            commit: (() -> Unit) -> Boolean,
        ): AvatarLookup {
            val call = Call(jid, knownId)
            calls += call
            val gate = CompletableDeferred<Unit>()
            pending += call to gate
            maxInFlight = maxOf(maxInFlight, pending.size)
            gate.await()
            val outcome = outcomes[jid] ?: AvatarLookup.Failed
            commits += commit {
                when (outcome) {
                    is AvatarLookup.Found -> store.onAvatar(outcome.avatar)
                    AvatarLookup.Absent -> store.clearAvatar(jid)
                    AvatarLookup.Failed -> Unit
                }
            }
            return outcome
        }
    }

    private class Harness(
        scope: TestScope,
        budgetBytes: Long = ProfileStore.MAX_CACHED_AVATAR_BYTES,
        own: String? = null,
    ) {
        var now = 0L
        val store = ProfileStore(maxCachedAvatarBytes = budgetBytes)
        val resolver = ScriptedResolver(store)
        val repository = PeerAvatarRepository(
            store = store,
            resolver = resolver,
            scope = scope.backgroundScope,
            ownJid = { own },
            clock = { now },
        )

        /** Resolve [jid] to a 4-byte avatar and let the fetch settle. */
        fun load(scope: TestScope, jid: String, watch: Boolean = false): (() -> Unit)? {
            resolver.answer(jid, AvatarLookup.Found(testAvatar(jid = jid, id = "id-$jid", data = ByteArray(4))))
            val release = if (watch) {
                repository.watch(jid)
            } else {
                repository.ensure(jid)
                null
            }
            scope.runCurrent()
            resolver.releaseAll()
            scope.runCurrent()
            return release
        }
    }

    private val alice = "alice@waddle.test"

    @Test
    fun `concurrent ensures of one JID share a single fetch`() = runTest {
        val h = Harness(this)
        h.repository.ensure(alice)
        h.repository.ensure("$alice/phone")
        runCurrent()
        h.repository.ensure(alice)
        runCurrent()

        assertEquals(listOf(Call(alice, null)), h.resolver.calls)
    }

    @Test
    fun `at most four fetches are in flight`() = runTest {
        val h = Harness(this)
        (1..6).forEach { h.repository.ensure("u$it@waddle.test") }
        runCurrent()

        assertEquals(4, h.resolver.inFlight)
        h.resolver.releaseAll()
        runCurrent()
        assertEquals(2, h.resolver.inFlight)
        h.resolver.releaseAll()
        runCurrent()
        assertEquals(6, h.resolver.calls.size)
        assertEquals(4, h.resolver.maxInFlight)
    }

    @Test
    fun `a found avatar is revalidated only after 45 minutes`() = runTest {
        val h = Harness(this)
        h.resolver.answer(alice, AvatarLookup.Found(testAvatar(jid = alice, id = "id-1")))
        h.repository.ensure(alice)
        runCurrent()
        h.resolver.releaseAll()
        runCurrent()
        assertEquals("id-1", h.store.avatars.value[alice]?.id)

        h.now = PeerAvatarRepository.POSITIVE_TTL_MILLIS - 1
        h.repository.ensure(alice)
        runCurrent()
        assertEquals(1, h.resolver.calls.size)

        h.now = PeerAvatarRepository.POSITIVE_TTL_MILLIS
        h.repository.ensure(alice)
        runCurrent()
        assertEquals(2, h.resolver.calls.size)
    }

    @Test
    fun `misses and failures are retried after 10 minutes`() = runTest {
        val h = Harness(this)
        h.resolver.answer(alice, AvatarLookup.Absent)
        h.repository.ensure(alice)
        h.repository.ensure("bob@waddle.test") // scripted: Failed
        runCurrent()
        h.resolver.releaseAll()
        runCurrent()

        h.now = PeerAvatarRepository.RETRY_TTL_MILLIS - 1
        h.repository.ensure(alice)
        h.repository.ensure("bob@waddle.test")
        runCurrent()
        assertEquals(2, h.resolver.calls.size)

        h.now = PeerAvatarRepository.RETRY_TTL_MILLIS
        h.repository.ensure(alice)
        h.repository.ensure("bob@waddle.test")
        runCurrent()
        assertEquals(4, h.resolver.calls.size)
    }

    @Test
    fun `a new session makes every result stale`() = runTest {
        val h = Harness(this)
        h.resolver.answer(alice, AvatarLookup.Found(testAvatar(jid = alice, id = "id-1")))
        h.repository.ensure(alice)
        runCurrent()
        h.resolver.releaseAll()
        runCurrent()

        h.repository.markStale()
        h.repository.ensure(alice)
        runCurrent()
        assertEquals(2, h.resolver.calls.size)
    }

    @Test
    fun `an ensure after reconnect re-runs a fetch started in the old session`() = runTest {
        val h = Harness(this)
        h.repository.ensure(alice)
        runCurrent()
        h.repository.markStale()
        h.repository.ensure(alice)
        runCurrent()
        assertEquals(1, h.resolver.calls.size)

        h.resolver.releaseAll()
        runCurrent()
        assertEquals(2, h.resolver.calls.size)
    }

    @Test
    fun `AvatarChanged refetches with the announced id as the known id`() = runTest {
        val h = Harness(this)
        h.resolver.answer(alice, AvatarLookup.Found(testAvatar(jid = alice, id = "id-2")))
        h.repository.onAvatarChanged(alice, "id-2")
        runCurrent()

        assertEquals(listOf(Call(alice, "id-2")), h.resolver.calls)
        h.resolver.releaseAll()
        runCurrent()
        assertEquals("id-2", h.store.avatars.value[alice]?.id)
    }

    @Test
    fun `AvatarChanged during a fetch discards the stale answer and refetches`() = runTest {
        val h = Harness(this)
        h.resolver.answer(alice, AvatarLookup.Found(testAvatar(jid = alice, id = "old")))
        h.repository.ensure(alice)
        runCurrent()
        h.repository.onAvatarChanged(alice, "new")
        runCurrent()
        assertEquals(1, h.resolver.calls.size)

        h.resolver.releaseAll()
        runCurrent()
        assertEquals(listOf(false), h.resolver.commits)
        assertNull(h.store.avatars.value[alice])
        assertEquals(Call(alice, "new"), h.resolver.calls.last())

        h.resolver.answer(alice, AvatarLookup.Found(testAvatar(jid = alice, id = "new")))
        h.resolver.releaseAll()
        runCurrent()
        assertEquals("new", h.store.avatars.value[alice]?.id)
    }

    @Test
    fun `a disable clears to initials immediately and beats an in-flight fetch`() = runTest {
        val h = Harness(this)
        h.store.onAvatar(testAvatar(jid = alice, id = "id-1"))
        h.resolver.answer(alice, AvatarLookup.Found(testAvatar(jid = alice, id = "id-1")))
        h.repository.ensure(alice)
        runCurrent()

        h.repository.onAvatarChanged(alice, null)
        assertNull(h.store.avatars.value[alice])

        h.resolver.releaseAll()
        runCurrent()
        assertNull(h.store.avatars.value[alice])
        assertEquals(1, h.resolver.calls.size)
        // The disable counts as a settled miss: no refetch until it ages out.
        h.repository.ensure(alice)
        runCurrent()
        assertEquals(1, h.resolver.calls.size)
    }

    @Test
    fun `clear cancels in-flight fetches and forgets settled results`() = runTest {
        val h = Harness(this)
        h.resolver.answer("bob@waddle.test", AvatarLookup.Absent)
        h.repository.ensure("bob@waddle.test")
        h.repository.ensure(alice)
        runCurrent()
        h.resolver.release("bob@waddle.test")
        runCurrent()

        h.repository.clear()
        runCurrent()
        h.repository.ensure("bob@waddle.test")
        runCurrent()
        assertEquals(3, h.resolver.calls.size)
    }

    @Test
    fun `case variants of one JID share a single cache entry`() = runTest {
        val h = Harness(this)
        h.resolver.answer(alice, AvatarLookup.Found(testAvatar(jid = alice, id = "id-1")))
        h.repository.ensure("Alice@Waddle.Test/Phone")
        h.repository.ensure(alice)
        runCurrent()
        h.resolver.releaseAll()
        runCurrent()
        h.repository.ensure("ALICE@waddle.test")
        runCurrent()

        assertEquals(listOf(Call(alice, null)), h.resolver.calls)
        assertEquals("id-1", h.store.avatars.value[normalizedBareJid("Alice@WADDLE.test")]?.id)
    }

    @Test
    fun `a watched JID revalidates on the repository timer without a reconnect`() = runTest {
        val h = Harness(this)
        h.resolver.answer(alice, AvatarLookup.Found(testAvatar(jid = alice, id = "id-1")))
        val release = h.repository.watch(alice)
        runCurrent()
        h.resolver.releaseAll()
        runCurrent()
        assertEquals(1, h.resolver.calls.size)

        // Ticks before the TTL re-check but fetch nothing.
        h.now = PeerAvatarRepository.POSITIVE_TTL_MILLIS - 1
        advanceTimeBy(PeerAvatarRepository.REVALIDATE_TICK_MILLIS)
        runCurrent()
        assertEquals(1, h.resolver.calls.size)

        // The first tick past 45 min revalidates with the known id path.
        h.now = PeerAvatarRepository.POSITIVE_TTL_MILLIS
        advanceTimeBy(PeerAvatarRepository.REVALIDATE_TICK_MILLIS)
        runCurrent()
        assertEquals(2, h.resolver.calls.size)
        h.resolver.releaseAll()
        runCurrent()

        // Released: no watcher, no timer, no more fetches.
        release()
        h.now = 2 * PeerAvatarRepository.POSITIVE_TTL_MILLIS
        advanceTimeBy(10 * PeerAvatarRepository.REVALIDATE_TICK_MILLIS)
        runCurrent()
        assertEquals(2, h.resolver.calls.size)
    }

    @Test
    fun `a new session refetches watched JIDs right away`() = runTest {
        val h = Harness(this)
        h.resolver.answer(alice, AvatarLookup.Found(testAvatar(jid = alice, id = "id-1")))
        val release = h.repository.watch(alice)
        runCurrent()
        h.resolver.releaseAll()
        runCurrent()

        h.repository.markStale()
        runCurrent()
        assertEquals(2, h.resolver.calls.size)
        h.resolver.releaseAll()
        runCurrent()
        release()
    }

    @Test
    fun `the encoded-byte budget evicts the least recently used idle JIDs`() = runTest {
        val h = Harness(this, budgetBytes = 10)
        listOf("u1", "u2", "u3", "u4").forEach { h.load(this, "$it@waddle.test") }

        assertTrue(h.store.cachedAvatarBytes() <= 10)
        assertEquals(setOf("u3@waddle.test", "u4@waddle.test"), h.store.avatars.value.keys)
        assertTrue(h.store.knownAvatarIds("u1@waddle.test").isEmpty())
    }

    @Test
    fun `watched and own JIDs are never evicted`() = runTest {
        val own = "me@waddle.test"
        val h = Harness(this, budgetBytes = 10, own = own)
        h.load(this, own)
        val release = h.load(this, "shown@waddle.test", watch = true)
        listOf("u1", "u2", "u3").forEach { h.load(this, "$it@waddle.test") }

        val held = h.store.avatars.value.keys
        assertTrue(own in held)
        assertTrue("shown@waddle.test" in held)
        assertTrue(h.store.cachedAvatarBytes() <= 12)

        // Off screen again: it becomes evictable like any idle JID.
        checkNotNull(release).invoke()
        h.load(this, "u4@waddle.test")
        assertTrue("shown@waddle.test" !in h.store.avatars.value.keys)
        assertTrue(own in h.store.avatars.value.keys)
    }

    @Test
    fun `an evicted JID refetches on its next watch even inside the TTL`() = runTest {
        val h = Harness(this, budgetBytes = 4)
        h.load(this, "u1@waddle.test")
        h.load(this, "u2@waddle.test")
        assertTrue("u1@waddle.test" !in h.store.avatars.value.keys)
        val before = h.resolver.calls.count { it.jid == "u1@waddle.test" }

        h.load(this, "u1@waddle.test", watch = true)

        assertEquals(before + 1, h.resolver.calls.count { it.jid == "u1@waddle.test" })
        assertEquals("id-u1@waddle.test", h.store.avatars.value["u1@waddle.test"]?.id)
    }
}
