package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.PushDeviceRef
import social.waddle.android.client.prefs.PushEndpoint
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.push.PushRegistrar
import social.waddle.android.client.push.PushStatus
import social.waddle.android.client.push.pushInstanceFor
import social.waddle.android.client.push.pushStatusOf
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddlePushDeviceCredentials
import social.waddle.client.ffi.WaddlePushEnvironment
import social.waddle.client.ffi.WaddlePushVapidLookup

/**
 * XEP-0357 registration over the Web Push provider (UnifiedPush): the
 * Push Service must always know exactly the endpoint this install holds,
 * and nothing it registered may be lost track of.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class PushRegistrarTest {
    private class Harness(testScope: TestScope) {
        val factory = FakeClientFactory()
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val manager = XmppSessionManager(
            sessionPrefs = prefs,
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
        )
        val client: FakeWaddleClient get() = factory.clients.last()

        suspend fun state() = prefs.push.state.first()

        suspend fun status() = pushStatusOf(state(), OWNER)
    }

    private suspend fun TestScope.signedIn(configure: (FakeWaddleClient) -> Unit = {}): Harness {
        val harness = Harness(this)
        harness.factory.onCreate = configure
        harness.manager.login(testSessionInfo())
        runCurrent()
        return harness
    }

    private fun TestScope.connect(harness: Harness) {
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
    }

    @Test
    fun `an endpoint is registered as a web push device and enabled`() = runTest {
        val harness = signedIn()
        assertTrue(harness.manager.recordPushEndpoint(INSTANCE, ENDPOINT))
        connect(harness)

        val push = harness.client.push
        assertEquals(listOf(SERVICE), push.vapidLookups)
        assertEquals(
            listOf(
                RecordedPushRegister(
                    serviceJid = SERVICE,
                    appId = PushRegistrar.APP_ID,
                    environment = WaddlePushEnvironment.PRODUCTION,
                    credentials = WaddlePushDeviceCredentials.WebPush(ENDPOINT.url, ENDPOINT.p256dh, ENDPOINT.auth),
                ),
            ),
            push.registers,
        )
        assertEquals(listOf(SERVICE to "node-1"), push.enables)
        assertEquals(PushStatus.ACTIVE, harness.status())
        harness.manager.logout()
    }

    @Test
    fun `a known endpoint is only re-enabled on the next stream`() = runTest {
        val harness = signedIn()
        harness.manager.recordPushEndpoint(INSTANCE, ENDPOINT)
        connect(harness)
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        harness.manager.requestReconnect()
        runCurrent()
        connect(harness)

        val push = harness.client.push
        assertTrue("no second device for the same endpoint", push.registers.isEmpty())
        assertEquals(listOf(SERVICE to "node-1"), push.enables)
        assertEquals(PushStatus.ACTIVE, harness.status())
        harness.manager.logout()
    }

    @Test
    fun `a new endpoint retires the old device before registering`() = runTest {
        val harness = signedIn()
        harness.manager.recordPushEndpoint(INSTANCE, ENDPOINT)
        connect(harness)

        val moved = ENDPOINT.copy(url = "https://push.example/up/2", p256dh = "p2", auth = "a2")
        harness.manager.recordPushEndpoint(INSTANCE, moved)
        runCurrent()

        val push = harness.client.push
        assertEquals(listOf(RecordedPushDisable(SERVICE, "node-1", "dev-1")), push.disables)
        assertEquals(2, push.registers.size)
        assertEquals(moved.url, (push.registers.last().credentials as WaddlePushDeviceCredentials.WebPush).endpoint)
        val state = harness.state()!!
        assertEquals(PushDeviceRef(SERVICE, "node-1", "dev-2"), state.device?.ref)
        assertTrue(state.pendingDisables.isEmpty())
        harness.manager.logout()
    }

    @Test
    fun `a refused enable retires the device it just registered`() = runTest {
        val harness = signedIn { it.push.enableSucceeds = false }
        harness.manager.recordPushEndpoint(INSTANCE, ENDPOINT)
        connect(harness)

        assertEquals(listOf(RecordedPushDisable(SERVICE, "node-1", "dev-1")), harness.client.push.disables)
        val state = harness.state()!!
        assertNull(state.device)
        assertTrue(state.pendingDisables.isEmpty())
        assertEquals(PushStatus.FAILED, harness.status())
        harness.manager.logout()
    }

    @Test
    fun `a device that cannot be disabled stays queued until it can`() = runTest {
        val harness = signedIn { it.push.disableSucceeds = false }
        harness.manager.recordPushEndpoint(INSTANCE, ENDPOINT)
        connect(harness)
        harness.manager.recordPushEndpointRevoked(INSTANCE)
        runCurrent()
        assertEquals(listOf(PushDeviceRef(SERVICE, "node-1", "dev-1")), harness.state()!!.pendingDisables)

        harness.factory.onCreate = {}
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        harness.manager.requestReconnect()
        runCurrent()
        connect(harness)

        assertEquals(listOf(RecordedPushDisable(SERVICE, "node-1", "dev-1")), harness.client.push.disables)
        assertTrue(harness.state()!!.pendingDisables.isEmpty())
        assertEquals(PushStatus.NONE, harness.status())
        harness.manager.logout()
    }

    @Test
    fun `a failed key lookup keeps the registration`() = runTest {
        val harness = signedIn()
        harness.manager.recordPushEndpoint(INSTANCE, ENDPOINT)
        connect(harness)
        harness.factory.onCreate = { it.push.vapid = WaddlePushVapidLookup.Failed }
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        harness.manager.requestReconnect()
        runCurrent()
        connect(harness)

        val push = harness.client.push
        assertTrue(push.disables.isEmpty())
        assertTrue(push.registers.isEmpty())
        assertEquals("the registration is re-asserted", listOf(SERVICE to "node-1"), push.enables)
        assertEquals(PushStatus.ACTIVE, harness.status())
        harness.manager.logout()
    }

    @Test
    fun `a server without web push retires the device`() = runTest {
        val harness = signedIn()
        harness.manager.recordPushEndpoint(INSTANCE, ENDPOINT)
        connect(harness)
        harness.factory.onCreate = { it.push.vapid = WaddlePushVapidLookup.NotAdvertised }
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        harness.manager.requestReconnect()
        runCurrent()
        connect(harness)

        assertEquals(listOf(RecordedPushDisable(SERVICE, "node-1", "dev-1")), harness.client.push.disables)
        assertEquals(PushStatus.UNSUPPORTED, harness.status())
        harness.manager.logout()
    }

    @Test
    fun `a rotated key waits for the distributor to re-register`() = runTest {
        val harness = signedIn { it.push.vapid = WaddlePushVapidLookup.Advertised(publicKey = "BNEW", kid = "kid-2") }
        harness.manager.recordPushEndpoint(INSTANCE, ENDPOINT)
        connect(harness)

        assertTrue("an endpoint bound to the old key is not registered", harness.client.push.registers.isEmpty())
        assertEquals("kid-2", harness.state()!!.advertised?.kid)
        assertEquals(PushStatus.PENDING, harness.status())

        val rebound = ENDPOINT.copy(url = "https://push.example/up/3", vapidKid = "kid-2")
        harness.manager.recordPushEndpoint(INSTANCE, rebound)
        runCurrent()
        assertEquals(1, harness.client.push.registers.size)
        assertEquals(PushStatus.ACTIVE, harness.status())
        harness.manager.logout()
    }

    @Test
    fun `a key rotated under an active device is pending until re-bound`() = runTest {
        val harness = signedIn()
        harness.manager.recordPushEndpoint(INSTANCE, ENDPOINT)
        connect(harness)
        assertEquals(PushStatus.ACTIVE, harness.status())

        harness.factory.onCreate = {
            it.push.vapid = WaddlePushVapidLookup.Advertised(publicKey = "BNEW", kid = "kid-2")
        }
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        harness.manager.requestReconnect()
        runCurrent()
        connect(harness)

        assertEquals(
            "pushes signed with the new key no longer reach the old endpoint",
            PushStatus.PENDING,
            harness.status(),
        )
        harness.manager.logout()
    }

    @Test
    fun `another account's endpoint callback is ignored`() = runTest {
        val harness = signedIn()

        assertFalse(harness.manager.recordPushEndpoint(pushInstanceFor("someone@else.test"), ENDPOINT))
        connect(harness)

        assertTrue(harness.client.push.registers.isEmpty())
        assertEquals(PushStatus.NONE, harness.status())
        harness.manager.logout()
    }

    @Test
    fun `sign-out disables the device while the stream is live and wipes the state`() = runTest {
        val harness = signedIn()
        harness.manager.recordPushEndpoint(INSTANCE, ENDPOINT)
        connect(harness)
        val push = harness.client.push

        harness.manager.logout()
        runCurrent()

        assertEquals(listOf(RecordedPushDisable(SERVICE, "node-1", "dev-1")), push.disables)
        assertNull(harness.state())
    }

    @Test
    fun `sign-out during a registration leaves nothing behind`() = runTest {
        val gate = CompletableDeferred<Unit>()
        val harness = signedIn { it.push.registerGate = gate }
        harness.manager.recordPushEndpoint(INSTANCE, ENDPOINT)
        connect(harness)
        val push = harness.client.push
        assertEquals(1, push.registers.size)

        val signOut = launch { harness.manager.logout() }
        runCurrent()
        gate.complete(Unit)
        runCurrent()
        signOut.join()

        assertTrue("the cancelled pass never enables", push.enables.isEmpty())
        assertNull(harness.state())
    }

    private companion object {
        const val OWNER = "icepuma@waddle.test"
        const val SERVICE = "push.waddle.test"
        val INSTANCE = pushInstanceFor(OWNER)
        val ENDPOINT = PushEndpoint(
            url = "https://push.example/up/1",
            p256dh = "p256dh-1",
            auth = "auth-1",
            vapidKid = FakePushServiceState.TEST_VAPID_KID,
        )
    }
}
