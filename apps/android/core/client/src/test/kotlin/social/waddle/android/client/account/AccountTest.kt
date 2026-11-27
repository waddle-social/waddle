package social.waddle.android.client.account

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File

class AccountTest {
    @get:Rule
    val tempFolder = TemporaryFolder()

    private val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())

    @After
    fun tearDown() {
        scope.cancel()
    }

    private fun store(name: String) =
        PreferenceDataStoreFactory.create(scope = scope) { File(tempFolder.root, "$name.preferences_pb") }

    private fun registry() = AccountRegistry(store("accounts"))

    @Test
    fun `key is stable and depends on both origin and jid`() {
        val a = accountKey("https://a.example", "ann@a.example")
        assertEquals(16, a.length)
        assertEquals(a, accountKey("https://a.example", "ANN@a.example"))
        assertNotEquals(a, accountKey("https://b.example", "ann@a.example"))
        assertNotEquals(a, accountKey("https://a.example", "bob@a.example"))
    }

    @Test
    fun `identical jid on two servers are two accounts`() = runBlocking {
        val registry = registry()
        registry.upsertAndSelect(Account.create("https://a.example", "ann@x.test"))
        registry.upsertAndSelect(Account.create("https://b.example", "ann@x.test"))

        assertEquals(2, registry.accounts.first().size)
    }

    @Test
    fun `upsert selects, refreshes in place and clears needs-sign-in`() = runBlocking {
        val registry = registry()
        val ann = Account.create("https://a.example", "ann@a.example")
        registry.upsertAndSelect(ann)
        registry.upsertAndSelect(Account.create("https://b.example", "bob@b.example"))
        registry.setNeedsSignIn(ann.key, true)
        assertTrue(registry.accounts.first().first { it.key == ann.key }.needsSignIn)

        registry.upsertAndSelect(ann)

        val accounts = registry.accounts.first()
        assertEquals(listOf(ann.key, accountKey("https://b.example", "bob@b.example")), accounts.map { it.key })
        assertFalse(accounts.first().needsSignIn)
        assertEquals(ann.key, registry.activeKey.first())
    }

    @Test
    fun `removing the selected account selects the first remaining, then none`() = runBlocking {
        val registry = registry()
        val ann = Account.create("https://a.example", "ann@a.example")
        val bob = Account.create("https://b.example", "bob@b.example")
        registry.upsertAndSelect(ann)
        registry.upsertAndSelect(bob)

        registry.remove(bob.key)
        assertEquals(ann.key, registry.activeKey.first())

        registry.remove(ann.key)
        assertNull(registry.activeKey.first())
        assertTrue(registry.accounts.first().isEmpty())
    }

    @Test
    fun `select ignores unknown keys`() = runBlocking {
        val registry = registry()
        val ann = Account.create("https://a.example", "ann@a.example")
        registry.upsertAndSelect(ann)

        registry.select("deadbeefdeadbeef")

        assertEquals(ann.key, registry.activeKey.first())
    }

    @Test
    fun `stores are cached per key and isolated`() = runBlocking {
        val stores = AccountStores(File(tempFolder.root, "accounts"))
        val k = stringPreferencesKey("k")
        assertSame(stores.storeFor("aaaa"), stores.storeFor("aaaa"))

        stores.storeFor("aaaa").edit { it[k] = "one" }
        stores.storeFor("bbbb").edit { it[k] = "two" }

        assertEquals("one", stores.storeFor("aaaa").data.first()[k])
        assertEquals("two", stores.storeFor("bbbb").data.first()[k])
    }

    @Test
    fun `removing a store deletes its file`() = runBlocking {
        val dir = File(tempFolder.root, "accounts")
        val stores = AccountStores(dir)
        stores.storeFor("aaaa").edit { it[stringPreferencesKey("k")] = "v" }
        assertTrue(File(dir, "session_aaaa.preferences_pb").exists())

        stores.remove("aaaa")

        assertFalse(File(dir, "session_aaaa.preferences_pb").exists())
    }

    @Test
    fun `unsafe keys are rejected`() {
        val stores = AccountStores(File(tempFolder.root, "accounts"))
        val failure = runCatching { stores.storeFor("../x") }.exceptionOrNull()
        assertNotNull(failure)
    }

    @Test
    fun `legacy session moves into the first account once`() = runBlocking {
        val legacy = store("legacy")
        val sid = stringPreferencesKey("session_id")
        legacy.edit {
            it[sid] = "sess-1"
            it[stringPreferencesKey("owner_bare_jid")] = "ann@a.example"
            it[stringPreferencesKey("server_url")] = "https://a.example"
            it[stringPreferencesKey("resource_suffix")] = "deadbeef"
        }
        val registry = registry()
        val stores = AccountStores(File(tempFolder.root, "accounts"))

        val account = LegacySessionMigration.migrate(legacy, registry, stores, "https://default.example")

        assertNotNull(account)
        assertEquals("https://a.example", account!!.origin)
        assertEquals(account.key, registry.activeKey.first())
        assertEquals("sess-1", stores.storeFor(account.key).data.first()[sid])
        assertEquals("deadbeef", stores.storeFor(account.key).data.first()[stringPreferencesKey("resource_suffix")])
        assertNull(legacy.data.first()[sid])
        assertNull(LegacySessionMigration.migrate(legacy, registry, stores, "https://default.example"))
    }

    @Test
    fun `legacy without a stored origin uses the default`() = runBlocking {
        val legacy = store("legacy")
        legacy.edit {
            it[stringPreferencesKey("session_id")] = "s"
            it[stringPreferencesKey("owner_bare_jid")] = "ann@a.example"
        }

        val account = LegacySessionMigration.migrate(
            legacy,
            registry(),
            AccountStores(File(tempFolder.root, "accounts")),
            "https://default.example",
        )

        assertEquals("https://default.example", account?.origin)
    }

    @Test
    fun `no legacy session means nothing to migrate`() = runBlocking {
        val account = LegacySessionMigration.migrate(
            store("legacy"),
            registry(),
            AccountStores(File(tempFolder.root, "accounts")),
            "https://default.example",
        )
        assertNull(account)
    }
}
