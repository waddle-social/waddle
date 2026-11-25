package social.waddle.flutter;

import android.content.Context;
import android.content.SharedPreferences;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import com.google.crypto.tink.apps.fixed_webpush.WebPushHybridDecrypt;
import java.math.BigInteger;
import java.nio.ByteBuffer;
import java.security.GeneralSecurityException;
import java.security.KeyFactory;
import java.security.KeyPair;
import java.security.KeyPairGenerator;
import java.security.KeyStore;
import java.security.SecureRandom;
import java.security.interfaces.ECPrivateKey;
import java.security.interfaces.ECPublicKey;
import java.security.spec.ECGenParameterSpec;
import java.security.spec.PKCS8EncodedKeySpec;
import java.util.Base64;
import java.util.HashMap;
import java.util.Map;
import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;
import org.json.JSONObject;

/** One active recipient, sealed under this installation's Android Keystore. */
final class FcmKeys {
    private static final String ALIAS = "social.waddle.flutter.fcm";
    private static final Base64.Encoder ENCODE = Base64.getUrlEncoder().withoutPadding();
    private static final Base64.Decoder DECODE = Base64.getUrlDecoder();

    private static SharedPreferences prefs(Context context) {
        return context.getSharedPreferences(ALIAS, Context.MODE_PRIVATE);
    }

    private static SecretKey master() throws Exception {
        KeyStore store = KeyStore.getInstance("AndroidKeyStore");
        store.load(null);
        SecretKey existing = (SecretKey) store.getKey(ALIAS, null);
        if (existing != null) return existing;
        KeyGenerator generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
        generator.init(new KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256).build());
        return generator.generateKey();
    }

    private static JSONObject current(Context context) throws Exception {
        String sealed = prefs(context).getString("recipient", null);
        if (sealed == null) return null;
        byte[] record = DECODE.decode(sealed);
        Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
        cipher.init(Cipher.DECRYPT_MODE, master(), new GCMParameterSpec(128, record, 0, 12));
        return new JSONObject(new String(cipher.doFinal(record, 12, record.length - 12), java.nio.charset.StandardCharsets.UTF_8));
    }

    private static byte[] coordinate(BigInteger number) {
        byte[] source = number.toByteArray();
        byte[] dest = new byte[32];
        int length = Math.min(source.length, 32);
        System.arraycopy(source, source.length - length, dest, 32 - length, length);
        return dest;
    }

    static synchronized Map<String, String> subscribe(Context context, String instance) throws Exception {
        JSONObject keys;
        try { keys = current(context); } catch (GeneralSecurityException lostKey) { keys = null; }
        if (keys == null || !instance.equals(keys.getString("instance"))) {
            KeyPairGenerator generator = KeyPairGenerator.getInstance("EC");
            generator.initialize(new ECGenParameterSpec("secp256r1"));
            KeyPair pair = generator.generateKeyPair();
            var point = ((ECPublicKey) pair.getPublic()).getW();
            byte[] pub = ByteBuffer.allocate(65).put((byte) 4)
                    .put(coordinate(point.getAffineX())).put(coordinate(point.getAffineY())).array();
            byte[] auth = new byte[16];
            new SecureRandom().nextBytes(auth);
            keys = new JSONObject().put("instance", instance)
                    .put("p256dh", ENCODE.encodeToString(pub)).put("auth", ENCODE.encodeToString(auth))
                    .put("private", ENCODE.encodeToString(pair.getPrivate().getEncoded()));
            Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
            cipher.init(Cipher.ENCRYPT_MODE, master());
            byte[] content = cipher.doFinal(keys.toString().getBytes(java.nio.charset.StandardCharsets.UTF_8));
            byte[] sealed = ByteBuffer.allocate(cipher.getIV().length + content.length).put(cipher.getIV()).put(content).array();
            if (!prefs(context).edit().putString("recipient", ENCODE.encodeToString(sealed)).commit()) {
                throw new IllegalStateException("Could not store the push recipient");
            }
        }
        Map<String,String> credentials = new HashMap<>();
        credentials.put("p256dh", keys.getString("p256dh"));
        credentials.put("auth", keys.getString("auth"));
        return credentials;
    }

    static synchronized String instance(Context context) throws Exception {
        JSONObject keys = current(context);
        return keys == null ? null : keys.getString("instance");
    }

    static synchronized boolean retire(Context context, String instance) throws Exception {
        if (!instance.equals(instance(context))) return false;
        if (!prefs(context).edit().remove("recipient").commit()) throw new IllegalStateException("Could not retire push keys");
        return true;
    }

    static synchronized Map<String,Object> open(Context context, String ciphertext) throws Exception {
        // FCM data messages are limited to 4096 bytes, including the encoded payload.
        if (ciphertext == null || ciphertext.isEmpty() || ciphertext.length() > 4096) return null;
        JSONObject keys = current(context);
        if (keys == null) return null;
        ECPrivateKey secret = (ECPrivateKey) KeyFactory.getInstance("EC").generatePrivate(
                new PKCS8EncodedKeySpec(DECODE.decode(keys.getString("private"))));
        byte[] message = new WebPushHybridDecrypt.Builder()
                .withAuthSecret(DECODE.decode(keys.getString("auth")))
                .withRecipientPublicKey(DECODE.decode(keys.getString("p256dh")))
                .withRecipientPrivateKey(secret).build().decrypt(DECODE.decode(ciphertext), null);
        return Map.of("type","push","transport","fcm","instance",keys.getString("instance"),
                "decrypted",true,"content",message);
    }
}
