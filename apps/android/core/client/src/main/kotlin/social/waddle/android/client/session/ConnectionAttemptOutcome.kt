package social.waddle.android.client.session

import kotlinx.coroutines.CoroutineScope
import social.waddle.android.client.SaslRetryDisposition
import social.waddle.android.client.SessionLifecycleRef
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleSaslCondition

/** Exhaustive result of one physical connection attempt. */
internal sealed interface ConnectionAttemptOutcome {
    data class TerminalAuthenticationFailure(
        val condition: WaddleSaslCondition,
        val disposition: SaslRetryDisposition,
    ) : ConnectionAttemptOutcome

    data object CleanPostReadyDisconnect : ConnectionAttemptOutcome
    data object RetryableFailure : ConnectionAttemptOutcome
    data object FencedOrReplaced : ConnectionAttemptOutcome
}

/** Invoked exactly once after a ready attempt has passed its ownership fence. */
internal typealias SessionReadyListener = (
    attemptScope: CoroutineScope,
    client: WaddleClientInterface,
    session: WaddleSessionInfo,
    freshStream: Boolean,
    lifecycle: SessionLifecycleRef,
) -> Unit
