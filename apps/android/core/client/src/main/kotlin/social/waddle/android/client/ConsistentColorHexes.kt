package social.waddle.android.client

import social.waddle.client.ffi.consistentColorHex
import java.util.concurrent.ConcurrentHashMap

/**
 * XEP-0392 consistent-color cache. The color comes from the shared Rust
 * `consistent_color_hex` FFI export (SHA-1 hue through HSLuv
 * `hsluvToRgb`), the exact implementation the web client uses via WASM,
 * so a user's color matches across every Waddle surface. Inputs are
 * display names, verbatim (web `AppAvatar` parity). The FFI call SHA-1s
 * the input on every invocation, so results are memoized per input and
 * lightness.
 */
class ConsistentColorHexes(
    private val hexOf: (String, Double, Double) -> String = { input, saturation, lightness ->
        consistentColorHex(input, saturation, lightness)
    },
) {
    private val cache = ConcurrentHashMap<Triple<String, Double, Double>, String>()

    /** `#rrggbb` for [input] at HSLuv [saturation] and [lightness] (0-100). */
    fun hex(input: String, saturation: Double, lightness: Double): String =
        cache.getOrPut(Triple(input, saturation, lightness)) { hexOf(input, saturation, lightness) }
}
