package social.waddle.android.client

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import okhttp3.OkHttpClient
import okhttp3.Request
import social.waddle.client.ffi.WaddleEncryptedFile

/**
 * The web's composite decryption cache key:
 * url|cipher|key|iv|hashes…|sources…. Distinct envelopes for the same
 * URL never share a cache entry or an in-flight download.
 */
fun encryptedAttachmentCacheKey(url: String, encrypted: WaddleEncryptedFile): String = buildList {
    add(url)
    add(encrypted.cipher)
    add(encrypted.keyB64)
    add(encrypted.ivB64)
    encrypted.hashes.forEach { add("${it.algo}:${it.valueB64}") }
    addAll(encrypted.sources)
}.joinToString("|")

/**
 * XEP-0448 decrypt-on-display fetch: try each ciphertext source in
 * order (web `decryptEncryptedAttachment` source-fallback parity),
 * verify the advertised sha-256, and decrypt. Concurrent requests for
 * the same composite key share one in-flight download (web parity),
 * and the plaintext only ever lives in memory — nothing here touches a
 * disk cache.
 */
class EncryptedAttachmentDownloader(private val httpClient: OkHttpClient) {
    /** Owns shared in-flight downloads so one caller's cancellation
     *  cannot abort another caller awaiting the same key. */
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val inFlightLock = Mutex()
    private val inFlight = mutableMapOf<String, Deferred<ByteArray?>>()

    /**
     * Returns the decrypted plaintext, or `null` when every source
     * fails to download, verify, or decrypt.
     */
    suspend fun fetchAndDecrypt(
        url: String,
        encrypted: WaddleEncryptedFile,
        declaredSize: Long?,
    ): ByteArray? {
        val key = encryptedAttachmentCacheKey(url, encrypted)
        val task = inFlightLock.withLock {
            inFlight.getOrPut(key) {
                // The task removes ITSELF from the map (web promise-map
                // parity): removal must not depend on any awaiter's
                // finally running, because Coil cancels awaiters on
                // scroll while the shared download keeps going — a
                // completed entry left behind would pin the decrypted
                // plaintext in memory indefinitely.
                scope.async {
                    try {
                        fetchAndDecryptNow(url, encrypted, declaredSize)
                    } finally {
                        inFlightLock.withLock { inFlight.remove(key) }
                    }
                }
            }
        }
        return task.await()
    }

    /** Test seam: live in-flight entries (0 once downloads settle). */
    internal suspend fun inFlightCountForTest(): Int = inFlightLock.withLock { inFlight.size }

    private fun fetchAndDecryptNow(
        url: String,
        encrypted: WaddleEncryptedFile,
        declaredSize: Long?,
    ): ByteArray? {
        for (source in sourceUrls(url, encrypted)) {
            val plaintext = runCatching {
                fetch(source)?.let { EncryptedAttachmentCrypto.decrypt(it, encrypted, declaredSize) }
            }.getOrNull()
            if (plaintext != null) return plaintext
        }
        return null
    }

    private fun fetch(url: String): ByteArray? =
        httpClient.newCall(Request.Builder().url(url).build()).execute().use { response ->
            if (response.isSuccessful) response.body.bytes() else null
        }

    /** Web `sourceUrls` parity: envelope sources, else the outer URL. */
    private fun sourceUrls(url: String, encrypted: WaddleEncryptedFile): List<String> {
        val sources = encrypted.sources.filter { it.isNotEmpty() }
        return sources.ifEmpty { listOf(url) }
    }
}
