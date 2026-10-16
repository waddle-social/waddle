package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.session.SelfPresence
import social.waddle.android.client.session.effectivePresenceOf
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleManualStatus
import social.waddle.client.ffi.WaddleStatusPreference
import java.io.IOException
import java.time.Instant

/**
 * ADR-010 decision 5: a manual status picked on any device of the account
 * (`urn:waddle:status-preference:0`) overrides Android's automatic
 * presence; resetting to Automatic hands control back to it.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerStatusPreferenceTest {
    private class Harness(testScope: TestScope, initial: DesiredPresence = DesiredPresence.Available) {
        val factory = FakeClientFactory()
        val desired = MutableStateFlow(initial)
        val manager = XmppSessionManager(
            sessionPrefs = SessionPrefs(InMemoryPreferencesDataStore()),
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
            desiredPresence = desired,
        )

        val client: FakeWaddleClient get() = factory.clients.last()

        fun shows(): List<String?> = client.presence.calls.map { it.show }

        fun storeOnEveryClient(preference: WaddleStatusPreference) {
            factory.onCreate = { it.presence.storedPreference = preference }
        }
    }

    private suspend fun TestScope.ready(harness: Harness) {
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
    }

    private fun TestScope.pick(harness: Harness, preference: WaddleStatusPreference) {
        harness.factory.emit(WaddleClientEvent.StatusPreferenceChanged(preference))
        runCurrent()
    }

    @Test
    fun `a manual away set on another device replaces the first available`() = runTest {
        val harness = Harness(this)
        harness.storeOnEveryClient(MANUAL_AWAY)

        ready(harness)

        assertEquals(1, harness.client.presence.preferenceFetches)
        assertEquals("never available, not even briefly", listOf<String?>("away"), harness.shows())
        assertEquals("a manual pick carries no idle", null, harness.client.presence.calls.single().idleSince)
        harness.manager.logout()
    }

    @Test
    fun `a live pick overrides and resetting to automatic restores`() = runTest {
        val harness = Harness(this)
        ready(harness)

        pick(harness, MANUAL_AWAY)
        pick(harness, WaddleStatusPreference.Manual(WaddleManualStatus.DND))
        pick(harness, WaddleStatusPreference.Automatic)

        assertEquals(listOf(null, "away", "dnd", null), harness.shows())
        harness.manager.logout()
    }

    @Test
    fun `a manual pick suppresses auto-away and survives a reconnect`() = runTest {
        val harness = Harness(this)
        ready(harness)
        pick(harness, WaddleStatusPreference.Manual(WaddleManualStatus.AVAILABLE))

        // Available is a pin: the automatic away underneath changes nothing.
        harness.desired.value = DesiredPresence.Away(LEFT_AT)
        runCurrent()
        assertEquals(listOf<String?>(null), harness.shows())

        // The next stream advertises the pin even while its fetch fails.
        harness.factory.onCreate = { it.presence.preferenceFailure = IOException("timeout") }
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        advanceTimeBy(RECONNECT_DELAY_MILLIS)
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        assertEquals(2, harness.factory.clients.size)
        assertEquals(listOf<String?>(null), harness.shows())

        pick(harness, WaddleStatusPreference.Automatic)
        assertEquals("back to the automatic away", listOf(null, "away"), harness.shows())
        harness.manager.logout()
    }

    @Test
    fun `a slow fetch holds the first presence only briefly, then corrects it`() = runTest {
        val harness = Harness(this)
        val gate = CompletableDeferred<Unit>()
        harness.factory.onCreate = {
            it.presence.storedPreference = MANUAL_AWAY
            it.presence.preferenceStall = gate
        }
        ready(harness)
        assertTrue("waiting for the stored pick", harness.client.presence.calls.isEmpty())

        advanceTimeBy(SelfPresence.PREFERENCE_WAIT_MILLIS)
        runCurrent()
        assertEquals(listOf<String?>(null), harness.shows())

        gate.complete(Unit)
        runCurrent()
        assertEquals(listOf(null, "away"), harness.shows())
        harness.manager.logout()
    }

    @Test
    fun `a live pick during a slow fetch wins over the older stored value`() = runTest {
        val harness = Harness(this)
        val gate = CompletableDeferred<Unit>()
        harness.factory.onCreate = {
            it.presence.storedPreference = MANUAL_AWAY
            it.presence.preferenceStall = gate
        }
        ready(harness)

        pick(harness, WaddleStatusPreference.Automatic)
        gate.complete(Unit)
        runCurrent()

        assertEquals(listOf<String?>(null), harness.shows())
        harness.manager.logout()
    }

    @Test
    fun `a lease-only stream stays silent whatever the manual pick`() = runTest {
        val harness = Harness(this, initial = DesiredPresence.None)
        harness.storeOnEveryClient(MANUAL_AWAY)

        ready(harness)
        pick(harness, WaddleStatusPreference.Manual(WaddleManualStatus.DND))

        assertTrue(harness.client.presence.calls.isEmpty())
        harness.manager.logout()
    }

    @Test
    fun `sign-out forgets the pick`() = runTest {
        val harness = Harness(this)
        ready(harness)
        pick(harness, MANUAL_AWAY)
        harness.manager.logout()
        runCurrent()

        harness.factory.onCreate = { it.presence.preferenceFailure = IOException("timeout") }
        ready(harness)

        assertEquals("the next account starts automatic", listOf<String?>(null), harness.shows())
        harness.manager.logout()
    }

    @Test
    fun `ADR-010 precedence maps every manual status and never wakes a lease-only stream`() {
        val auto = DesiredPresence.ExtendedAway(LEFT_AT)
        assertEquals(auto, effectivePresenceOf(auto, WaddleStatusPreference.Automatic))
        assertEquals(
            DesiredPresence.Available,
            effectivePresenceOf(auto, WaddleStatusPreference.Manual(WaddleManualStatus.AVAILABLE)),
        )
        assertEquals(
            DesiredPresence.Chat,
            effectivePresenceOf(auto, WaddleStatusPreference.Manual(WaddleManualStatus.CHAT)),
        )
        assertEquals(DesiredPresence.Away(idleSince = null), effectivePresenceOf(auto, MANUAL_AWAY))
        assertEquals(
            DesiredPresence.DoNotDisturb,
            effectivePresenceOf(DesiredPresence.Available, WaddleStatusPreference.Manual(WaddleManualStatus.DND)),
        )
        assertEquals(DesiredPresence.None, effectivePresenceOf(DesiredPresence.None, MANUAL_AWAY))
    }

    private companion object {
        val MANUAL_AWAY = WaddleStatusPreference.Manual(WaddleManualStatus.AWAY)
        val LEFT_AT: Instant = Instant.parse("2026-10-16T09:15:30Z")

        /** First backoff step with `PinnedRandom(0.5)`. */
        const val RECONNECT_DELAY_MILLIS = 1_000L
    }
}
