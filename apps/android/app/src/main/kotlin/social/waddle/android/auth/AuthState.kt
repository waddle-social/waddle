package social.waddle.android.auth

import social.waddle.android.domain.auth.AuthProvider
import social.waddle.android.domain.auth.AuthSession
import social.waddle.android.domain.auth.DeviceFlow

internal sealed interface AuthState {
    data object Bootstrapping : AuthState

    data class SignedOut(
        val serverUrl: String,
        val providers: List<AuthProvider>,
        val isLoadingProviders: Boolean = false,
        val errorMessage: String? = null,
    ) : AuthState

    data class AwaitingDevice(
        val serverUrl: String,
        val provider: AuthProvider,
        val flow: DeviceFlow,
        val errorMessage: String? = null,
    ) : AuthState

    data class SignedIn(
        val serverUrl: String,
        val session: AuthSession,
    ) : AuthState
}
