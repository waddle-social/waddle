package social.waddle.android.client.store

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.testArchivedMessage
import social.waddle.android.client.testMessage

/**
 * Publication cost is paid by every collector of a conversation, so the
 * store must publish once per page and keep unchanged rows identical
 * across emissions (the UI skips identical rows).
 */
@OptIn(ExperimentalCoroutinesApi::class)
class TimelinePublishTest {
    private val store = TimelineStore()

    @Before
    fun setUp() {
        store.setOwnBareJid("me@waddle.test")
    }

    @Test
    fun `a history page publishes once`() = runTest {
        val emissions = mutableListOf<List<TimelineItem>>()
        val job = launch(UnconfinedTestDispatcher(testScheduler)) {
            store.timeline("alice@waddle.test").collect { emissions += it }
        }

        store.onArchivedPage(
            (1..50).map { n ->
                testArchivedMessage(
                    mamId = "mam-$n",
                    id = "msg-$n",
                    timestamp = "2026-07-15T10:%02d:00Z".format(n),
                )
            },
        )

        assertEquals("initial empty value plus one page publish", 2, emissions.size)
        assertEquals(50, emissions.last().size)
        job.cancel()
    }

    @Test
    fun `a page keeps time order whatever order it arrives in`() {
        store.onArchivedPage(
            listOf(
                testArchivedMessage(mamId = "m3", id = "c", timestamp = "2026-07-15T10:03:00Z"),
                testArchivedMessage(mamId = "m1", id = "a", timestamp = "2026-07-15T10:01:00Z"),
                testArchivedMessage(mamId = "m2", id = "b", timestamp = "2026-07-15T10:02:00Z"),
            ),
        )
        store.onLiveMessage(testMessage(id = "d", stanzaId = "d", from = "alice@waddle.test/phone"))

        assertEquals(listOf("a", "b", "c", "d"), store.timeline("alice@waddle.test").value.map { it.id })
    }

    @Test
    fun `unchanged rows keep their identity across publishes`() {
        store.onArchivedPage(
            listOf(
                testArchivedMessage(mamId = "m1", id = "a", timestamp = "2026-07-15T10:01:00Z"),
                testArchivedMessage(mamId = "m2", id = "b", timestamp = "2026-07-15T10:02:00Z"),
            ),
        )
        val before = store.timeline("alice@waddle.test").value

        store.onLiveMessage(testMessage(id = "c", stanzaId = "c", from = "alice@waddle.test/phone"))
        val after = store.timeline("alice@waddle.test").value

        assertEquals(3, after.size)
        assertSame(before[0], after[0])
        assertSame(before[1], after[1])
    }

    @Test
    fun `a malformed row does not drop the rest of its page`() {
        store.onArchivedPage(
            listOf(
                testArchivedMessage(mamId = "m1", id = "a", timestamp = "2026-07-15T10:01:00Z"),
                testArchivedMessage(mamId = "m2", id = null, body = null, timestamp = "not a time"),
                testArchivedMessage(mamId = "m3", id = "c", timestamp = "2026-07-15T10:03:00Z"),
            ),
        )

        assertEquals(listOf("a", "c"), store.timeline("alice@waddle.test").value.map { it.id })
    }
}
