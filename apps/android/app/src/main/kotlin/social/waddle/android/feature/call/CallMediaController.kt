package social.waddle.android.feature.call

import kotlinx.coroutines.flow.StateFlow
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleLiveKitJoin

/**
 * The media plane behind one call: connect to the SFU room named by
 * the server-issued join credentials, publish mic/camera, and expose
 * the toggles the in-call UI drives. Constructor-injected behind this
 * interface so JVM tests and the hermetic emulator suite drive the
 * call state machine with a fake — CI must never touch a real
 * microphone or camera.
 */
interface CallMediaController {
    /** Media transport phase; the UI shows reconnect/failure states off this. */
    val connection: StateFlow<CallMediaConnection>

    /** Whether the local mic track is ACTUALLY publishing right now. */
    val micEnabled: StateFlow<Boolean>

    /** Whether the local camera track is ACTUALLY publishing right now. */
    val cameraEnabled: StateFlow<Boolean>

    /** Loudspeaker (vs earpiece) routing state. */
    val speakerOn: StateFlow<Boolean>

    /** The remote peer's camera track, when one is subscribed. */
    val remoteVideo: StateFlow<VideoTrackHandle?>

    /** Our own camera track for the local PiP preview. */
    val localVideo: StateFlow<VideoTrackHandle?>

    /**
     * The SFU room's REMOTE participant identities (LiveKit identities
     * are full JIDs), seeded when the connection turns Connected and
     * updated on participant connect/disconnect events; empty while no
     * room is connected. Source of the group-call live-roster
     * projection (web muc-call-live-participants.ts).
     */
    val remoteParticipantIdentities: StateFlow<List<String>>

    /**
     * Validate the join grant and connect. [media] names the tracks to
     * publish (audio and/or video); [iceServers] is the XEP-0215
     * advertisement mapped by [iceServerConfigsFrom] — empty means "no
     * advertisement", keeping the SDK's signalling-provided servers.
     * Throws [CallMediaException] on a validation or connect failure.
     */
    suspend fun connect(join: WaddleLiveKitJoin, media: WaddleCallMedia, iceServers: List<IceServerConfig>)

    suspend fun disconnect()

    suspend fun setMicEnabled(enabled: Boolean)

    suspend fun setCameraEnabled(enabled: Boolean)

    suspend fun flipCamera()

    fun setSpeakerphone(on: Boolean)
}

/** Media transport phase, SDK-free for the UI layer. */
sealed interface CallMediaConnection {
    data object Idle : CallMediaConnection

    data object Connecting : CallMediaConnection

    data object Connected : CallMediaConnection

    data object Reconnecting : CallMediaConnection

    data class Failed(val message: String) : CallMediaConnection
}

/**
 * Opaque handle to a live video track. The LiveKit implementation
 * carries the SDK track; fakes carry a marker the preview composable
 * renders as a placeholder surface.
 */
interface VideoTrackHandle

/** Typed failure from [CallMediaController.connect]. */
class CallMediaException(message: String, cause: Throwable? = null) : Exception(message, cause)
