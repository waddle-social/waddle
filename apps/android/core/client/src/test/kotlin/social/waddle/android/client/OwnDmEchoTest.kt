package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Test
import social.waddle.android.client.prefs.EncryptedFileRef
import social.waddle.android.client.prefs.SharedFileRef

class OwnDmEchoTest {
    @Test
    fun `the own-dm echo carries the encrypted attachment envelope verbatim`() {
        val (body, options) = preparedSend(
            stanzaId = "sid-1",
            body = "",
            extras = MessageSendExtras(
                sharedFiles = listOf(
                    SharedFileRef(
                        url = "https://files.waddle.test/photo.jpg.enc",
                        name = "photo.jpg",
                        mediaType = "image/jpeg",
                        sizeBytes = 2048L,
                        disposition = FileDisposition.INLINE,
                        hashes = listOf(StickerHash(algo = "sha-256", valueB64 = "cGxhaW4=")),
                        encrypted = EncryptedFileRef(
                            cipher = EncryptedAttachmentCrypto.CIPHER_AES_256_GCM,
                            keyB64 = "a2V5",
                            ivB64 = "aXY=",
                            hashes = listOf(StickerHash(algo = "sha-256", valueB64 = "Y2lwaGVy")),
                            sources = listOf("https://files.waddle.test/photo.jpg.enc"),
                        ),
                    ),
                ),
            ),
        )

        val echo = ownDmEcho(
            ownJid = "alice@waddle.test",
            peerJid = "bob@waddle.test/phone",
            stanzaId = "sid-1",
            body = body,
            options = options,
        )

        // The echo must render like the eventual MAM copy: same files,
        // envelope included — the sender's own timeline decrypts too.
        assertEquals(options.sharedFiles, echo.sharedFiles)
        assertNotNull(echo.sharedFiles.single().encrypted)
    }
}
