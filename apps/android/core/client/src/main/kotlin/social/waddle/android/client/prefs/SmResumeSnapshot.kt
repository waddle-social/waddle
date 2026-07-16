package social.waddle.android.client.prefs

import kotlinx.serialization.Serializable
import social.waddle.client.ffi.WaddleSmResumeEntry
import social.waddle.client.ffi.WaddleSmResumeState
import java.time.Instant

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
    val queuedEntries: List<SmResumeEntrySnapshot> = emptyList(),
)

@Serializable
data class SmResumeEntrySnapshot(
    val stanzaXml: String,
    val sentAtEpochSeconds: Long,
    val sentAtNanoseconds: Int,
)

fun WaddleSmResumeState.toSnapshot(): SmResumeSnapshot = SmResumeSnapshot(
    previd = previd,
    inboundH = inboundH,
    outboundH = outboundH,
    maxResumeSeconds = maxResumeSeconds,
    queuedEntries = queuedEntries.map {
        SmResumeEntrySnapshot(it.stanzaXml, it.sentAt.epochSecond, it.sentAt.nano)
    },
)

fun SmResumeSnapshot.toFfi(): WaddleSmResumeState = WaddleSmResumeState(
    previd = previd,
    inboundH = inboundH,
    outboundH = outboundH,
    maxResumeSeconds = maxResumeSeconds,
    queuedEntries = queuedEntries.map {
        WaddleSmResumeEntry(
            it.stanzaXml,
            Instant.ofEpochSecond(it.sentAtEpochSeconds, it.sentAtNanoseconds.toLong()),
        )
    },
)
