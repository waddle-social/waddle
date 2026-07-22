package social.waddle.android.media

import coil3.ImageLoader
import coil3.decode.DataSource
import coil3.decode.ImageSource
import coil3.fetch.FetchResult
import coil3.fetch.Fetcher
import coil3.fetch.SourceFetchResult
import coil3.key.Keyer
import coil3.request.Options
import okio.Buffer
import social.waddle.android.client.EncryptedAttachmentDownloader
import java.io.IOException

/**
 * Memory-cache key for encrypted images: the web's composite
 * url|cipher|key|iv|hashes|sources key, so a re-keyed re-share of the
 * same URL is never served a stale plaintext.
 */
class EncryptedAttachmentKeyer : Keyer<EncryptedFileModel> {
    override fun key(data: EncryptedFileModel, options: Options): String = data.cacheKey
}

/**
 * Coil fetcher for [EncryptedFileModel]: download the ciphertext
 * (source-fallback loop), verify the advertised sha-256 (hard fail on
 * mismatch), decrypt, and hand Coil the plaintext from memory. The
 * plaintext NEVER reaches the disk cache — this fetcher replaces the
 * network fetcher that would write it there (web parity: decrypted
 * attachment blobs are memory-only).
 */
class EncryptedAttachmentFetcher(
    private val model: EncryptedFileModel,
    private val options: Options,
    private val downloader: EncryptedAttachmentDownloader,
) : Fetcher {
    override suspend fun fetch(): FetchResult {
        val plaintext = downloader.fetchAndDecrypt(model.url, model.encrypted, model.sizeBytes)
            ?: throw IOException("unable to fetch or decrypt encrypted attachment")
        return SourceFetchResult(
            source = ImageSource(Buffer().write(plaintext), options.fileSystem),
            mimeType = model.mediaType,
            dataSource = DataSource.NETWORK,
        )
    }

    class Factory(private val downloader: EncryptedAttachmentDownloader) : Fetcher.Factory<EncryptedFileModel> {
        override fun create(data: EncryptedFileModel, options: Options, imageLoader: ImageLoader): Fetcher =
            EncryptedAttachmentFetcher(data, options, downloader)
    }
}
