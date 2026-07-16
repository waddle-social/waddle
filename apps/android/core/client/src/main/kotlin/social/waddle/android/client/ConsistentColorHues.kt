package social.waddle.android.client

import social.waddle.client.ffi.consistentColorHue
import java.util.concurrent.ConcurrentHashMap

/**
 * XEP-0392 consistent-color hue cache. The hue comes from the shared
 * Rust `consistent_color_hue` FFI export — the exact implementation
 * the web client uses via WASM — so a user's color matches across
 * every Waddle surface. Inputs are display names, verbatim (web
 * `AppAvatar` parity). The FFI call SHA-1s the input on every
 * invocation, so results are memoized per input.
 */
class ConsistentColorHues(
    private val hueOf: (String) -> Double = { input -> consistentColorHue(input) },
) {
    private val cache = ConcurrentHashMap<String, Double>()

    /** Hue in degrees (`0.0..360.0`) for [input]. */
    fun hue(input: String): Double = cache.getOrPut(input) { hueOf(input) }
}
