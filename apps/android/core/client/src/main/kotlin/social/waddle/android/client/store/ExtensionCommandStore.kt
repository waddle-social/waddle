package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import social.waddle.android.client.ExtensionCommand

/**
 * Session cache of the discovered `urn:waddle:extension:1` command
 * set: seeded by one XEP-0050 discovery walk per session (the command
 * roster only changes on server redeploys), wiped on login/logout via
 * [SessionStores.clear]. Failed or empty discoveries are never
 * cached — the next slash keystroke retries.
 */
class ExtensionCommandStore {
    private val _commands = MutableStateFlow<List<ExtensionCommand>?>(null)

    /** `null` until the first successful discovery of the session. */
    val commands: StateFlow<List<ExtensionCommand>?> = _commands.asStateFlow()

    /** A discovery answered with commands: cache them for the session. */
    fun applyDiscovered(commands: List<ExtensionCommand>) {
        _commands.value = commands
    }

    fun clear() {
        _commands.value = null
    }
}
