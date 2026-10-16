package social.waddle.android.push

import com.google.crypto.tink.apps.fixed_webpush.WebPushHybridEncrypt
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Test
import social.waddle.android.client.prefs.PushEndpoint
import java.math.BigInteger
import java.security.KeyFactory
import java.security.KeyPairGenerator
import java.security.interfaces.ECPrivateKey
import java.security.spec.ECGenParameterSpec
import java.security.spec.ECPrivateKeySpec
import java.util.Base64

/**
 * The FCM envelope keys: what the Push Service encrypts to them decrypts,
 * the RFC 8291 test vector decrypts, and nothing else does.
 */
class FcmEnvelopeKeysTest {
    /** Base64url passthrough; the real wrapper needs the Android Keystore. */
    private class PlainWrapper : KeyWrapper {
        override fun wrap(plain: ByteArray): String = url.encodeToString(plain)

        override fun unwrap(wrapped: String): ByteArray? = runCatching { urlDecoder.decode(wrapped) }.getOrNull()
    }

    private val wrapper = PlainWrapper()

    @Test
    fun `generated keys are a p256 point and a 16 byte secret`() {
        val keys = FcmEnvelopeKeys.generate(wrapper)

        val point = urlDecoder.decode(keys.p256dh)
        assertEquals(65, point.size)
        assertEquals(0x04.toByte(), point[0])
        assertEquals(16, urlDecoder.decode(keys.auth).size)
        assertFalse("the private key never shows in the string form", keys.toString().contains(keys.wrappedPrivateKey))
    }

    @Test
    fun `a payload encrypted to the registered keys decrypts`() {
        val keys = FcmEnvelopeKeys.generate(wrapper)
        val endpoint = endpointOf(keys)
        val envelope = """{"v":1,"class":"dm","conversation":"alice@waddle.test","item":"s-1"}""".encodeToByteArray()
        val ciphertext = WebPushHybridEncrypt.Builder()
            .withAuthSecret(urlDecoder.decode(keys.auth))
            .withRecipientPublicKey(urlDecoder.decode(keys.p256dh))
            .build()
            .encrypt(envelope, null)

        val plaintext = FcmEnvelopeKeys.decrypt(endpoint, url.encodeToString(ciphertext), wrapper)

        assertArrayEquals(envelope, plaintext)
    }

    @Test
    fun `the rfc 8291 example decrypts`() {
        // RFC 8291 Appendix A: the receiver's keys and the encrypted message.
        val generator = KeyPairGenerator.getInstance("EC").apply { initialize(ECGenParameterSpec("secp256r1")) }
        val params = (generator.generateKeyPair().private as ECPrivateKey).params
        val scalar = BigInteger(1, urlDecoder.decode("q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94"))
        val privateKey = KeyFactory.getInstance("EC").generatePrivate(ECPrivateKeySpec(scalar, params))
        val endpoint = PushEndpoint.Fcm(
            token = "token",
            p256dh = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
            auth = "BTBZMqHH6r4Tts7J_aSIgg",
            wrappedPrivateKey = wrapper.wrap(privateKey.encoded),
        )
        val message = listOf(
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmY",
            "WAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexS",
            "gSxsj_Qulcy4a-fN",
        ).joinToString("")

        val plaintext = FcmEnvelopeKeys.decrypt(endpoint, message, wrapper)

        assertEquals("When I grow up, I want to be a watermelon", plaintext?.decodeToString())
    }

    @Test
    fun `other keys, garbage and an unopenable private key decrypt to nothing`() {
        val keys = FcmEnvelopeKeys.generate(wrapper)
        val other = FcmEnvelopeKeys.generate(wrapper)
        val ciphertext = WebPushHybridEncrypt.Builder()
            .withAuthSecret(urlDecoder.decode(other.auth))
            .withRecipientPublicKey(urlDecoder.decode(other.p256dh))
            .build()
            .encrypt("secret".encodeToByteArray(), null)

        assertNull(FcmEnvelopeKeys.decrypt(endpointOf(keys), url.encodeToString(ciphertext), wrapper))
        assertNull(FcmEnvelopeKeys.decrypt(endpointOf(keys), "not*base64", wrapper))
        assertNull(FcmEnvelopeKeys.decrypt(endpointOf(keys), url.encodeToString(ByteArray(8)), wrapper))
        val lost = endpointOf(keys).copy(wrappedPrivateKey = "%%%")
        assertNull(FcmEnvelopeKeys.decrypt(lost, url.encodeToString(ciphertext), wrapper))
    }

    private fun endpointOf(keys: FcmEnvelopeKeys.Generated) = PushEndpoint.Fcm(
        token = "token",
        p256dh = keys.p256dh,
        auth = keys.auth,
        wrappedPrivateKey = keys.wrappedPrivateKey,
    )

    private companion object {
        val url: Base64.Encoder = Base64.getUrlEncoder().withoutPadding()
        val urlDecoder: Base64.Decoder = Base64.getUrlDecoder()
    }
}
