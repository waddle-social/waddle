package social.waddle.android.client

import social.waddle.client.ffi.WaddleEncryptedFile
import social.waddle.client.ffi.WaddleEncryptedFileHash
import java.security.MessageDigest
import java.util.Base64
import javax.crypto.Cipher
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

/** Typed reason a XEP-0448 ciphertext was refused or failed to decrypt. */
sealed class EncryptedAttachmentException(message: String) : Exception(message) {
    /** The cipher URN is not one Waddle implements. */
    class UnsupportedCipher(cipher: String) :
        EncryptedAttachmentException("unsupported XEP-0448 cipher: $cipher")

    /** The key/iv metadata is not valid base64 or has the wrong length. */
    class InvalidKeyMaterial :
        EncryptedAttachmentException("encrypted attachment metadata does not contain a valid key")

    /** The ciphertext does not match any advertised sha-256 hash. */
    class IntegrityCheckFailed :
        EncryptedAttachmentException("encrypted attachment integrity check failed")

    /** AES-GCM decryption itself failed (bad tag, corrupt stream). */
    class DecryptionFailed :
        EncryptedAttachmentException("encrypted attachment could not be decrypted")
}

/**
 * XEP-0448 decrypt-on-display helper — the web `decryptCiphertext`
 * mirror: cipher/key-length validation, sha-256 verification against the
 * advertised ciphertext hashes (pass-through when none advertises
 * sha-256, hard fail on mismatch), AES/GCM/NoPadding decryption, and a
 * truncate-to-declared-size guard (moot for GCM, spec-directed for
 * future padded ciphers). Pure JVM — no `android.*` dependencies.
 */
object EncryptedAttachmentCrypto {
    /** `urn:xmpp:ciphers:aes-128-gcm-nopadding:0` (16-byte key). */
    const val CIPHER_AES_128_GCM = "urn:xmpp:ciphers:aes-128-gcm-nopadding:0"

    /** `urn:xmpp:ciphers:aes-256-gcm-nopadding:0` (32-byte key). */
    const val CIPHER_AES_256_GCM = "urn:xmpp:ciphers:aes-256-gcm-nopadding:0"

    const val AES_128_KEY_BYTES = 16
    const val AES_256_KEY_BYTES = 32
    const val GCM_IV_BYTES = 12
    const val GCM_TAG_BYTES = 16

    const val SHA_256_ALGO = "sha-256"
    internal const val JCA_TRANSFORM = "AES/GCM/NoPadding"
    internal const val JCA_KEY_ALGO = "AES"
    internal const val GCM_TAG_BITS = GCM_TAG_BYTES * 8

    /**
     * Decrypt one downloaded ciphertext with its XEP-0448 envelope.
     * [declaredSize] is the plaintext `<size/>` from the XEP-0446 file
     * metadata; a longer decryption result is truncated to it.
     */
    @Throws(EncryptedAttachmentException::class)
    fun decrypt(
        ciphertext: ByteArray,
        encrypted: WaddleEncryptedFile,
        declaredSize: Long? = null,
    ): ByteArray {
        val (key, iv) = decodeKeyMaterial(encrypted)
        validateCipherKeyLength(encrypted.cipher, key)
        verifyCiphertextHashes(ciphertext, encrypted.hashes)
        val plaintext = runCatching {
            val cipher = Cipher.getInstance(JCA_TRANSFORM)
            cipher.init(
                Cipher.DECRYPT_MODE,
                SecretKeySpec(key, JCA_KEY_ALGO),
                GCMParameterSpec(GCM_TAG_BITS, iv),
            )
            cipher.doFinal(ciphertext)
        }.getOrElse { throw EncryptedAttachmentException.DecryptionFailed() }
        return truncateToDeclaredSize(plaintext, declaredSize)
    }

    private fun decodeKeyMaterial(encrypted: WaddleEncryptedFile): Pair<ByteArray, ByteArray> {
        val key = decodeBase64(encrypted.keyB64)
        val iv = decodeBase64(encrypted.ivB64)
        if (key == null || iv == null) throw EncryptedAttachmentException.InvalidKeyMaterial()
        return key to iv
    }

    /**
     * Web `validateCipherKeyLength` parity — each GCM variant requires
     * its exact key length — plus refusal of any cipher URN outside the
     * closed set (the web reaches this point only with the closed TS
     * union; the equivalent guard here is explicit).
     */
    private fun validateCipherKeyLength(cipher: String, key: ByteArray) {
        val expectedKeyBytes = when (cipher) {
            CIPHER_AES_128_GCM -> AES_128_KEY_BYTES
            CIPHER_AES_256_GCM -> AES_256_KEY_BYTES
            else -> throw EncryptedAttachmentException.UnsupportedCipher(cipher)
        }
        if (key.size != expectedKeyBytes) throw EncryptedAttachmentException.InvalidKeyMaterial()
    }

    /**
     * Web parity: verify only against advertised sha-256 entries; no
     * advertised sha-256 means nothing to verify. A mismatch on every
     * advertised value is a hard failure.
     */
    private fun verifyCiphertextHashes(ciphertext: ByteArray, hashes: List<WaddleEncryptedFileHash>) {
        val advertised = hashes.filter { it.algo.lowercase() == SHA_256_ALGO }
        if (advertised.isEmpty()) return
        val digest = MessageDigest.getInstance("SHA-256").digest(ciphertext)
        val matched = advertised.any { hash ->
            decodeBase64(hash.valueB64)?.let { MessageDigest.isEqual(digest, it) } == true
        }
        if (!matched) throw EncryptedAttachmentException.IntegrityCheckFailed()
    }

    /**
     * XEP-0448: plaintext longer than the declared `<size/>` is
     * truncated — padding-capable ciphers may append filler bytes.
     */
    private fun truncateToDeclaredSize(plaintext: ByteArray, declaredSize: Long?): ByteArray {
        if (declaredSize == null || declaredSize < 0 || plaintext.size <= declaredSize) return plaintext
        return plaintext.copyOf(declaredSize.toInt())
    }

    private fun decodeBase64(value: String): ByteArray? =
        runCatching { Base64.getDecoder().decode(value) }.getOrNull()
}
