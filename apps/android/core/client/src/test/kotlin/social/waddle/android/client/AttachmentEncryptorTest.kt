package social.waddle.android.client

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import social.waddle.client.ffi.WaddleEncryptedFile
import social.waddle.client.ffi.WaddleEncryptedFileHash
import java.io.ByteArrayInputStream
import java.security.MessageDigest
import java.util.Base64

class AttachmentEncryptorTest {
    @get:Rule
    val tempFolder = TemporaryFolder()

    private val encryptor = AttachmentEncryptor()
    private val plaintext = ByteArray(70_000) { (it % 251).toByte() }

    private fun sha256B64(bytes: ByteArray): String =
        Base64.getEncoder().encodeToString(MessageDigest.getInstance("SHA-256").digest(bytes))

    @Test
    fun `stages ciphertext that decrypts back to the original bytes`() {
        val staged = checkNotNull(
            encryptor.encryptToFile(tempFolder.root) { ByteArrayInputStream(plaintext) },
        )

        assertEquals(plaintext.size.toLong(), staged.plaintextLength)
        assertEquals(
            plaintext.size.toLong() + EncryptedAttachmentCrypto.GCM_TAG_BYTES,
            staged.ciphertextLength,
        )
        val ciphertext = staged.cipherFile.readBytes()
        assertEquals(staged.ciphertextLength, ciphertext.size.toLong())

        // The ciphertext hash inside the envelope digests the staged file;
        // the plaintext hash digests the original bytes — never equal.
        assertEquals(listOf(sha256B64(ciphertext)), staged.encrypted.hashes.map { it.valueB64 })
        assertEquals(listOf(sha256B64(plaintext)), staged.plaintextHashes.map { it.valueB64 })
        assertEquals(EncryptedAttachmentCrypto.CIPHER_AES_256_GCM, staged.encrypted.cipher)
        assertTrue(staged.encrypted.sources.isEmpty())

        // Round-trip through the A1 decrypt helper.
        val decrypted = EncryptedAttachmentCrypto.decrypt(
            ciphertext = ciphertext,
            encrypted = WaddleEncryptedFile(
                cipher = staged.encrypted.cipher,
                keyB64 = staged.encrypted.keyB64,
                ivB64 = staged.encrypted.ivB64,
                hashes = staged.encrypted.hashes.map {
                    WaddleEncryptedFileHash(algo = it.algo, valueB64 = it.valueB64)
                },
                sources = listOf("https://files.waddle.test/blob.enc"),
            ),
            declaredSize = staged.plaintextLength,
        )
        assertArrayEquals(plaintext, decrypted)
    }

    @Test
    fun `fresh key and iv per staging`() {
        val first = checkNotNull(encryptor.encryptToFile(tempFolder.root) { ByteArrayInputStream(plaintext) })
        val second = checkNotNull(encryptor.encryptToFile(tempFolder.root) { ByteArrayInputStream(plaintext) })

        assertTrue(first.encrypted.keyB64 != second.encrypted.keyB64)
        assertTrue(first.encrypted.ivB64 != second.encrypted.ivB64)
    }

    @Test
    fun `an unopenable stream stages nothing and leaves no temp file`() {
        assertNull(encryptor.encryptToFile(tempFolder.root) { null })
        assertEquals(0, tempFolder.root.listFiles().orEmpty().size)
    }
}
