package social.waddle.android.domain

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import social.waddle.android.ffi.WaddleClientHandle
import uniffi.waddle_xmpp_client.WaddleAvatar

public class AvatarRepository(private val client: WaddleClientHandle) {
    private val mutable = MutableStateFlow<Map<String, WaddleAvatar>>(emptyMap())
    public val byJid: StateFlow<Map<String, WaddleAvatar>> = mutable.asStateFlow()

    public suspend fun fetch(jid: String): WaddleAvatar? {
        val cached = mutable.value[jid]
        if (cached != null) return cached
        val avatar = client.requestAvatar(jid) ?: return null
        mutable.value = mutable.value + (jid to avatar)
        return avatar
    }
}
