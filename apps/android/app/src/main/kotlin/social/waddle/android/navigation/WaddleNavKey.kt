package social.waddle.android.navigation

import androidx.navigation3.runtime.NavKey
import kotlinx.serialization.Serializable

/**
 * Navigation 3 back-stack keys (phone-first). Login is NOT a
 * destination — the app shell switches on `WaddleAppState` instead
 * (web `AppShell.vue` parity). All keys are `@Serializable` so the
 * back stack survives process death via `rememberNavBackStack`.
 */
sealed interface WaddleNavKey : NavKey {
    @Serializable
    data object Home : WaddleNavKey

    @Serializable
    data class Channel(val roomJid: String, val name: String) : WaddleNavKey

    @Serializable
    data class Dm(val peerJid: String, val name: String) : WaddleNavKey

    @Serializable
    data object DmList : WaddleNavKey

    @Serializable
    data class Thread(
        val conversationJid: String,
        val isGroupchat: Boolean,
        val threadId: String,
    ) : WaddleNavKey

    @Serializable
    data object Settings : WaddleNavKey

    /** XEP-0045 member management for one room. */
    @Serializable
    data class Members(val roomJid: String, val name: String) : WaddleNavKey

    /** Community-admin users list (owner-gated settings entry). */
    @Serializable
    data object CommunityUsers : WaddleNavKey
}
