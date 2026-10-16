package social.waddle.android.feature.you

import social.waddle.android.client.ConnectionState
import social.waddle.client.ffi.WaddleMood

/** Who the You tab is about, as of now. */
internal data class YouIdentity(
    val name: String,
    val jid: String?,
    val connection: ConnectionState = ConnectionState.Idle,
    val mood: WaddleMood? = null,
)
