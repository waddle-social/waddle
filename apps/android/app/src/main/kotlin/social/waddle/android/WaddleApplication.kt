package social.waddle.android

import android.app.Application
import coil3.ImageLoader
import coil3.PlatformContext
import coil3.SingletonImageLoader
import social.waddle.android.flavour.Distribution
import social.waddle.android.media.newWaddleImageLoader
import social.waddle.android.service.NotificationChannels

/**
 * Creates the [AppGraph] composition root, registers the notification
 * channels, and kicks off cold-start session restore (which drives the
 * connection service through the app-state collector).
 */
class WaddleApplication : Application(), SingletonImageLoader.Factory {
    lateinit var graph: AppGraph
        private set

    override fun onCreate() {
        super.onCreate()
        // Before the graph: the Play build's FCM must exist when it is wired.
        Distribution.initialize(this)
        graph = AppGraph(this)
        NotificationChannels.ensure(this)
        graph.start()
    }

    /** Coil 3 image loader sharing the graph's OkHttp client. */
    override fun newImageLoader(context: PlatformContext): ImageLoader =
        newWaddleImageLoader(context, graph.okHttpClient, graph.encryptedAttachmentDownloader)
}
