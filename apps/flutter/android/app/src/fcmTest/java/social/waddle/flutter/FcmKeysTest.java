package social.waddle.flutter;

import android.content.Context;
import android.content.ContextWrapper;
import android.content.SharedPreferences;
import com.google.firebase.FirebaseApp;
import androidx.test.ext.junit.runners.AndroidJUnit4;
import androidx.test.platform.app.InstrumentationRegistry;
import com.google.crypto.tink.apps.fixed_webpush.WebPushHybridEncrypt;
import java.nio.charset.StandardCharsets;
import java.util.Base64;
import java.util.Map;
import org.junit.Test;
import org.junit.runner.RunWith;
import static org.junit.Assert.*;

@RunWith(AndroidJUnit4.class)
public final class FcmKeysTest {
    @Test
    public void sealedRecipientSurvivesReloadAndRejectsPriorKeys() throws Exception {
        Context context = new ContextWrapper(InstrumentationRegistry.getInstrumentation().getTargetContext()) {
            @Override public SharedPreferences getSharedPreferences(String name, int mode) {
                return super.getSharedPreferences("instrumentation." + name, mode);
            }
        };
        assertNotNull(FirebaseApp.getInstance().getOptions().getGcmSenderId());
        assertNotNull(FirebaseApp.getInstance().getOptions().getProjectId());
        String first = "fcm-test-account-issuer-one", second = "fcm-test-account-issuer-two";
        Map<String,String> keys = FcmKeys.subscribe(context, first);
        try {
            assertEquals(keys, FcmKeys.subscribe(context, first));
            assertFalse(keys.containsKey("private"));
            Base64.Decoder decode = Base64.getUrlDecoder();
            byte[] message = "{\"v\":1}".getBytes(StandardCharsets.UTF_8);
            byte[] sealed = new WebPushHybridEncrypt.Builder().withAuthSecret(decode.decode(keys.get("auth")))
                    .withRecipientPublicKey(decode.decode(keys.get("p256dh"))).build().encrypt(message, null);
            String wire = Base64.getUrlEncoder().withoutPadding().encodeToString(sealed);
            Map<String,Object> event = FcmKeys.open(context, wire);
            assertEquals(first, event.get("instance"));
            assertArrayEquals(message, (byte[]) event.get("content"));
            assertNull(FcmKeys.open(context, new String(new char[4097])));
            FcmKeys.subscribe(context, second);
            assertFalse(FcmKeys.retire(context, first));
            assertThrows(Exception.class, () -> FcmKeys.open(context, wire));
            assertTrue(FcmKeys.retire(context, second));
            assertNull(FcmKeys.open(context, wire));
        } finally {
            FcmKeys.retire(context, first);
            FcmKeys.retire(context, second);
        }
    }
}
