package social.waddle.android.client

/**
 * App-shell gate (the analog of web `connectionStore.appState` gating
 * `AppShell.vue`): whether a signed-in session exists at all, independent
 * of the live [ConnectionState].
 */
sealed interface WaddleAppState {
    data object Loading : WaddleAppState

    data object SignedOut : WaddleAppState

    data class Error(val message: String) : WaddleAppState

    data object Ready : WaddleAppState
}
