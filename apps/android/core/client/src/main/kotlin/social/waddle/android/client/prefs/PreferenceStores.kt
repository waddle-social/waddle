package social.waddle.android.client.prefs

import android.content.Context
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.preferencesDataStore

/**
 * The two on-device Preferences DataStore files (§8 of the architecture
 * plan). Process-wide singletons via the `preferencesDataStore` delegate;
 * `:app` wires them into [SessionPrefs]/[UserPrefs] by construction. The app
 * declares no secondary Android process, so terminal receipt claims rely on
 * this single-process DataStore contract rather than a distributed lease.
 */
val Context.sessionPreferencesDataStore: DataStore<Preferences> by preferencesDataStore(name = "session")

val Context.userPreferencesDataStore: DataStore<Preferences> by preferencesDataStore(name = "user")
