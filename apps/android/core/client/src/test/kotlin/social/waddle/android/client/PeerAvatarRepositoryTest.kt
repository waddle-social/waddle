package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
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

    private class Harness(scope: TestScope) {
        var now = 0L
        val store = ProfileStore()
        val resolver = ScriptedResolver(store)
        val repository = PeerAvatarRepository(
            avatars = store.avatars,
            resolver = resolver,
            clearAvatar = store::clearAvatar,
            scope = scope.backgroundScope,
            clock = { now },
        )
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

        val before = h.repository.epoch.value
        h.repository.markStale()
        assertTrue(h.repository.epoch.value > before)
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
}
