package social.waddle.android.client

/** Monotonic, runtime-local identity for one logged-in ownership generation. */
@JvmInline
internal value class RuntimeGeneration(val value: Long)
