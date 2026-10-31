package social.waddle.android.flutter

import android.content.Context
import android.view.View
import android.widget.FrameLayout
import io.flutter.plugin.common.StandardMessageCodec
import io.flutter.plugin.platform.PlatformView
import io.flutter.plugin.platform.PlatformViewFactory
import io.livekit.android.renderer.TextureViewRenderer
import io.livekit.android.room.track.VideoTrack
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.feature.call.LiveKitVideoTrackHandle

/** A texture-backed LiveKit video tile, driven by the existing app-scoped media session. */
class FlutterCallVideoFactory(private val graph: AppGraph, private val scope: CoroutineScope) :
    PlatformViewFactory(StandardMessageCodec.INSTANCE) {
    override fun create(context: Context, viewId: Int, args: Any?): PlatformView {
        val local = (args as? Map<*, *>)?.get("local") == true
        return FlutterCallVideo(context, graph, scope, local)
    }
}

private class FlutterCallVideo(
    context: Context,
    graph: AppGraph,
    parent: CoroutineScope,
    local: Boolean,
) : PlatformView {
    private val container = FrameLayout(context)
    private val scope =
        CoroutineScope(
            parent.coroutineContext +
                SupervisorJob(parent.coroutineContext[Job]) +
                Dispatchers.Main.immediate
        )
    private var renderer: TextureViewRenderer? = null
    private var track: VideoTrack? = null

    init {
        container.setBackgroundColor(android.graphics.Color.BLACK)
        val flow =
            if (local) {
                graph.callSessionController.media.localVideo
            } else {
                graph.callSessionController.media.remoteVideo
            }
        scope.launch {
            flow.collect { handle ->
                val live = handle as? LiveKitVideoTrackHandle
                if (live?.track === track) return@collect
                releaseRenderer()
                if (live != null) {
                    val next = TextureViewRenderer(context)
                    live.room.initVideoRenderer(next)
                    next.setMirror(local)
                    container.addView(
                        next,
                        FrameLayout.LayoutParams(
                            FrameLayout.LayoutParams.MATCH_PARENT,
                            FrameLayout.LayoutParams.MATCH_PARENT,
                        ),
                    )
                    renderer = next
                    track = live.track
                    runCatching { live.track.addRenderer(next) }
                }
            }
        }
    }

    override fun getView(): View = container

    override fun dispose() {
        scope.cancel()
        releaseRenderer()
    }

    private fun releaseRenderer() {
        renderer?.let { view ->
            runCatching { track?.removeRenderer(view) }
            container.removeView(view)
            view.release()
        }
        renderer = null
        track = null
    }
}
