package social.waddle.android.push

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import com.google.crypto.tink.apps.fixed_webpush.WebPushHybridDecrypt
import social.waddle.android.client.prefs.PushEndpoint
import java.security.GeneralSecurityException
import java.security.KeyFactory
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.SecureRandom
import java.security.interfaces.ECPrivateKey
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import java.security.spec.PKCS8EncodedKeySpec
import java.util.Base64
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Seals small secrets at rest under a key that never leaves the platform
 * keystore. The output is opaque base64url.
 */
interface KeyWrapper {
    fun wrap(plain: ByteArray): String

    /** `null` when [wrapped] cannot be opened (another install, a lost key). */
    fun unwrap(wrapped: String): ByteArray?
}

/** [KeyWrapper] over an AES-256-GCM key in the Android Keystore. */
class AndroidKeystoreWrapper(private val alias: String = DEFAULT_ALIAS) : KeyWrapper {
    override fun wrap(plain: ByteArray): String {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key())
        return base64Url.encodeToString(cipher.iv + cipher.doFinal(plain))
    }

    override fun unwrap(wrapped: String): ByteArray? = try {
        val sealed = base64UrlDecoder.decode(wrapped)
        if (sealed.size <= IV_BYTES) {
            null
        } else {
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(TAG_BITS, sealed, 0, IV_BYTES))
            cipher.doFinal(sealed, IV_BYTES, sealed.size - IV_BYTES)
        }
    } catch (_: GeneralSecurityException) {
        null
    } catch (_: IllegalArgumentException) {
        null
    }

    private fun key(): SecretKey {
        val store = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        (store.getEntry(alias, null) as? KeyStore.SecretKeyEntry)?.let { return it.secretKey }
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
        generator.init(
            KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(AES_KEY_BITS)
                .build(),
        )
        return generator.generateKey()
    }

    private companion object {
        const val DEFAULT_ALIAS = "waddle-push-envelope-keys"
        const val ANDROID_KEYSTORE = "AndroidKeyStore"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val IV_BYTES = 12
        const val TAG_BITS = 128
        const val AES_KEY_BITS = 256
    }
}

/**
 * The RFC 8291 key pair this install generates for FCM delivery, and the
 * decryption of what the Push Service encrypts to it. The Push Service
 * sends FCM the same `aes128gcm` ciphertext of the v1 envelope it sends
 * Web Push, so Google only ever carries ciphertext.
 */
object FcmEnvelopeKeys {
    /** A fresh key set: what is registered, and the sealed private key. */
    data class Generated(val p256dh: String, val auth: String, val wrappedPrivateKey: String) {
        override fun toString(): String = "Generated(p256dh=$p256dh)"
    }

    fun generate(wrapper: KeyWrapper, random: SecureRandom = SecureRandom()): Generated {
        val generator = KeyPairGenerator.getInstance("EC")
        generator.initialize(ECGenParameterSpec("secp256r1"), random)
        val pair = generator.generateKeyPair()
        val auth = ByteArray(AUTH_BYTES).also(random::nextBytes)
        return Generated(
            p256dh = base64Url.encodeToString(uncompressedPoint(pair.public as ECPublicKey)),
            auth = base64Url.encodeToString(auth),
            wrappedPrivateKey = wrapper.wrap(pair.private.encoded),
        )
    }

    /**
     * Decrypt the FCM `payload` data value (base64url `aes128gcm`
     * ciphertext) with [endpoint]'s keys; `null` for anything that does
     * not decrypt, which is then dropped unseen.
     */
    fun decrypt(endpoint: PushEndpoint.Fcm, payload: String, wrapper: KeyWrapper): ByteArray? = try {
        val privateKey = wrapper.unwrap(endpoint.wrappedPrivateKey)?.let { encoded ->
            KeyFactory.getInstance("EC").generatePrivate(PKCS8EncodedKeySpec(encoded)) as? ECPrivateKey
        }
        privateKey?.let {
            WebPushHybridDecrypt.Builder()
                .withAuthSecret(base64UrlDecoder.decode(endpoint.auth))
                .withRecipientPublicKey(base64UrlDecoder.decode(endpoint.p256dh))
                .withRecipientPrivateKey(it)
                .build()
                .decrypt(base64UrlDecoder.decode(payload), null)
        }
    } catch (_: GeneralSecurityException) {
        null
    } catch (_: IllegalArgumentException) {
        null
    }

    /** SEC1 uncompressed point: `0x04 || X || Y`, 65 bytes for P-256. */
    private fun uncompressedPoint(key: ECPublicKey): ByteArray {
        fun coordinate(value: java.math.BigInteger): ByteArray {
            val bytes = value.toByteArray().dropWhile { it == 0.toByte() }.toByteArray()
            return ByteArray(COORDINATE_BYTES - bytes.size) + bytes
        }
        return byteArrayOf(UNCOMPRESSED_TAG) + coordinate(key.w.affineX) + coordinate(key.w.affineY)
    }

    private const val AUTH_BYTES = 16
    private const val COORDINATE_BYTES = 32
    private const val UNCOMPRESSED_TAG: Byte = 0x04
}

private val base64Url: Base64.Encoder = Base64.getUrlEncoder().withoutPadding()

/** Accepts padded and unpadded base64url. */
private val base64UrlDecoder: Base64.Decoder = Base64.getUrlDecoder()
