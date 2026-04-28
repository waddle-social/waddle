package social.waddle.android.ffi

/**
 * Loads the JNI shared library that backs the uniffi-generated Kotlin
 * bindings. Must be called once during process startup, before any
 * [uniffi.waddle_xmpp_client.WaddleClient] is constructed. Idempotent.
 */
public object WaddleNativeLoader {
    @Volatile
    private var loaded: Boolean = false
    private val lock = Any()

    public fun load() {
        if (loaded) return
        synchronized(lock) {
            if (loaded) return
            System.loadLibrary("waddle_xmpp_client_ffi")
            loaded = true
        }
    }
}
