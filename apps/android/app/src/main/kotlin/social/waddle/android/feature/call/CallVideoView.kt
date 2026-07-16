package social.waddle.android.feature.call

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.viewinterop.AndroidView
import io.livekit.android.renderer.TextureViewRenderer
import io.livekit.android.room.track.VideoTrack

/**
 * Renders a [VideoTrackHandle]: a LiveKit `TextureViewRenderer` for
 * the production handle, a plain surface for fakes so the hermetic
 * emulator suite can lay out the video call UI without WebRTC.
 */
@Composable
fun CallVideoView(handle: VideoTrackHandle, modifier: Modifier = Modifier) {
    when (handle) {
        is LiveKitVideoTrackHandle -> AndroidView(
            modifier = modifier,
            factory = { context ->
                TextureViewRenderer(context).also { renderer ->
                    handle.room.initVideoRenderer(renderer)
                }
            },
            update = { renderer ->
                val previous = renderer.tag as? VideoTrack
                if (previous !== handle.track) {
                    previous?.removeRenderer(renderer)
                    handle.track.addRenderer(renderer)
                    renderer.tag = handle.track
                }
            },
            onRelease = { renderer ->
                (renderer.tag as? VideoTrack)?.removeRenderer(renderer)
                renderer.tag = null
                renderer.release()
            },
        )
        else -> Box(
            modifier = modifier
                .background(MaterialTheme.colorScheme.surfaceVariant)
                .testTag(CallTestTags.FAKE_VIDEO_SURFACE),
        )
    }
}
