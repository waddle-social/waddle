package social.waddle.android.media

import coil3.ImageLoader
import coil3.PlatformContext
import coil3.gif.AnimatedImageDecoder
import coil3.network.okhttp.OkHttpNetworkFetcherFactory
import okhttp3.OkHttpClient
import social.waddle.android.client.EncryptedAttachmentDownloader

/** The shared loader used by timeline media, the GIF picker, and the full-screen viewer. */
internal fun newWaddleImageLoader(
    context: PlatformContext,
    httpClient: OkHttpClient,
    encryptedAttachmentDownloader: EncryptedAttachmentDownloader,
): ImageLoader = ImageLoader.Builder(context)
    .components {
        // minSdk 34: Android's animated decoder supports GIF and animated WebP.
        add(AnimatedImageDecoder.Factory())
        // XEP-0448 plaintext stays in memory and never reaches the disk cache.
        add(EncryptedAttachmentFetcher.Factory(encryptedAttachmentDownloader))
        add(EncryptedAttachmentKeyer())
        add(OkHttpNetworkFetcherFactory(callFactory = { httpClient }))
    }
    .build()
