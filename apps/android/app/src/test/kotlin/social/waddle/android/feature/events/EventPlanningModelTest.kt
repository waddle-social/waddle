package social.waddle.android.feature.events

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.EventPlanningFailureReason
import social.waddle.android.client.EventPlanningStage
import social.waddle.android.client.FakeClientFactory
import social.waddle.android.client.FakeNetworkSignal
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.PinnedRandom
import social.waddle.android.client.ReconnectPolicy
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.testSessionInfo
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddlePlanningKind

@OptIn(ExperimentalCoroutinesApi::class)
class EventPlanningModelTest {
    @Before
    fun setUp() { Dispatchers.setMain(StandardTestDispatcher()) }

    @After
    fun tearDown() { Dispatchers.resetMain() }

    @Test
    fun `new editor clears initial refresh failure and old create error`() = runTest {
        val harness = Harness(this)
        val model = EventPlanningModel(harness.manager, SELF)
        runCurrent()
        assertTrue(model.state.value.failed)
        model.startEditor()
        assertFalse(model.state.value.failed)
        model.create(newPlanningItem(WaddlePlanningKind.EVENT, "London")) {}
        runCurrent()
        assertEquals(EventPlanningStage.CREATE_ROOM, model.state.value.failure?.stage)
        assertEquals(EventPlanningFailureReason.NOT_CONNECTED, model.state.value.failure?.reason)
        model.startEditor()
        assertFalse(model.state.value.failed)
        assertNull(model.state.value.failure)
    }

    @Test
    fun `explicit refresh recovers absent owner and stale same-account authority`() = runTest {
        val harness = Harness(this)
        val model = EventPlanningModel(harness.manager, SELF)
        runCurrent()
        harness.login(this)
        model.refresh()
        runCurrent()
        assertFalse(model.state.value.failed)
        model.create(newPlanningItem(WaddlePlanningKind.EVENT, "London")) {}
        runCurrent()
        assertEquals("London", model.state.value.items.single().title)
        harness.manager.logout()
        harness.login(this)
        model.save(newPlanningItem(WaddlePlanningKind.EVENT, "Stale update"))
        runCurrent()
        assertEquals(EventPlanningFailureReason.SESSION_CHANGED, model.state.value.failure?.reason)
        assertEquals("London", model.state.value.items.single().title)
        model.refresh()
        runCurrent()
        assertNull(model.state.value.room)
        assertTrue(model.state.value.items.isEmpty())
        assertFalse(model.state.value.failed)
        model.create(newPlanningItem(WaddlePlanningKind.EVENT, "Fresh event")) {}
        runCurrent()
        assertEquals("Fresh event", model.state.value.items.single().title)
        harness.manager.logout()
    }

    private class Harness(scope: TestScope) {
        val factory = FakeClientFactory()
        val manager = XmppSessionManager(
            sessionPrefs = SessionPrefs(InMemoryPreferencesDataStore()), clientFactory = factory,
            networkSignal = FakeNetworkSignal(), userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(scope.testScheduler),
        )
        suspend fun login(scope: TestScope) {
            manager.login(testSessionInfo())
            scope.runCurrent()
            factory.emit(WaddleClientEvent.Connected)
            scope.runCurrent()
        }
    }

    private companion object { const val SELF = "icepuma@waddle.test" }
}
