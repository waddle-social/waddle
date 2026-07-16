package social.waddle.android.feature.call

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleLiveKitJoin
import java.util.concurrent.CopyOnWriteArrayList

/**
 * Hermetic [CallMediaController]: records connects/disconnects and
 * mirrors toggles into its state flows without ever touching WebRTC,
 * the microphone, or the camera — mandatory for CI emulators.
 */
class FakeCallMediaController : CallMediaController {
    data class ConnectCall(
        val join: WaddleLiveKitJoin,
        val media: WaddleCallMedia,
        val iceServers: List<IceServerConfig>,
    )

    val connectCalls = CopyOnWriteArrayList<ConnectCall>()

    @Volatile
    var disconnectCalls = 0

    private val _connection = MutableStateFlow<CallMediaConnection>(CallMediaConnection.Idle)
    override val connection: StateFlow<CallMediaConnection> = _connection

    private val _micEnabled = MutableStateFlow(false)
    override val micEnabled: StateFlow<Boolean> = _micEnabled

    private val _cameraEnabled = MutableStateFlow(false)
    override val cameraEnabled: StateFlow<Boolean> = _cameraEnabled

    private val _speakerOn = MutableStateFlow(false)
    override val speakerOn: StateFlow<Boolean> = _speakerOn

    private val _remoteVideo = MutableStateFlow<VideoTrackHandle?>(null)
    override val remoteVideo: StateFlow<VideoTrackHandle?> = _remoteVideo

    private val _localVideo = MutableStateFlow<VideoTrackHandle?>(null)
    override val localVideo: StateFlow<VideoTrackHandle?> = _localVideo

    override suspend fun connect(
        join: WaddleLiveKitJoin,
        media: WaddleCallMedia,
        iceServers: List<IceServerConfig>,
    ) {
        connectCalls += ConnectCall(join, media, iceServers)
        _connection.value = CallMediaConnection.Connected
        _micEnabled.value = media.audio
        _cameraEnabled.value = media.video
        _speakerOn.value = media.video
    }

    override suspend fun disconnect() {
        disconnectCalls += 1
        _connection.value = CallMediaConnection.Idle
        _micEnabled.value = false
        _cameraEnabled.value = false
        _speakerOn.value = false
        _remoteVideo.value = null
        _localVideo.value = null
    }

    override suspend fun setMicEnabled(enabled: Boolean) {
        _micEnabled.value = enabled
    }

    override suspend fun setCameraEnabled(enabled: Boolean) {
        _cameraEnabled.value = enabled
    }

    override suspend fun flipCamera() = Unit

    override fun setSpeakerphone(on: Boolean) {
        _speakerOn.value = on
    }
}
