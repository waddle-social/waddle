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
    data object Settings : WaddleNavKey
}
