package social.waddle.android.client

import social.waddle.android.client.prefs.EncryptedFileRef
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.security.DigestInputStream
import java.security.MessageDigest
import java.security.SecureRandom
import java.util.Base64
import javax.crypto.Cipher
import javax.crypto.CipherInputStream
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

/**
 * One encrypted upload staged on disk: the ciphertext temp file (the
 * caller MUST delete it after the upload), the exact lengths for the
 * XEP-0363 slot request and the `<size/>` metadata, the XEP-0448
 * envelope (ciphertext sha-256 set, sources filled in after the slot
 * grants the GET URL), and the XEP-0448-mandated plaintext sha-256 for
 * the `<file/>` metadata.
 */
class StagedEncryptedUpload(
    val cipherFile: File,
    val ciphertextLength: Long,
    val plaintextLength: Long,
    val encrypted: EncryptedFileRef,
    val plaintextHashes: List<StickerHash>,
)

/**
 * XEP-0448 encrypt-on-upload: fresh 32-byte AES-256 key + 12-byte GCM
 * IV per attachment, streamed through [CipherInputStream] into a temp
 * file so a 10 MB attachment never buffers in memory; both the
 * plaintext and ciphertext sha-256 digests are computed on the same
 * pass. Pure JVM — no `android.*` dependencies.
 */
class AttachmentEncryptor(private val random: SecureRandom = SecureRandom()) {
    /**
     * Stage the ciphertext for [open]'s stream in [tempDir]. Returns
     * `null` when the stream cannot be opened or encryption fails.
     */
    fun encryptToFile(tempDir: File, open: () -> InputStream?): StagedEncryptedUpload? {
        val cipherFile = runCatching {
            File.createTempFile("waddle-attachment", ".enc", tempDir)
        }.getOrNull() ?: return null
        return runCatching { stage(cipherFile, open) }
            .onFailure { cipherFile.delete() }
            .getOrNull()
    }

    private fun stage(cipherFile: File, open: () -> InputStream?): StagedEncryptedUpload {
        val key = ByteArray(EncryptedAttachmentCrypto.AES_256_KEY_BYTES).also(random::nextBytes)
        val iv = ByteArray(EncryptedAttachmentCrypto.GCM_IV_BYTES).also(random::nextBytes)
        val cipher = Cipher.getInstance(EncryptedAttachmentCrypto.JCA_TRANSFORM)
        cipher.init(
            Cipher.ENCRYPT_MODE,
            SecretKeySpec(key, EncryptedAttachmentCrypto.JCA_KEY_ALGO),
            GCMParameterSpec(EncryptedAttachmentCrypto.GCM_TAG_BITS, iv),
        )
        val plainDigest = MessageDigest.getInstance("SHA-256")
        val cipherDigest = MessageDigest.getInstance("SHA-256")
        val input = open() ?: throw IOException("attachment stream unavailable")
        var ciphertextLength = 0L
        DigestInputStream(input, plainDigest).use { plainIn ->
            CipherInputStream(plainIn, cipher).use { encrypting ->
                cipherFile.outputStream().use { out ->
                    ciphertextLength = copyDigesting(encrypting, out, cipherDigest)
                }
            }
        }
        val encoder = Base64.getEncoder()
        return StagedEncryptedUpload(
            cipherFile = cipherFile,
            ciphertextLength = ciphertextLength,
            // AES-GCM appends exactly the 16-byte tag; no padding.
            plaintextLength = ciphertextLength - EncryptedAttachmentCrypto.GCM_TAG_BYTES,
            encrypted = EncryptedFileRef(
                cipher = EncryptedAttachmentCrypto.CIPHER_AES_256_GCM,
                keyB64 = encoder.encodeToString(key),
                ivB64 = encoder.encodeToString(iv),
                hashes = listOf(
                    StickerHash(
                        algo = EncryptedAttachmentCrypto.SHA_256_ALGO,
                        valueB64 = encoder.encodeToString(cipherDigest.digest()),
                    ),
                ),
                sources = emptyList(),
            ),
            plaintextHashes = listOf(
                StickerHash(
                    algo = EncryptedAttachmentCrypto.SHA_256_ALGO,
                    valueB64 = encoder.encodeToString(plainDigest.digest()),
                ),
            ),
        )
    }

    /** Stream [input] into [out], digesting every written byte. */
    private fun copyDigesting(
        input: InputStream,
        out: OutputStream,
        digest: MessageDigest,
    ): Long {
        var written = 0L
        val buffer = ByteArray(STREAM_BUFFER_BYTES)
        while (true) {
            val read = input.read(buffer)
            if (read < 0) break
            digest.update(buffer, 0, read)
            out.write(buffer, 0, read)
            written += read
        }
        return written
    }

    private companion object {
        const val STREAM_BUFFER_BYTES = 16 * 1024
    }
}
