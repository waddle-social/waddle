package social.waddle.android.client

import social.waddle.client.ffi.WaddleClient
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleConfig

/**
 * Seam over `WaddleClient` construction so `XmppSessionManager` is
 * testable on the JVM without loading the native library. The FFI client
 * is one-shot per connection: every reconnect attempt builds a fresh
 * config, bridge, and client.
 */
fun interface ClientFactory {
    fun create(config: WaddleConfig): WaddleClientInterface
}

/** Production factory: constructs the UniFFI-backed Rust client. */
class RustClientFactory : ClientFactory {
    override fun create(config: WaddleConfig): WaddleClientInterface =
        WaddleClient(config)
}
