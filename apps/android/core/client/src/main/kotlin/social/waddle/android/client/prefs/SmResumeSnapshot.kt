package social.waddle.android.client.prefs

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import social.waddle.client.ffi.WaddleResumeStanza
import social.waddle.client.ffi.WaddleResumeStanzaKind
import social.waddle.client.ffi.WaddleResumeXmlAttribute
import social.waddle.client.ffi.WaddleResumeXmlLocalName
import social.waddle.client.ffi.WaddleResumeXmlName
import social.waddle.client.ffi.WaddleResumeXmlNamespace
import social.waddle.client.ffi.WaddleResumeXmlToken
import social.waddle.client.ffi.WaddleResumeXmlValue
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
    val stanza: SmResumeStanzaSnapshot,
    val sentAtEpochSeconds: Long,
    val sentAtNanoseconds: Int,
)

@Serializable
enum class SmResumeStanzaKind {
    MESSAGE,
    PRESENCE,
    IQ,
}

@Serializable
data class SmResumeXmlName(
    val namespace: String,
    val localName: String,
)

@Serializable
data class SmResumeXmlAttribute(
    val name: SmResumeXmlName,
    val value: String,
)

@Serializable
sealed interface SmResumeXmlToken {
    @Serializable
    @SerialName("start")
    data class Start(
        val name: SmResumeXmlName,
        val attributes: List<SmResumeXmlAttribute>,
    ) : SmResumeXmlToken

    @Serializable
    @SerialName("text")
    data class Text(val value: String) : SmResumeXmlToken

    @Serializable
    @SerialName("end")
    data object End : SmResumeXmlToken
}

@Serializable
data class SmResumeStanzaSnapshot(
    val stanzaKind: SmResumeStanzaKind,
    val tokens: List<SmResumeXmlToken>,
)

fun WaddleSmResumeState.toSnapshot(): SmResumeSnapshot = SmResumeSnapshot(
    previd = previd,
    inboundH = inboundH,
    outboundH = outboundH,
    maxResumeSeconds = maxResumeSeconds,
    queuedEntries = queuedEntries.map {
        SmResumeEntrySnapshot(it.stanza.toSnapshot(), it.sentAt.epochSecond, it.sentAt.nano)
    },
)

fun SmResumeSnapshot.toFfi(): WaddleSmResumeState = WaddleSmResumeState(
    previd = previd,
    inboundH = inboundH,
    outboundH = outboundH,
    maxResumeSeconds = maxResumeSeconds,
    queuedEntries = queuedEntries.map {
        WaddleSmResumeEntry(
            it.stanza.toFfi(),
            Instant.ofEpochSecond(it.sentAtEpochSeconds, it.sentAtNanoseconds.toLong()),
        )
    },
)

private fun WaddleResumeStanza.toSnapshot(): SmResumeStanzaSnapshot = SmResumeStanzaSnapshot(
    stanzaKind = when (stanzaKind) {
        WaddleResumeStanzaKind.MESSAGE -> SmResumeStanzaKind.MESSAGE
        WaddleResumeStanzaKind.PRESENCE -> SmResumeStanzaKind.PRESENCE
        WaddleResumeStanzaKind.IQ -> SmResumeStanzaKind.IQ
    },
    tokens = tokens.map { token ->
        when (token) {
            is WaddleResumeXmlToken.Start -> SmResumeXmlToken.Start(
                name = token.name.toSnapshot(),
                attributes = token.attributes.map { attribute ->
                    SmResumeXmlAttribute(
                        name = attribute.name.toSnapshot(),
                        value = attribute.value.value,
                    )
                },
            )
            is WaddleResumeXmlToken.Text -> SmResumeXmlToken.Text(token.value.value)
            WaddleResumeXmlToken.End -> SmResumeXmlToken.End
        }
    },
)

private fun WaddleResumeXmlName.toSnapshot(): SmResumeXmlName = SmResumeXmlName(
    namespace = namespace.value,
    localName = localName.value,
)

private fun SmResumeStanzaSnapshot.toFfi(): WaddleResumeStanza {
    require(tokens.size <= MAX_RESUME_XML_TOKENS) { "resume stanza token limit exceeded" }
    var depth = 0
    tokens.forEach { token ->
        when (token) {
            is SmResumeXmlToken.Start -> {
                depth += 1
                require(depth <= MAX_RESUME_XML_DEPTH) { "resume stanza depth limit exceeded" }
            }
            is SmResumeXmlToken.Text -> require(depth > 0) { "resume stanza text is outside a root" }
            SmResumeXmlToken.End -> {
                require(depth > 0) { "resume stanza end token is unbalanced" }
                depth -= 1
            }
        }
    }
    require(depth == 0) { "resume stanza token sequence is unbalanced" }
    return WaddleResumeStanza(
        stanzaKind = when (stanzaKind) {
            SmResumeStanzaKind.MESSAGE -> WaddleResumeStanzaKind.MESSAGE
            SmResumeStanzaKind.PRESENCE -> WaddleResumeStanzaKind.PRESENCE
            SmResumeStanzaKind.IQ -> WaddleResumeStanzaKind.IQ
        },
        tokens = tokens.map { token ->
            when (token) {
                is SmResumeXmlToken.Start -> WaddleResumeXmlToken.Start(
                    name = token.name.toFfi(),
                    attributes = token.attributes.map { attribute ->
                        WaddleResumeXmlAttribute(
                            name = attribute.name.toFfi(),
                            value = WaddleResumeXmlValue(attribute.value),
                        )
                    },
                )
                is SmResumeXmlToken.Text -> WaddleResumeXmlToken.Text(
                    WaddleResumeXmlValue(token.value),
                )
                SmResumeXmlToken.End -> WaddleResumeXmlToken.End
            }
        },
    )
}

private fun SmResumeXmlName.toFfi(): WaddleResumeXmlName = WaddleResumeXmlName(
    namespace = WaddleResumeXmlNamespace(namespace),
    localName = WaddleResumeXmlLocalName(localName),
)

private const val MAX_RESUME_XML_TOKENS = 16_384
private const val MAX_RESUME_XML_DEPTH = 64
