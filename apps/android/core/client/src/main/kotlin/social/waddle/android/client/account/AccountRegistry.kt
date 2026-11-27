package social.waddle.android.client.account

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json

/**
 * The list of accounts and which one the UI shows. Non-secret metadata
 * only; each account's credential and state live in its own store (see
 * [AccountStores]).
 */
class AccountRegistry(
    private val dataStore: DataStore<Preferences>,
    private val json: Json = Json { ignoreUnknownKeys = true },
) {
    /** Accounts in the order they were added. */
    val accounts: Flow<List<Account>> = dataStore.data.map { decode(it[KEY_ACCOUNTS]) }

    /** The selected account's key, or `null` when there are none. */
    val activeKey: Flow<String?> = dataStore.data.map { prefs ->
        val accounts = decode(prefs[KEY_ACCOUNTS])
        prefs[KEY_ACTIVE]?.takeIf { key -> accounts.any { it.key == key } } ?: accounts.firstOrNull()?.key
    }

    /** Adds the account, or refreshes an existing one in place (same key), and selects it. */
    suspend fun upsertAndSelect(account: Account) {
        dataStore.edit { prefs ->
            val current = decode(prefs[KEY_ACCOUNTS])
            val existing = current.firstOrNull { it.key == account.key }
            val merged = existing?.copy(origin = account.origin, bareJid = account.bareJid, needsSignIn = false)
                ?: account
            val next = if (existing == null) {
                current + merged
            } else {
                current.map { if (it.key == merged.key) merged else it }
            }
            prefs[KEY_ACCOUNTS] = json.encodeToString(ACCOUNTS_SERIALIZER, next)
            prefs[KEY_ACTIVE] = merged.key
        }
    }

    suspend fun select(key: String) {
        dataStore.edit { prefs ->
            if (decode(prefs[KEY_ACCOUNTS]).any { it.key == key }) prefs[KEY_ACTIVE] = key
        }
    }

    suspend fun setNeedsSignIn(key: String, needsSignIn: Boolean) {
        update(key) { it.copy(needsSignIn = needsSignIn) }
    }

    suspend fun rename(key: String, label: String) {
        update(key) { it.copy(label = label.trim().ifEmpty { it.label }) }
    }

    /** Drops the entry; if it was selected the first remaining account takes over. */
    suspend fun remove(key: String) {
        dataStore.edit { prefs ->
            val next = decode(prefs[KEY_ACCOUNTS]).filterNot { it.key == key }
            prefs[KEY_ACCOUNTS] = json.encodeToString(ACCOUNTS_SERIALIZER, next)
            if (prefs[KEY_ACTIVE] == key) {
                next.firstOrNull()?.let { prefs[KEY_ACTIVE] = it.key } ?: prefs.remove(KEY_ACTIVE)
            }
        }
    }

    private suspend fun update(key: String, transform: (Account) -> Account) {
        dataStore.edit { prefs ->
            val next = decode(prefs[KEY_ACCOUNTS]).map { if (it.key == key) transform(it) else it }
            prefs[KEY_ACCOUNTS] = json.encodeToString(ACCOUNTS_SERIALIZER, next)
        }
    }

    private fun decode(stored: String?): List<Account> =
        stored?.let { runCatching { json.decodeFromString(ACCOUNTS_SERIALIZER, it) }.getOrNull() }.orEmpty()

    private companion object {
        val KEY_ACCOUNTS = stringPreferencesKey("accounts")
        val KEY_ACTIVE = stringPreferencesKey("active_key")
        val ACCOUNTS_SERIALIZER = ListSerializer(Account.serializer())
    }
}
