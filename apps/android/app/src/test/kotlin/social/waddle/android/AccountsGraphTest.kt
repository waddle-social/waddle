package social.waddle.android

import android.app.Application
import androidx.test.core.app.ApplicationProvider
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNotSame
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import social.waddle.android.client.FakeClientFactory
import social.waddle.android.client.FakeNetworkSignal
import social.waddle.android.client.InMemoryAccountStores
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.account.accountKey
import social.waddle.android.client.testSessionInfo
import social.waddle.android.push.KeyWrapper

/**
 * Several accounts in one process: separate runtimes, stores and keys, a
 * selection the passthroughs follow, and sign-out that touches only one.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], application = Application::class)
class AccountsGraphTest {
    private class PlainWrapper : KeyWrapper {
        override fun wrap(plain: ByteArray): String = plain.decodeToString()

        override fun unwrap(wrapped: String): ByteArray? = wrapped.encodeToByteArray()
    }

    private val stores = InMemoryAccountStores()
    private val graph = AppGraph(
        context = ApplicationProvider.getApplicationContext(),
        clientFactory = FakeClientFactory(),
        defaultServerUrl = "https://a.example",
        legacySessionStore = InMemoryPreferencesDataStore(),
        accountStores = stores,
        accountRegistryStore = InMemoryPreferencesDataStore(),
        userStore = InMemoryPreferencesDataStore(),
        networkSignal = FakeNetworkSignal(),
        connectionWanted = MutableStateFlow(true),
        fcmTokens = null,
        stayConnectedSupported = false,
        keyWrapper = PlainWrapper(),
    )

    @After
    fun tearDown() {
        graph.applicationScope.coroutineContext[kotlinx.coroutines.Job]?.cancel()
    }

    private fun signIn(origin: String, jid: String, sessionId: String) = runBlocking {
        graph.signIn(testSessionInfo(sessionId = sessionId, username = jid.substringBefore('@'), jid = jid), origin)
    }

    @Test
    fun `signing in to two servers keeps both runtimes and selects the newest`() {
        signIn("https://a.example", "ann@a.example", "sess-a")
        signIn("https://b.example", "bob@b.example", "sess-b")

        assertEquals(2, graph.runtimes.value.size)
        val a = accountKey("https://a.example", "ann@a.example")
        val b = accountKey("https://b.example", "bob@b.example")
        assertEquals(b, graph.activeKey.value)
        assertNotSame(graph.runtimes.value.getValue(a).sessionManager, graph.runtimes.value.getValue(b).sessionManager)
        assertSame(graph.runtimes.value.getValue(b).sessionManager, graph.sessionManager)
        assertEquals("https://b.example", graph.sessionOrigin.value)
        assertEquals("bob@b.example", graph.currentSession.value?.jid)
    }

    @Test
    fun `selecting an account switches the passthroughs without touching the other`() {
        signIn("https://a.example", "ann@a.example", "sess-a")
        signIn("https://b.example", "bob@b.example", "sess-b")
        val a = accountKey("https://a.example", "ann@a.example")

        runBlocking { graph.selectAccount(a) }

        assertEquals(a, graph.activeKey.value)
        assertEquals("ann@a.example", graph.currentSession.value?.jid)
        assertEquals("https://a.example", graph.sessionOrigin.value)
        assertEquals("sess-b", runBlocking { graph.runtimes.value.values.first { it.key != a }.sessionPrefs.sessionId.first() })
    }

    @Test
    fun `the same jid on two servers is two accounts with separate stores`() {
        signIn("https://a.example", "ann@x.test", "sess-1")
        signIn("https://b.example", "ann@x.test", "sess-2")

        val a = accountKey("https://a.example", "ann@x.test")
        val b = accountKey("https://b.example", "ann@x.test")
        assertEquals(setOf(a, b), graph.runtimes.value.keys)
        val runtimeA = graph.runtimes.value.getValue(a)
        val runtimeB = graph.runtimes.value.getValue(b)
        assertEquals("sess-1", runBlocking { runtimeA.sessionPrefs.sessionId.first() })
        assertEquals("sess-2", runBlocking { runtimeB.sessionPrefs.sessionId.first() })
        // Push instance names are scoped to the account, not just the JID.
        assertNotSame(runtimeA.sessionManager.pushInstanceOf("ann@x.test"), runtimeB.sessionManager.pushInstanceOf("ann@x.test"))
        assertNull(graph.runtimeForPushInstance("waddle-0000000000000000"))
        assertSame(runtimeA, graph.runtimeForPushInstance(runtimeA.sessionManager.pushInstanceOf("ann@x.test")))
    }

    @Test
    fun `signing out one account leaves the other signed in and selects it`() {
        signIn("https://a.example", "ann@a.example", "sess-a")
        signIn("https://b.example", "bob@b.example", "sess-b")
        val a = accountKey("https://a.example", "ann@a.example")
        val b = accountKey("https://b.example", "bob@b.example")

        runBlocking { graph.signOut(b) }

        assertEquals(setOf(a), graph.runtimes.value.keys)
        assertEquals(a, graph.activeKey.value)
        assertEquals("sess-a", runBlocking { graph.runtimes.value.getValue(a).sessionPrefs.sessionId.first() })
        assertEquals(listOf(a), graph.accounts.value.map { it.key })
    }

    @Test
    fun `signing out the last account returns to the sign-in screen`() {
        signIn("https://a.example", "ann@a.example", "sess-a")

        runBlocking { graph.signOut() }

        assertTrue(graph.runtimes.value.isEmpty())
        assertNull(graph.activeKey.value)
        assertNull(graph.currentSession.value)
        assertNull(graph.sessionOrigin.value)
        assertNotNull(graph.sessionManager)
    }

    @Test
    fun `a rejected session flags the account without deleting it`() {
        signIn("https://a.example", "ann@a.example", "sess-a")
        val a = accountKey("https://a.example", "ann@a.example")

        runBlocking { graph.runtimes.value.getValue(a).sessionManager.logout() }
        // The observer flags the account asynchronously.
        val deadline = System.currentTimeMillis() + 5_000
        while (graph.accounts.value.none { it.needsSignIn } && System.currentTimeMillis() < deadline) Thread.sleep(20)

        assertTrue(graph.accounts.value.single().needsSignIn)
        assertEquals(setOf(a), graph.runtimes.value.keys)
    }
}
