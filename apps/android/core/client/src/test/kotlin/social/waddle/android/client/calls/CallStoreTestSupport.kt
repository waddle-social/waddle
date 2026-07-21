package social.waddle.android.client.calls

import social.waddle.android.client.FakeWaddleClient
import social.waddle.android.client.session.ActiveSession
import social.waddle.client.ffi.WaddleCallEvent
import social.waddle.client.ffi.WaddleCallEventKind
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleJingleReason
import social.waddle.client.ffi.WaddleLiveKitJoin

/**
 * Shared fixture for the CallStore suites ([CallStoreTest],
 * [CallStoreTieBreakTest]): a real [ClientCallSignaling] wire path
 * into the recording [FakeWaddleClient], plus typed event builders.
 */
internal const val OWN_BARE = "alice@waddle.test"
internal const val OWN_FULL = "alice@waddle.test/waddle-android-1"
internal const val PEER_BARE = "bob@waddle.test"
internal const val PEER_FULL = "bob@waddle.test/phone"

internal val audio = WaddleCallMedia(audio = true, video = false)
internal val video = WaddleCallMedia(audio = true, video = true)
internal val join = WaddleLiveKitJoin(
    url = "wss://livekit.waddle.test",
    room = "dm-room",
    identity = OWN_FULL,
    token = "jwt",
)

internal class Fixture(sid: () -> String = { "c-fixed" }) {
    val client = FakeWaddleClient()
    val activeSession = ActiveSession { }
    val store: CallStore

    init {
        activeSession.ownBareJid = "alice@waddle.test"
        activeSession.ownFullJid = "alice@waddle.test/waddle-android-1"
        activeSession.onReady(client)
        store = CallStore(
            signaling = ClientCallSignaling(activeSession),
            ownBareJid = { activeSession.ownBareJid },
            ownFullJid = { activeSession.ownFullJid },
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
