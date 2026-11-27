package social.waddle.android.client.account

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.flow.first

/**
 * One-time move of the pre-accounts single session file into the first
 * account, so the developer's own session survives the upgrade. The
 * legacy file is emptied afterwards, which makes the move idempotent.
 */
object LegacySessionMigration {
    private val KEY_SESSION_ID = stringPreferencesKey("session_id")
    private val KEY_OWNER = stringPreferencesKey("owner_bare_jid")
    private val KEY_SERVER_URL = stringPreferencesKey("server_url")

    /** Returns the migrated account, or `null` when there was nothing to move. */
    suspend fun migrate(
        legacy: DataStore<Preferences>,
        registry: AccountRegistry,
        stores: AccountStoreProvider,
        defaultOrigin: String,
    ): Account? {
        if (registry.accounts.first().isNotEmpty()) return null
        val snapshot = legacy.data.first()
        if (snapshot[KEY_SESSION_ID] == null) return null
        val owner = snapshot[KEY_OWNER] ?: return null
        val origin = snapshot[KEY_SERVER_URL]?.takeIf { it.isNotBlank() } ?: defaultOrigin
        val account = Account.create(origin, owner)
        // Copy first, register second, clear last: a crash in between leaves
        // the legacy file intact and the next launch simply repeats the move.
        stores.storeFor(account.key).updateData { snapshot }
        registry.upsertAndSelect(account)
        legacy.edit { it.clear() }
        return account
    }
}
