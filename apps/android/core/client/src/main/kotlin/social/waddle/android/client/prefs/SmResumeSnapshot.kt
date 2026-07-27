package social.waddle.android.client.prefs

import kotlinx.serialization.Serializable
import social.waddle.client.ffi.WaddleSmResumeState
import social.waddle.client.ffi.WaddleUnhandledOutboundEntry

/**
 * JSON-serializable mirror of the FFI [WaddleSmResumeState], persisted in
 * [SessionPrefs] so an XEP-0198 `<resume/>` can survive a process death
 * (the Android analog of web localStorage `waddle.chat.sm-resume`).
 */
@Serializable
data class SmResumeSnapshot(
    val previd: String,
    val inboundH: UInt,
    val outboundH: UInt,
    val maxResumeSeconds: UInt? = null,
    val unhandledOutboundEntries: List<SmUnhandledOutboundEntry> = emptyList(),
)

@Serializable
data class SmUnhandledOutboundEntry(
    val xml: String,
    val sentAt: String,
)

fun WaddleSmResumeState.toSnapshot(): SmResumeSnapshot = SmResumeSnapshot(
    previd = previd,
    inboundH = inboundH,
    outboundH = outboundH,
    maxResumeSeconds = maxResumeSeconds,
    unhandledOutboundEntries = unhandledOutboundEntries.map { entry ->
        SmUnhandledOutboundEntry(xml = entry.xml, sentAt = entry.sentAt)
    },
)

fun SmResumeSnapshot.toFfi(): WaddleSmResumeState = WaddleSmResumeState(
    previd = previd,
    inboundH = inboundH,
    outboundH = outboundH,
    maxResumeSeconds = maxResumeSeconds,
    unhandledOutboundEntries = unhandledOutboundEntries.map { entry ->
        WaddleUnhandledOutboundEntry(xml = entry.xml, sentAt = entry.sentAt)
    },
)
