package social.waddle.android.feature.login

import kotlinx.coroutines.flow.StateFlow
import social.waddle.android.client.server.ServerDiscovery
import social.waddle.android.client.server.ServerSelection

/** The slice of server selection the login flow uses, as a test seam. */
interface LoginServerGateway {
    /** The origin in use. */
    val current: StateFlow<String>

    /** The build's default origin, the fallback a person can return to. */
    val default: String

    /** Validate a typed address and resolve it to the origin serving the auth API. */
    suspend fun resolve(input: String): Result<String>

    /** Persist and switch to a resolved origin. */
    suspend fun select(origin: String)
}

class SelectionLoginServerGateway(
    private val selection: ServerSelection,
    private val discovery: ServerDiscovery,
) : LoginServerGateway {
    override val current: StateFlow<String> = selection.current
    override val default: String = selection.default

    override suspend fun resolve(input: String): Result<String> = discovery.discover(input)

    override suspend fun select(origin: String) = selection.select(origin)
}
