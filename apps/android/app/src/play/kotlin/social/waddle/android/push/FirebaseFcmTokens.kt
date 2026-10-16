package social.waddle.android.push

import com.google.android.gms.tasks.Task
import com.google.firebase.messaging.FirebaseMessaging
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlin.coroutines.resume

/** [FcmTokens] over Firebase Messaging (initialized by `Distribution`). */
class FirebaseFcmTokens : FcmTokens {
    private val messaging: FirebaseMessaging get() = FirebaseMessaging.getInstance()

    override suspend fun token(): String? = messaging.token.awaitOrNull()?.takeIf { it.isNotBlank() }

    override suspend fun delete(): Boolean = messaging.deleteToken().awaitSuccess()

    override fun setAutoInit(enabled: Boolean) {
        messaging.isAutoInitEnabled = enabled
    }
}

/** The task's result; `null` when it failed or was cancelled. */
private suspend fun <T> Task<T>.awaitOrNull(): T? = suspendCancellableCoroutine { continuation ->
    addOnCompleteListener { task ->
        continuation.resume(if (task.isSuccessful) task.result else null)
    }
}

private suspend fun Task<*>.awaitSuccess(): Boolean = suspendCancellableCoroutine { continuation ->
    addOnCompleteListener { task -> continuation.resume(task.isSuccessful) }
}
