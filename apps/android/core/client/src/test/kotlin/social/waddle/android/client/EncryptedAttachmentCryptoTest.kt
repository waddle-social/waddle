package social.waddle.android.client

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import social.waddle.client.ffi.WaddleEncryptedFile
import social.waddle.client.ffi.WaddleEncryptedFileHash
import java.security.MessageDigest
import java.util.Base64
import javax.crypto.Cipher
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

class EncryptedAttachmentCryptoTest {
    private val plaintext = "hello encrypted penguin".toByteArray()
    private val key256 = ByteArray(32) { it.toByte() }
    private val key128 = ByteArray(16) { (it + 1).toByte() }
    private val iv = ByteArray(12) { (it * 7).toByte() }

    private fun encrypt(key: ByteArray): ByteArray {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, SecretKeySpec(key, "AES"), GCMParameterSpec(128, iv))
        return cipher.doFinal(plaintext)
    }

    private fun b64(bytes: ByteArray): String = Base64.getEncoder().encodeToString(bytes)

    private fun sha256B64(bytes: ByteArray): String =
        b64(MessageDigest.getInstance("SHA-256").digest(bytes))

    private fun envelope(
        key: ByteArray = key256,
        cipher: String = EncryptedAttachmentCrypto.CIPHER_AES_256_GCM,
        hashes: List<WaddleEncryptedFileHash> = emptyList(),
    ) = WaddleEncryptedFile(
        cipher = cipher,
        keyB64 = b64(key),
        ivB64 = b64(iv),
        hashes = hashes,
        sources = listOf("https://files.waddle.test/blob.enc"),
    )

    @Test
    fun `round trips aes-256 ciphertext with a matching advertised hash`() {
        val ciphertext = encrypt(key256)
        val envelope = envelope(
            hashes = listOf(WaddleEncryptedFileHash(algo = "sha-256", valueB64 = sha256B64(ciphertext))),
        )

        val decrypted = EncryptedAttachmentCrypto.decrypt(ciphertext, envelope, plaintext.size.toLong())

        assertArrayEquals(plaintext, decrypted)
    }

    @Test
    fun `accepts a 128-bit key under the aes-128 cipher urn`() {
        val ciphertext = encrypt(key128)
        val envelope = envelope(key = key128, cipher = EncryptedAttachmentCrypto.CIPHER_AES_128_GCM)

        assertArrayEquals(plaintext, EncryptedAttachmentCrypto.decrypt(ciphertext, envelope, null))
    }

    @Test
    fun `passes when no sha-256 hash is advertised`() {
        val ciphertext = encrypt(key256)

        assertArrayEquals(plaintext, EncryptedAttachmentCrypto.decrypt(ciphertext, envelope(), null))
    }

    @Test
    fun `hard fails when the ciphertext does not match the advertised hash`() {
        val ciphertext = encrypt(key256)
        val envelope = envelope(
            hashes = listOf(WaddleEncryptedFileHash(algo = "sha-256", valueB64 = sha256B64(plaintext))),
        )

        assertThrows(EncryptedAttachmentException.IntegrityCheckFailed::class.java) {
            EncryptedAttachmentCrypto.decrypt(ciphertext, envelope, null)
        }
    }

    @Test
    fun `refuses ciphers outside the closed set`() {
        val ciphertext = encrypt(key256)
        val envelope = envelope(cipher = "urn:xmpp:ciphers:rot13:0")

        assertThrows(EncryptedAttachmentException.UnsupportedCipher::class.java) {
            EncryptedAttachmentCrypto.decrypt(ciphertext, envelope, null)
        }
    }

    @Test
    fun `enforces the exact key length per cipher urn`() {
        val ciphertext = encrypt(key128)
        // A 128-bit key advertised under the aes-256 URN must be refused
        // (web validateCipherKeyLength parity), and vice versa.
        val misdeclared256 = envelope(key = key128, cipher = EncryptedAttachmentCrypto.CIPHER_AES_256_GCM)
        val misdeclared128 = envelope(key = key256, cipher = EncryptedAttachmentCrypto.CIPHER_AES_128_GCM)

        assertThrows(EncryptedAttachmentException.InvalidKeyMaterial::class.java) {
            EncryptedAttachmentCrypto.decrypt(ciphertext, misdeclared256, null)
        }
        assertThrows(EncryptedAttachmentException.InvalidKeyMaterial::class.java) {
            EncryptedAttachmentCrypto.decrypt(ciphertext, misdeclared128, null)
        }
    }

    @Test
    fun `truncates plaintext longer than the declared size`() {
        val ciphertext = encrypt(key256)
        val declared = 5L

        val decrypted = EncryptedAttachmentCrypto.decrypt(ciphertext, envelope(), declared)

        assertArrayEquals(plaintext.copyOf(declared.toInt()), decrypted)
    }

    @Test
    fun `a corrupted ciphertext fails decryption`() {
        val ciphertext = encrypt(key256).apply { this[0] = (this[0].toInt() xor 0xFF).toByte() }

        assertThrows(EncryptedAttachmentException.DecryptionFailed::class.java) {
            EncryptedAttachmentCrypto.decrypt(ciphertext, envelope(), null)
        }
    }
}
