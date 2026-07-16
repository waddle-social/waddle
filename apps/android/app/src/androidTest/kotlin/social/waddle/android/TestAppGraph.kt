package social.waddle.android

import androidx.test.core.app.ApplicationProvider
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.FakeClientFactory
import social.waddle.android.client.FakeNetworkSignal
import social.waddle.android.client.FakeWaddleClient
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.auth.AuthProvider
import social.waddle.android.client.auth.DevicePollResult
import social.waddle.android.client.auth.DeviceStartResponse
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.testSessionInfo
import social.waddle.android.feature.login.LoginAuthGateway
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleMessage
import social.waddle.client.ffi.WaddlePresence

/**
 * Scriptable [LoginAuthGateway]: tests preset each call's result. The
 * fields are volatile because tests write them on the instrumentation
 * thread while ViewModel coroutines read them elsewhere.
 */
class FakeLoginAuthGateway : LoginAuthGateway {
    @Volatile
    var providersResult: Result<List<AuthProvider>> = Result.success(listOf(testAuthProvider()))

    @Volatile
    var startResult: Result<DeviceStartResponse> = Result.success(testDeviceStart())

    @Volatile
    var pollResult: Result<DevicePollResult> = Result.success(DevicePollResult.Pending)

    @Volatile
    var sessionResult: Result<WaddleSessionInfo?> = Result.success(testSessionInfo())

    override suspend fun providers(): Result<List<AuthProvider>> = providersResult

    override suspend fun startDeviceAuth(providerId: String): Result<DeviceStartResponse> =
        startResult

    override suspend fun pollDeviceAuth(deviceCode: String): Result<DevicePollResult> = pollResult

    override suspend fun session(sessionId: String): Result<WaddleSessionInfo?> = sessionResult
}

fun testAuthProvider(
    id: String = "github",
    displayName: String? = "GitHub",
): AuthProvider = AuthProvider(id = id, kind = "oauth", displayName = displayName)

/** No verification URI on purpose: the screen must not launch a Custom Tab mid-test. */
fun testDeviceStart(userCode: String = "ABCD-1234"): DeviceStartResponse = DeviceStartResponse(
    deviceCode = "device-1",
    userCode = userCode,
    verificationUri = null,
    verificationUriComplete = null,
    interval = 1,
    expiresIn = 600,
)

/**
 * A fully-faked [AppGraph] plus handles to its fakes. The in-memory
 * DataStores are mandatory: a second graph over the production
 * preference files crashes with "multiple DataStores active for the
 * same file" (the real graph exists in [WaddleApplication]).
 */
class TestAppGraph {
    val loginGateway = FakeLoginAuthGateway()
    val clientFactory = FakeClientFactory()
    val networkSignal = FakeNetworkSignal()

    val graph = AppGraph(
        context = ApplicationProvider.getApplicationContext(),
        clientFactory = clientFactory,
        serverUrl = "https://waddle.test",
        sessionStore = InMemoryPreferencesDataStore(),
        userStore = InMemoryPreferencesDataStore(),
        networkSignal = networkSignal,
        loginGatewayFactory = { loginGateway },
    )

    /** Flip the shell to SignedOut without touching the network. */
    fun signOut() {
        runBlocking { graph.sessionManager.logout() }
    }

    /** Sign in only: appState flips to Ready, no live connection yet. */
    fun signIn(session: WaddleSessionInfo = testSessionInfo()) {
        runBlocking { graph.signIn(session) }
    }

    /** Sign in and drive the fake FFI connection to a ready session. */
    fun signInAndConnect(session: WaddleSessionInfo = testSessionInfo()) {
        signIn(session)
        awaitCondition("a connection attempt") { clientFactory.clients.isNotEmpty() }
        clientFactory.emit(WaddleClientEvent.Connected)
        awaitCondition("the ready connection") {
            graph.sessionManager.connectionState.value == ConnectionState.Ready
        }
    }

    /** Inject a live FFI message into the current attempt's event stream. */
    fun emitLiveMessage(message: WaddleMessage) {
        clientFactory.emit(WaddleClientEvent.Message(message))
    }

    /** Inject a live FFI presence (e.g. to seed MUC occupants). */
    fun emitPresence(presence: WaddlePresence) {
        clientFactory.emit(WaddleClientEvent.Presence(presence))
    }

    /** The current attempt's fake client (records sends and queries). */
    fun activeFakeClient(): FakeWaddleClient = clientFactory.clients.last()

    /** Stop the graph's collectors and connection loop after a test. */
    fun shutdown() {
        runBlocking { graph.sessionManager.logout() }
        graph.applicationScope.cancel()
    }

    private fun awaitCondition(what: String, condition: () -> Boolean) = runBlocking {
        try {
            withTimeout(WAIT_TIMEOUT_MILLIS) {
                while (!condition()) delay(POLL_MILLIS)
            }
        } catch (timeout: TimeoutCancellationException) {
            throw AssertionError("timed out waiting for $what", timeout)
        }
    }

    private companion object {
        const val WAIT_TIMEOUT_MILLIS = 10_000L
        const val POLL_MILLIS = 20L
    }
}
