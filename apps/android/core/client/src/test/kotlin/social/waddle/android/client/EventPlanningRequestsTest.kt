package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.session.ActiveSession
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleException
import social.waddle.client.ffi.WaddlePlanningItem
import social.waddle.client.ffi.WaddlePlanningKind

class EventPlanningRequestsTest {
    @Test
    fun `create refusal preserves native condition and never publishes metadata`() = runTest {
        val client = PlanningClient()
        val refusal = WaddleException.Stanza("forbidden", "Account cannot create rooms")
        client.createFailure = refusal
        val (active, requests) = ready(client)
        val result = requests.create(requests.ownerFor(SELF), item()) as EventPlanningResult.Failed
        assertEquals(EventPlanningStage.CREATE_ROOM, result.failure.stage)
        assertEquals(EventPlanningFailureReason.PERMISSION_DENIED, result.failure.reason)
        assertSame(refusal, result.failure.nativeError)
        assertNull(result.retry)
        assertEquals(1, client.creates)
        assertTrue(client.saved.isEmpty())
        assertTrue(active.hasActiveClient())
    }

    @Test
    fun `unsupported server is distinct from access denial during create and save`() = runTest {
        val client = PlanningClient()
        val (_, requests) = ready(client)
        val owner = requests.ownerFor(SELF)
        client.createFailure = WaddleException.Stanza("feature-not-implemented", null)
        val create = requests.create(owner, item()) as EventPlanningResult.Failed
        assertEquals(EventPlanningStage.CREATE_ROOM, create.failure.stage)
        assertEquals(EventPlanningFailureReason.UNSUPPORTED, create.failure.reason)
        client.createFailure = null
        client.saveFailure = WaddleException.Stanza("forbidden", null)
        val save = requests.save(owner, ROOM, item()) as EventPlanningResult.Failed
        assertEquals(EventPlanningStage.SAVE_ITEM, save.failure.stage)
        assertEquals(EventPlanningFailureReason.PERMISSION_DENIED, save.failure.reason)
        client.saveFailure = WaddleException.Stanza("service-unavailable", null)
        val unsupported = requests.save(owner, ROOM, item()) as EventPlanningResult.Failed
        assertEquals(EventPlanningFailureReason.UNAVAILABLE, unsupported.failure.reason)
    }

    @Test
    fun `failed first publish retains room and retry uses it without creating a duplicate`() = runTest {
        val client = PlanningClient()
        client.saveFailure = WaddleException.Timeout()
        val (_, requests) = ready(client)
        val owner = requests.ownerFor(SELF)
        var retained: EventPlanningRetry? = null
        val failed = requests.create(owner, item(), onRoomCreated = { retained = it }) as EventPlanningResult.Failed
        assertEquals(EventPlanningStage.SAVE_ITEM, failed.failure.stage)
        assertEquals(EventPlanningFailureReason.TIMEOUT, failed.failure.reason)
        assertSame(retained, failed.retry)
        assertEquals(ROOM, retained?.roomJid)
        client.saveFailure = null
        val saved = requests.create(owner, item().copy(title = "London revised"), retained) as EventPlanningResult.Saved
        assertEquals(1, client.creates)
        assertEquals(listOf(ROOM, ROOM), client.saved.map { it.first })
        assertEquals("event", saved.item.id)
        assertEquals(WaddlePlanningKind.EVENT, saved.item.kind)
        assertEquals(SELF, saved.item.author)
        assertEquals("London revised", saved.item.title)
    }

    @Test
    fun `cancelling metadata publish propagates and retains created room for retry`() = runTest {
        val client = PlanningClient()
        client.saveStall = CompletableDeferred()
        val (active, requests) = ready(client)
        val owner = requests.ownerFor(SELF)
        var retained: EventPlanningRetry? = null
        val request = async { requests.create(owner, item(), onRoomCreated = { retained = it }) }
        runCurrent()
        assertNotNull(retained)
        request.cancelAndJoin()
        assertTrue(request.isCancelled)
        assertTrue(active.hasActiveClient())
        client.saveStall = null
        assertTrue(requests.create(owner, item(), retained) is EventPlanningResult.Saved)
        assertEquals(1, client.creates)
    }

    @Test
    fun `cancellation during creation cannot publish or turn into generic failure`() = runTest {
        val client = PlanningClient()
        client.createStall = CompletableDeferred()
        val (_, requests) = ready(client)
        val request = async { requests.create(requests.ownerFor(SELF), item()) }
        runCurrent()
        request.cancelAndJoin()
        assertTrue(request.isCancelled)
        assertTrue(client.saved.isEmpty())
    }

    @Test
    fun `one fence keeps room creation and first publish on the same account while logout waits`() = runTest {
        val client = PlanningClient()
        val release = CompletableDeferred<Unit>()
        client.createStall = release
        val (active, requests) = ready(client)
        val create = async { requests.create(requests.ownerFor(SELF), item()) }
        runCurrent()
        val logout = async { active.revokeOutboundAuthority() }
        runCurrent()
        assertFalse(logout.isCompleted)
        release.complete(Unit)
        assertTrue(create.await() is EventPlanningResult.Saved)
        logout.await()
        assertEquals(1, client.saved.size)
        assertEquals(SELF, client.saved.single().second.author)
    }

    @Test
    fun `different account cannot capture old owner or reuse its retry`() = runTest {
        val old = PlanningClient().apply { saveFailure = WaddleException.Timeout() }
        val (active, requests) = ready(old)
        val owner = requests.ownerFor(SELF)
        val retry = (requests.create(owner, item()) as EventPlanningResult.Failed).retry
        val successor = PlanningClient()
        relogin(active, successor, "bob@example.test")
        assertNull(requests.ownerFor(SELF))
        val failed = requests.create(owner, item(), retry) as EventPlanningResult.Failed
        assertEquals(EventPlanningFailureReason.SESSION_CHANGED, failed.failure.reason)
        assertEquals(0, successor.creates)
        assertTrue(successor.saved.isEmpty())
        assertFalse(requests.applyIfCurrent(owner) { error("stale projection") })
    }

    @Test
    fun `same account relogin blocks stale queued work but a fresh explicit owner recovers`() = runTest {
        val old = PlanningClient().apply { saveFailure = WaddleException.Timeout() }
        val (active, requests) = ready(old)
        val stale = requests.ownerFor(SELF)
        val retry = (requests.create(stale, item()) as EventPlanningResult.Failed).retry
        val successor = PlanningClient()
        relogin(active, successor, SELF)
        assertEquals(
            EventPlanningFailureReason.SESSION_CHANGED,
            (requests.save(stale, ROOM, item()) as EventPlanningResult.Failed).failure.reason
        )
        val fresh = requests.ownerFor(SELF)
        assertFalse(checkNotNull(stale).sameSessionAs(fresh))
        assertEquals(
            EventPlanningFailureReason.SESSION_CHANGED,
            (requests.create(fresh, item(), retry) as EventPlanningResult.Failed).failure.reason
        )
        assertTrue(successor.saved.isEmpty())
        assertTrue(requests.create(fresh, item()) is EventPlanningResult.Saved)
        assertEquals(1, successor.creates)
        assertEquals(1, successor.saved.size)
    }

    @Test
    fun `owner captured before ready can retry and initially absent owner can be captured later`() = runTest {
        val active = ActiveSession()
        val requests = EventPlanningRequests(active)
        assertNull(requests.ownerFor(SELF))
        assertEquals(
            EventPlanningFailureReason.NOT_CONNECTED,
            (requests.create(null, item()) as EventPlanningResult.Failed).failure.reason
        )
        active.advanceGeneration()
        active.activateOwner(SELF)
        val owner = requests.ownerFor(SELF)
        assertEquals(
            EventPlanningFailureReason.NOT_CONNECTED,
            (requests.create(owner, item()) as EventPlanningResult.Failed).failure.reason
        )
        publish(active, PlanningClient(), SELF)
        assertTrue(requests.create(owner, item()) is EventPlanningResult.Saved)
    }

    private suspend fun ready(client: PlanningClient): Pair<ActiveSession, EventPlanningRequests> {
        val active = ActiveSession()
        active.advanceGeneration()
        active.activateOwner(SELF)
        publish(active, client, SELF)
        return active to EventPlanningRequests(active)
    }

    private suspend fun relogin(active: ActiveSession, client: PlanningClient, self: String) {
        active.revokeOutboundAuthority()
        active.advanceGeneration()
        active.activateOwner(self)
        publish(active, client, self)
    }

    private suspend fun publish(active: ActiveSession, client: PlanningClient, self: String) {
        val attempt = checkNotNull(active.beginAttempt())
        assertTrue(active.publishReady(attempt, client, "$self/android") {})
    }

    private class PlanningClient : WaddleClientInterface by FakeWaddleClient() {
        var creates = 0
        val saved = mutableListOf<Pair<String, WaddlePlanningItem>>()
        var createFailure: WaddleException? = null
        var saveFailure: WaddleException? = null
        var createStall: CompletableDeferred<Unit>? = null
        var saveStall: CompletableDeferred<Unit>? = null
        override suspend fun createEventRoom(name: String, privateRoom: Boolean): String {
            creates += 1
            createFailure?.let { throw it }
            createStall?.await()
            return ROOM
        }
        override suspend fun eventPlanningPublish(roomJid: String, item: WaddlePlanningItem): WaddlePlanningItem {
            saved += roomJid to item
            saveFailure?.let { throw it }
            saveStall?.await()
            return item
        }
    }

    companion object {
        private const val SELF = "alice@example.test"
        private const val ROOM = "event-london@muc.example.test"
        private fun item() = WaddlePlanningItem(
            id = "wrong-ui-id", kind = WaddlePlanningKind.TASK, author = "forged@example.test", title = "London",
            description = null, url = null, category = null, location = null, timezone = null,
            startsAt = null, endsAt = null, assignee = null, completed = false, pinned = false,
            amountMinor = null, currency = null, participants = emptyList(), options = emptyList(),
            selections = emptyList(), subject = null, attendance = null, status = null,
        )
    }
}
