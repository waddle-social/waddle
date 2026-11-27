package social.waddle.android.client.account

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.Preferences
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import java.io.File
import java.util.concurrent.ConcurrentHashMap

/** Where each account's session DataStore lives; tests swap in memory-backed ones. */
interface AccountStoreProvider {
    fun storeFor(key: String): DataStore<Preferences>

    /** Drops the account's store and everything persisted in it. */
    fun remove(key: String)
}

/**
 * One session DataStore file per account (`session_<key>.preferences_pb`).
 * A second DataStore over the same file crashes ("multiple DataStores
 * active for the same file"), so each key is created once and cached for
 * the life of the process. [remove] cancels the store's scope before
 * deleting the file so nothing can recreate it.
 */
class AccountStores(private val directory: File) : AccountStoreProvider {
    private class Entry(val scope: CoroutineScope, val store: DataStore<Preferences>)

    private val entries = ConcurrentHashMap<String, Entry>()

    override fun storeFor(key: String): DataStore<Preferences> {
        requireSafe(key)
        return entries.computeIfAbsent(key) { create(key) }.store
    }

    private fun create(key: String): Entry {
        val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())
        return Entry(scope, PreferenceDataStoreFactory.create(scope = scope) { fileFor(key) })
    }

    /** Stops the account's store and deletes its file. */
    override fun remove(key: String) {
        requireSafe(key)
        entries.remove(key)?.scope?.cancel()
        fileFor(key).delete()
    }

    private fun requireSafe(key: String) = require(key.isNotEmpty() && key.all { it.isLetterOrDigit() }) {
        "unsafe account key"
    }

    private fun fileFor(key: String): File {
        directory.mkdirs()
        return File(directory, "session_$key.preferences_pb")
    }
}
