package social.waddle.android.client.prefs

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.core.stringSetPreferencesKey
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import kotlinx.serialization.json.Json
import kotlin.random.Random

/**
 * Session-scoped persistence (`session.preferences_pb`).
 *
 * Delivery state has exactly one authority: [deliveryJournal]. Account
 * metadata, SM snapshots, outbound rows, and terminal intents are encoded
 * under one versioned key and changed through [updateDeliveryJournal].
 */
class SessionPrefs(
    private val dataStore: DataStore<Preferences>,
    private val json: Json = Json { ignoreUnknownKeys = true },
) {
    val deliveryJournal: Flow<DeliveryJournal> =
        dataStore.data.map(::decodeDeliveryJournal)

    val ownerBareJid: Flow<String?> =
        deliveryJournal.map { journal -> journal.activeOwnerBareJid }

    val sessionId: Flow<String?> = deliveryJournal.map { journal ->
        journal.activeOwnerBareJid
            ?.let(journal.owners::get)
            ?.session
            ?.sessionId
    }

    val joinedRooms: Flow<Set<String>> = dataStore.data.map { it[KEY_JOINED_ROOMS] ?: emptySet() }

    val lastSeen: Flow<Map<String, String>> = dataStore.data.map { prefs ->
        prefs[KEY_LAST_SEEN]?.let { stored ->
            runCatching { json.decodeFromString<Map<String, String>>(stored) }.getOrNull()
        } ?: emptyMap()
    }

    val resumeCursors: Flow<Map<String, ResumeCursor>> = dataStore.data.map { prefs ->
        prefs[KEY_RESUME_CURSORS]?.let { stored ->
            runCatching { json.decodeFromString<Map<String, ResumeCursor>>(stored) }.getOrNull()
        } ?: emptyMap()
    }

    /**
     * Select and persist one authenticated owner without touching any
     * foreign owner's delivery state.
     */
    suspend fun activateSession(ownerBareJid: String, sessionId: String) {
        updateDeliveryJournal { journal ->
            val owner = journal.owners[ownerBareJid] ?: DeliveryOwnerJournal()
            DeliveryJournalMutation(
                journal = journal.copy(
                    activeOwnerBareJid = ownerBareJid,
                    owners = journal.owners + (
                        ownerBareJid to owner.copy(
                            session = DeliverySessionMetadata(sessionId),
                        )
                    ),
                ),
                result = Unit,
            )
        }
    }

    /**
     * The sole delivery-journal read-modify-write boundary. DataStore
     * serializes edits; [transform] returns both the new journal and the
     * exact operation result observed from that same committed state.
     */
    suspend fun <T> updateDeliveryJournal(
        transform: (DeliveryJournal) -> DeliveryJournalMutation<T>,
    ): T {
        var outcome: Result<T>? = null
        dataStore.edit { prefs ->
            val mutation = transform(decodeDeliveryJournal(prefs))
            require(mutation.journal.schemaVersion == DeliveryJournal.CURRENT_SCHEMA_VERSION)
            prefs[KEY_DELIVERY_JOURNAL] = json.encodeToString(mutation.journal)
            outcome = Result.success(mutation.result)
        }
        return checkNotNull(outcome) { "delivery journal edit did not run" }.getOrThrow()
    }

    suspend fun setJoinedRooms(rooms: Set<String>) {
        dataStore.edit { it[KEY_JOINED_ROOMS] = rooms }
    }

    suspend fun setLastSeen(conversationJid: String, marker: String) {
        dataStore.edit { prefs ->
            val current = prefs[KEY_LAST_SEEN]?.let { stored ->
                runCatching { json.decodeFromString<Map<String, String>>(stored) }.getOrNull()
            } ?: emptyMap()
            prefs[KEY_LAST_SEEN] = json.encodeToString(current + (conversationJid to marker))
        }
    }

    suspend fun setResumeCursors(cursors: Map<String, ResumeCursor>) {
        dataStore.edit { prefs ->
            if (cursors.isEmpty()) {
                prefs.remove(KEY_RESUME_CURSORS)
            } else {
                prefs[KEY_RESUME_CURSORS] = json.encodeToString(cursors)
            }
        }
    }

    /**
     * Stable-per-install 8-hex suffix for the XMPP resource
     * (`waddle-android-<suffix>`); generated once, atomically, and kept
     * across [clear] so reinstalls — not logouts — rotate the resource.
     */
    suspend fun resourceSuffix(): String {
        var suffix = ""
        dataStore.edit { prefs ->
            suffix = prefs[KEY_RESOURCE_SUFFIX] ?: generateResourceSuffix().also {
                prefs[KEY_RESOURCE_SUFFIX] = it
            }
        }
        return suffix
    }

    /**
     * Logout: purge only the active owner's session/SM/rows/intents while
     * preserving every foreign owner and the per-install resource suffix.
     * Other session-scoped projection keys belong to the active owner and
     * are cleared for privacy.
     */
    suspend fun clear() {
        dataStore.edit { prefs ->
            val suffix = prefs[KEY_RESOURCE_SUFFIX]
            val journal = decodeDeliveryJournal(prefs)
            val activeOwner = journal.activeOwnerBareJid
            val retainedJournal = journal.copy(
                activeOwnerBareJid = null,
                owners = if (activeOwner == null) {
                    journal.owners
                } else {
                    journal.owners - activeOwner
                },
            )
            prefs.clear()
            suffix?.let { prefs[KEY_RESOURCE_SUFFIX] = it }
            if (retainedJournal.owners.isNotEmpty()) {
                prefs[KEY_DELIVERY_JOURNAL] = json.encodeToString(retainedJournal)
            }
        }
    }

    private fun decodeDeliveryJournal(prefs: Preferences): DeliveryJournal {
        val stored = prefs[KEY_DELIVERY_JOURNAL] ?: return DeliveryJournal()
        return json.decodeFromString<DeliveryJournal>(stored).also { journal ->
            require(journal.schemaVersion == DeliveryJournal.CURRENT_SCHEMA_VERSION) {
                "unsupported delivery journal schema version: ${journal.schemaVersion}"
            }
        }
    }

    private fun generateResourceSuffix(): String =
        buildString(RESOURCE_SUFFIX_LENGTH) {
            repeat(RESOURCE_SUFFIX_LENGTH) { append(HEX_ALPHABET.random(Random)) }
        }

    private companion object {
        val KEY_DELIVERY_JOURNAL = stringPreferencesKey("delivery_journal_v1")
        val KEY_JOINED_ROOMS = stringSetPreferencesKey("joined_rooms")
        val KEY_LAST_SEEN = stringPreferencesKey("last_seen")
        val KEY_RESUME_CURSORS = stringPreferencesKey("resume_cursors")
        val KEY_RESOURCE_SUFFIX = stringPreferencesKey("resource_suffix")

        const val RESOURCE_SUFFIX_LENGTH = 8
        const val HEX_ALPHABET = "0123456789abcdef"
    }
}
