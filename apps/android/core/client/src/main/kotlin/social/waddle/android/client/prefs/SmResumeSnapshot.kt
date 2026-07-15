package social.waddle.android.client.prefs

import kotlinx.serialization.Serializable
import social.waddle.client.ffi.WaddleSmResumeState

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
    val queuedStanzasXml: List<String> = emptyList(),
)

fun WaddleSmResumeState.toSnapshot(): SmResumeSnapshot = SmResumeSnapshot(
    previd = previd,
    inboundH = inboundH,
    outboundH = outboundH,
    maxResumeSeconds = maxResumeSeconds,
    queuedStanzasXml = queuedStanzasXml,
)

fun SmResumeSnapshot.toFfi(): WaddleSmResumeState = WaddleSmResumeState(
    previd = previd,
    inboundH = inboundH,
    outboundH = outboundH,
    maxResumeSeconds = maxResumeSeconds,
    queuedStanzasXml = queuedStanzasXml,
)
