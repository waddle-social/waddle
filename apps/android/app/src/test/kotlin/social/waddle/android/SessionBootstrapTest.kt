package social.waddle.android

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.onStart
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import okhttp3.Dispatcher
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.FakeNetworkSignal
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.WaddleAppState
import social.waddle.android.client.auth.WaddleAuthApi
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.testSessionInfo
import java.io.IOException
import java.util.concurrent.AbstractExecutorService
import java.util.concurrent.TimeUnit

/**
 * Drives [SessionBootstrap] against a real [WaddleAuthApi] whose OkHttp
 * client short-circuits in an interceptor on a same-thread dispatcher —
 * responses resolve inline on the test scheduler, no real sockets or
 * threads, so `runCurrent` fully settles every restore.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class SessionBootstrapTest {
    /** Canned `/api/auth/session` exchanges, consumed in FIFO order. */
    private sealed interface Canned {
        data class Json(val code: Int, val body: String) : Canned

        data object NetworkFailure : Canned
    }

    private val responses = ArrayDeque<Canned>()
    private var requestCount = 0

    private val interceptor = Interceptor { chain ->
        requestCount += 1
        when (val canned = checkNotNull(responses.removeFirstOrNull()) { "no canned response left" }) {
            is Canned.NetworkFailure -> throw IOException("network unreachable")
            is Canned.Json -> Response.Builder()
                .request(chain.request())
                .protocol(Protocol.HTTP_1_1)
                .code(canned.code)
                .message("")
                .body(canned.body.toResponseBody("application/json".toMediaType()))
                .build()
        }
    }

    /** Runs enqueued OkHttp calls inline on the enqueueing thread. */
    private class DirectExecutorService : AbstractExecutorService() {
        override fun execute(command: Runnable) = command.run()

        override fun shutdown() = Unit

        override fun shutdownNow(): MutableList<Runnable> = mutableListOf()

        override fun isShutdown(): Boolean = false

        override fun isTerminated(): Boolean = false

        override fun awaitTermination(timeout: Long, unit: TimeUnit): Boolean = true
    }

    private inner class Harness(
        testScope: TestScope,
        store: DataStore<Preferences> = InMemoryPreferencesDataStore(),
    ) {
        val sessionPrefs = SessionPrefs(store)
        val network = FakeNetworkSignal()
        val managerAppState = MutableStateFlow<WaddleAppState>(WaddleAppState.Loading)
        val signedIn = mutableListOf<WaddleSessionInfo>()
        var signOutCalls = 0

        val bootstrap = SessionBootstrap(
            sessionPrefs = sessionPrefs,
            authApi = WaddleAuthApi(
                "https://waddle.test",
                OkHttpClient.Builder()
                    .dispatcher(Dispatcher(DirectExecutorService()))
                    .addInterceptor(interceptor)
                    .build(),
            ),
            signIn = { session ->
                signedIn += session
                managerAppState.value = WaddleAppState.Ready
            },
            signOutLocally = {
                signOutCalls += 1
                managerAppState.value = WaddleAppState.SignedOut
            },
            managerAppState = managerAppState,
            networkSignal = network,
            scope = testScope.backgroundScope,
        )
    }

    private fun sessionJson(isExpired: Boolean = false): String =
        """
        {"session_id":"sess-1","username":"icepuma","xmpp_localpart":"icepuma",
         "jid":"icepuma@waddle.test","xmpp_websocket_url":"wss://waddle.test/xmpp",
         "is_expired":$isExpired}
        """.trimIndent()

    @Test
    fun `a valid stored session restores and signs in`() = runTest {
        val harness = Harness(this)
        harness.sessionPrefs.setSessionId("sess-1")
        responses += Canned.Json(200, sessionJson())

        harness.bootstrap.restore()
        runCurrent()

        assertEquals(listOf(testSessionInfo()), harness.signedIn)
        assertEquals(0, harness.signOutCalls)
        assertEquals(WaddleAppState.Ready, harness.bootstrap.appState.value)
        assertFalse(harness.bootstrap.splashHold.value)
    }

    @Test
    fun `an expired session signs out locally`() = runTest {
        val harness = Harness(this)
        harness.sessionPrefs.setSessionId("sess-1")
        responses += Canned.Json(200, sessionJson(isExpired = true))

        harness.bootstrap.restore()
        runCurrent()

        assertTrue(harness.signedIn.isEmpty())
        assertEquals(1, harness.signOutCalls)
        assertEquals(WaddleAppState.SignedOut, harness.bootstrap.appState.value)
    }

    @Test
    fun `a server-side dead session signs out locally`() = runTest {
        val harness = Harness(this)
        harness.sessionPrefs.setSessionId("sess-gone")
        responses += Canned.Json(401, "")

        harness.bootstrap.restore()
        runCurrent()

        assertTrue(harness.signedIn.isEmpty())
        assertEquals(1, harness.signOutCalls)
        assertEquals(WaddleAppState.SignedOut, harness.bootstrap.appState.value)
    }

    @Test
    fun `no stored session signs out without touching the network`() = runTest {
        val harness = Harness(this)

        harness.bootstrap.restore()
        runCurrent()

        assertEquals(1, harness.signOutCalls)
        assertEquals("no session validation call", 0, requestCount)
        assertEquals(WaddleAppState.SignedOut, harness.bootstrap.appState.value)
        assertFalse(harness.bootstrap.splashHold.value)
    }

    @Test
    fun `a network failure surfaces the retryable error state`() = runTest {
        val harness = Harness(this)
        harness.sessionPrefs.setSessionId("sess-1")
        responses += Canned.NetworkFailure

        harness.bootstrap.restore()
        runCurrent()

        assertTrue(harness.bootstrap.appState.value is WaddleAppState.Error)
        assertTrue(harness.signedIn.isEmpty())
        assertEquals("no local sign-out on a network failure", 0, harness.signOutCalls)
        assertFalse("splash never pins on the network round-trip", harness.bootstrap.splashHold.value)
    }

    @Test
    fun `a failed restore re-runs when connectivity arrives`() = runTest {
        val harness = Harness(this)
        harness.sessionPrefs.setSessionId("sess-1")
        responses += Canned.NetworkFailure

        harness.bootstrap.restore()
        runCurrent()
        assertTrue(harness.bootstrap.appState.value is WaddleAppState.Error)

        responses += Canned.Json(200, sessionJson())
        harness.network.state.value = false
        runCurrent()
        harness.network.state.value = true
        runCurrent()

        assertEquals(listOf(testSessionInfo()), harness.signedIn)
        assertEquals(WaddleAppState.Ready, harness.bootstrap.appState.value)
    }

    @Test
    fun `connectivity arrival without a failed restore does nothing`() = runTest {
        val harness = Harness(this)
        harness.sessionPrefs.setSessionId("sess-1")

        harness.network.state.value = false
        runCurrent()
        harness.network.state.value = true
        runCurrent()

        assertEquals("no spontaneous restore", 0, requestCount)
    }

    @Test
    fun `restore is single-flight while one is in progress`() = runTest {
        // Delay the local prefs read so the first restore is still
        // suspended when the second one arrives.
        val inner = InMemoryPreferencesDataStore()
        val slowStore = object : DataStore<Preferences> {
            override val data: Flow<Preferences> = inner.data.onStart { delay(50) }

            override suspend fun updateData(
                transform: suspend (t: Preferences) -> Preferences,
            ): Preferences = inner.updateData(transform)
        }
        val harness = Harness(this, store = slowStore)
        harness.sessionPrefs.setSessionId("sess-1")
        responses += Canned.Json(200, sessionJson())
        responses += Canned.Json(200, sessionJson())

        harness.bootstrap.restore()
        runCurrent()
        assertTrue("splash held until the local read completes", harness.bootstrap.splashHold.value)

        harness.bootstrap.restore()
        advanceTimeBy(50)
        runCurrent()

        assertEquals("the double-tapped retry coalesces", 1, requestCount)
        assertEquals(listOf(testSessionInfo()), harness.signedIn)
        assertFalse(harness.bootstrap.splashHold.value)
    }

    @Test
    fun `splash hold releases after the local read even on failure`() = runTest {
        val harness = Harness(this)
        harness.sessionPrefs.setSessionId("sess-1")
        responses += Canned.NetworkFailure

        assertTrue(harness.bootstrap.splashHold.value)
        harness.bootstrap.restore()
        runCurrent()
        assertFalse(harness.bootstrap.splashHold.value)
    }

    @Test
    fun `manager state passes through once past loading`() = runTest {
        val harness = Harness(this)
        harness.sessionPrefs.setSessionId("sess-1")
        responses += Canned.NetworkFailure

        harness.bootstrap.restore()
        runCurrent()
        assertTrue(harness.bootstrap.appState.value is WaddleAppState.Error)

        // A restore failure only masks Loading; a signed-in manager wins.
        harness.managerAppState.value = WaddleAppState.Ready
        runCurrent()
        assertEquals(WaddleAppState.Ready, harness.bootstrap.appState.value)
    }

    @Test
    fun `a throwing prefs read surfaces the error instead of crashing`() = runTest {
        val broken = object : DataStore<Preferences> {
            override val data: Flow<Preferences> =
                flow { throw IOException("preferences_pb corrupted") }

            override suspend fun updateData(
                transform: suspend (t: Preferences) -> Preferences,
            ): Preferences = throw IOException("preferences_pb corrupted")
        }
        val harness = Harness(this, store = broken)

        harness.bootstrap.restore()
        runCurrent()

        assertEquals(
            WaddleAppState.Error("preferences_pb corrupted"),
            harness.bootstrap.appState.value,
        )
        assertFalse(harness.bootstrap.splashHold.value)
    }
}
