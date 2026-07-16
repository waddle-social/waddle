package social.waddle.android.feature.call

import android.content.Context
import com.twilio.audioswitch.AudioDevice
import io.livekit.android.ConnectOptions
import io.livekit.android.LiveKit
import io.livekit.android.events.RoomEvent
import io.livekit.android.events.collect
import io.livekit.android.room.Room
import io.livekit.android.room.track.LocalVideoTrack
import io.livekit.android.room.track.Track
import io.livekit.android.room.track.VideoTrack
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import livekit.org.webrtc.PeerConnection
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleLiveKitJoin

/** [VideoTrackHandle] carrying a live LiveKit track for rendering. */
class LiveKitVideoTrackHandle(
    val room: Room,
    val track: VideoTrack,
) : VideoTrackHandle

/**
 * Production [CallMediaController] over `livekit-android`: one `Room`
 * per call, connected with the server-issued in-band join credentials
 * (grant pre-validated against the join room/identity, web engine.ts
 * parity) and the XEP-0215 TURN/STUN advertisement as `rtcConfig` ICE
 * servers.
 */
class LiveKitCallMediaController(
    private val appContext: Context,
    private val scope: CoroutineScope,
    private val makeRoom: (Context) -> Room = { context -> LiveKit.create(context.applicationContext) },
) : CallMediaController {
    private val _connection = MutableStateFlow<CallMediaConnection>(CallMediaConnection.Idle)
    override val connection: StateFlow<CallMediaConnection> = _connection.asStateFlow()

    private val _micEnabled = MutableStateFlow(false)
    override val micEnabled: StateFlow<Boolean> = _micEnabled.asStateFlow()

    private val _cameraEnabled = MutableStateFlow(false)
    override val cameraEnabled: StateFlow<Boolean> = _cameraEnabled.asStateFlow()

    private val _speakerOn = MutableStateFlow(false)
    override val speakerOn: StateFlow<Boolean> = _speakerOn.asStateFlow()

    private val _remoteVideo = MutableStateFlow<VideoTrackHandle?>(null)
    override val remoteVideo: StateFlow<VideoTrackHandle?> = _remoteVideo.asStateFlow()

    private val _localVideo = MutableStateFlow<VideoTrackHandle?>(null)
    override val localVideo: StateFlow<VideoTrackHandle?> = _localVideo.asStateFlow()

    private var room: Room? = null
    private var eventsJob: Job? = null

    override suspend fun connect(
        join: WaddleLiveKitJoin,
        media: WaddleCallMedia,
        iceServers: List<IceServerConfig>,
    ) {
        if (room != null) throw CallMediaException("call media already connected")
        // Defensive pre-flight: a mismatched/misconfigured token
        // otherwise fails deep inside Room.connect with an opaque
        // rejection (web engine.ts parity).
        val grant = validateLiveKitGrant(join)
        if (grant is LiveKitGrantCheck.Invalid) {
            throw CallMediaException("invalid LiveKit join token: ${grant.defect}")
        }
        _connection.value = CallMediaConnection.Connecting
        val freshRoom = makeRoom(appContext)
        eventsJob = scope.launch {
            freshRoom.events.collect { event -> onRoomEvent(event) }
        }
        try {
            // XEP-0215 ICE injection: an advertised list becomes the
            // authoritative ICE set; an empty list means "no
            // advertisement" — pass none so LiveKit keeps its own
            // signalling-provided servers (web ice-servers.ts parity).
            val options = if (iceServers.isEmpty()) {
                ConnectOptions()
            } else {
                ConnectOptions(iceServers = iceServers.map(::toRtcIceServer))
            }
            freshRoom.connect(join.url, join.token, options)
        } catch (cancellation: CancellationException) {
            teardown(freshRoom)
            throw cancellation
        } catch (failure: Throwable) {
            teardown(freshRoom)
            _connection.value = CallMediaConnection.Failed(failure.message ?: "connect failed")
            throw CallMediaException("LiveKit connect failed", failure)
        }
        room = freshRoom
        _connection.value = CallMediaConnection.Connected
        // Publishing is BEST-EFFORT and decoupled from joining: a
        // missing device or denied permission must not eject the user —
        // they stay a receive-only participant (web parity).
        if (media.audio) {
            runCatching { freshRoom.localParticipant.setMicrophoneEnabled(true) }
        }
        if (media.video) {
            runCatching { freshRoom.localParticipant.setCameraEnabled(true) }
        }
        refreshLocalTrackStates()
        // Video calls route to the loudspeaker by default; audio calls
        // start on the earpiece.
        setSpeakerphone(media.video)
    }

    override suspend fun disconnect() {
        val current = room ?: return
        room = null
        teardown(current)
        _connection.value = CallMediaConnection.Idle
    }

    override suspend fun setMicEnabled(enabled: Boolean) {
        val current = room ?: return
        runCatching { current.localParticipant.setMicrophoneEnabled(enabled) }
        refreshLocalTrackStates()
    }

    override suspend fun setCameraEnabled(enabled: Boolean) {
        val current = room ?: return
        runCatching { current.localParticipant.setCameraEnabled(enabled) }
        refreshLocalTrackStates()
    }

    override suspend fun flipCamera() {
        val current = room ?: return
        val track = current.localParticipant
            .getTrackPublication(Track.Source.CAMERA)?.track as? LocalVideoTrack ?: return
        track.switchCamera()
    }

    override fun setSpeakerphone(on: Boolean) {
        val handler = room?.audioSwitchHandler ?: return
        val devices = handler.availableAudioDevices
        val target = if (on) {
            devices.filterIsInstance<AudioDevice.Speakerphone>().firstOrNull()
        } else {
            devices.filterIsInstance<AudioDevice.Earpiece>().firstOrNull()
                ?: devices.filterIsInstance<AudioDevice.WiredHeadset>().firstOrNull()
        }
        if (target != null) {
            handler.selectDevice(target)
            _speakerOn.value = on
        }
    }

    private fun onRoomEvent(event: RoomEvent) {
        when (event) {
            is RoomEvent.TrackSubscribed -> {
                val track = event.track
                if (track is VideoTrack) _remoteVideo.value = LiveKitVideoTrackHandle(event.room, track)
            }
            is RoomEvent.TrackUnsubscribed -> {
                val handle = _remoteVideo.value as? LiveKitVideoTrackHandle
                if (handle != null && handle.track == event.track) _remoteVideo.value = null
            }
            is RoomEvent.TrackPublished, is RoomEvent.TrackUnpublished,
            is RoomEvent.LocalTrackSubscribed,
            is RoomEvent.TrackMuted, is RoomEvent.TrackUnmuted,
            -> refreshLocalTrackStates()
            is RoomEvent.Reconnecting -> _connection.value = CallMediaConnection.Reconnecting
            is RoomEvent.Reconnected -> _connection.value = CallMediaConnection.Connected
            is RoomEvent.Disconnected -> {
                // Local disconnect() nulls `room` first; a remote/SDK
                // disconnect surfaces as Failed so the session
                // controller can end the call.
                if (room != null) {
                    _connection.value = CallMediaConnection.Failed(event.reason.name)
                }
            }
            else -> Unit
        }
    }

    private fun refreshLocalTrackStates() {
        val current = room ?: run {
            _micEnabled.value = false
            _cameraEnabled.value = false
            _localVideo.value = null
            return
        }
        _micEnabled.value = current.localParticipant.isMicrophoneEnabled
        _cameraEnabled.value = current.localParticipant.isCameraEnabled
        val cameraTrack = current.localParticipant
            .getTrackPublication(Track.Source.CAMERA)?.track as? VideoTrack
        _localVideo.value = cameraTrack?.let { LiveKitVideoTrackHandle(current, it) }
    }

    private fun teardown(target: Room) {
        eventsJob?.cancel()
        eventsJob = null
        _remoteVideo.value = null
        _localVideo.value = null
        _micEnabled.value = false
        _cameraEnabled.value = false
        _speakerOn.value = false
        runCatching { target.disconnect() }
        runCatching { target.release() }
    }

    private fun toRtcIceServer(config: IceServerConfig): PeerConnection.IceServer {
        val builder = PeerConnection.IceServer.builder(config.url)
        config.username?.let(builder::setUsername)
        config.password?.let(builder::setPassword)
        return builder.createIceServer()
    }
}
