package social.waddle.android.client.calls

import social.waddle.android.client.FakeWaddleClient
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.testPresence
import social.waddle.client.ffi.WaddleCallEvent
import social.waddle.client.ffi.WaddleCallEventKind
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleJingleReason
import social.waddle.client.ffi.WaddleLiveKitJoin
import social.waddle.client.ffi.WaddleMujiPresence
import social.waddle.client.ffi.WaddlePresence
import java.util.Base64

/**
 * Shared fixture for the CallStore suites ([CallStoreTest],
 * [CallStoreTieBreakTest], [CallStoreMucTest]): a real
 * [ClientCallSignaling] wire path into the recording
 * [FakeWaddleClient], plus typed event builders.
 */
internal const val OWN_BARE = "alice@waddle.test"
internal const val OWN_FULL = "alice@waddle.test/waddle-android-1"
internal const val PEER_BARE = "bob@waddle.test"
internal const val PEER_FULL = "bob@waddle.test/phone"

internal const val ROOM_JID = "channel@muc.waddle.test"
internal const val SELF_NICK = "alice"
internal const val MIXER_JID = "calls.waddle.test"

internal val audio = WaddleCallMedia(audio = true, video = false)
internal val video = WaddleCallMedia(audio = true, video = true)
internal val join = WaddleLiveKitJoin(
    url = "wss://livekit.waddle.test",
    room = "dm-room",
    identity = OWN_FULL,
    token = "jwt",
)

/** A LiveKit join JWT whose payload carries the given `exp` claim. */
internal fun liveKitToken(expEpochSeconds: Long): String {
    val payload = Base64.getUrlEncoder().withoutPadding()
        .encodeToString("""{"exp":$expEpochSeconds}""".toByteArray(Charsets.UTF_8))
    return "hdr.$payload.sig"
}

/** Far-future group-call join credentials for the mixer's accept. */
internal val mucJoin = WaddleLiveKitJoin(
    url = "wss://livekit.waddle.test",
    room = ROOM_JID,
    identity = OWN_FULL,
    token = liveKitToken(expEpochSeconds = 4_102_444_800), // 2100-01-01
)

internal class Fixture(sid: () -> String = { "c-fixed" }) {
    val client = FakeWaddleClient()
    val activeSession = ActiveSession { }
    val sessionCache = MucCallSessionCache(InMemoryPreferencesDataStore())
    val store: CallStore

    init {
        activeSession.ownBareJid = "alice@waddle.test"
        activeSession.ownFullJid = "alice@waddle.test/waddle-android-1"
        activeSession.onReady(client)
        store = CallStore(
            signaling = ClientCallSignaling(activeSession),
            ownBareJid = { activeSession.ownBareJid },
            ownFullJid = { activeSession.ownFullJid },
            mucSessionCache = sessionCache,
            newSid = sid,
        )
    }
}

internal fun propose(from: String, sid: String, media: WaddleCallMedia = audio) =
    WaddleCallEvent(from = from, to = null, sid = sid, kind = WaddleCallEventKind.Propose(media))

internal fun proceed(from: String, sid: String) =
    WaddleCallEvent(from = from, to = null, sid = sid, kind = WaddleCallEventKind.Proceed)

internal fun ringing(from: String, sid: String) =
    WaddleCallEvent(from = from, to = null, sid = sid, kind = WaddleCallEventKind.Ringing)

internal fun reject(from: String, sid: String, tieBreak: Boolean = false) = WaddleCallEvent(
    from = from,
    to = null,
    sid = sid,
    kind = WaddleCallEventKind.Reject(
        reason = if (tieBreak) WaddleJingleReason.EXPIRED else null,
        tieBreak = tieBreak,
    ),
)

internal fun retract(from: String, sid: String) = WaddleCallEvent(
    from = from,
    to = null,
    sid = sid,
    kind = WaddleCallEventKind.Retract(reason = null, tieBreak = false),
)

internal fun sessionInitiate(from: String, sid: String, media: WaddleCallMedia = audio) =
    WaddleCallEvent(
        from = from,
        to = null,
        sid = sid,
        kind = WaddleCallEventKind.SessionInitiate(join = join, media = media),
    )

internal fun sessionAccept(from: String, sid: String, media: WaddleCallMedia = audio) =
    WaddleCallEvent(
        from = from,
        to = null,
        sid = sid,
        kind = WaddleCallEventKind.SessionAccept(join = join, media = media),
    )

internal fun sessionTerminate(from: String, sid: String, reason: WaddleJingleReason?) =
    WaddleCallEvent(
        from = from,
        to = null,
        sid = sid,
        kind = WaddleCallEventKind.SessionTerminate(reason),
    )

internal fun finish(from: String, sid: String, reason: WaddleJingleReason? = null) =
    WaddleCallEvent(
        from = from,
        to = null,
        sid = sid,
        kind = WaddleCallEventKind.Finish(reason = reason, migratedTo = null),
    )

/** The SFU mixer's separate-IQ Muji session-accept (XEP-0166 §6.3). */
internal fun mucSessionAccept(
    sid: String,
    from: String = "$MIXER_JID/mixer",
    accepted: WaddleLiveKitJoin = mucJoin,
    media: WaddleCallMedia = audio,
) = WaddleCallEvent(
    from = from,
    to = null,
    sid = sid,
    kind = WaddleCallEventKind.SessionAccept(join = accepted, media = media),
)

/** An occupant's XEP-0272 `<muji/>`-bearing MUC presence. */
internal fun mujiPresence(
    nick: String,
    preparing: Boolean = false,
    active: Boolean = false,
    hasAudio: Boolean = true,
    hasVideo: Boolean = false,
    mucJid: String? = null,
    presenceType: String = "available",
    handRaised: Boolean = false,
    muted: Boolean = false,
    room: String = ROOM_JID,
): WaddlePresence = testPresence(
    from = "$room/$nick",
    presenceType = presenceType,
    mucJid = mucJid,
    muji = if (preparing || active) {
        WaddleMujiPresence(preparing = preparing, active = active, audio = hasAudio, video = hasVideo)
    } else {
        null
    },
    handRaised = handRaised,
    muted = muted,
)
