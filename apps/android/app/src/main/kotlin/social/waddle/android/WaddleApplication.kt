package social.waddle.android

import android.app.Application
import coil3.ImageLoader
import coil3.PlatformContext
import coil3.SingletonImageLoader
import coil3.network.okhttp.OkHttpNetworkFetcherFactory
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
        graph = AppGraph(this)
        NotificationChannels.ensure(this)
        graph.start()
    }

    /** Coil 3 image loader sharing the graph's OkHttp client. */
    override fun newImageLoader(context: PlatformContext): ImageLoader =
        ImageLoader.Builder(context)
            .components {
                add(OkHttpNetworkFetcherFactory(callFactory = { graph.okHttpClient }))
            }
            .build()
}
