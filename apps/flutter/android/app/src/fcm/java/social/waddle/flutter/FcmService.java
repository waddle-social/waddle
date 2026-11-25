package social.waddle.flutter;

import android.content.Intent;
import android.os.Bundle;
import com.google.firebase.messaging.FirebaseMessagingService;
import com.google.firebase.messaging.RemoteMessage;
import java.util.Map;

public final class FcmService extends FirebaseMessagingService {
    @Override
    public void handleIntent(Intent intent) {
        // Firebase otherwise displays notification messages before onMessageReceived.
        Bundle extras = intent.getExtras();
        if (extras != null && new RemoteMessage(extras).getNotification() != null) return;
        super.handleIntent(intent);
    }

    @Override
    public void onMessageReceived(RemoteMessage message) {
        try {
            Map<String,Object> event = FcmKeys.open(this, message.getData().get("payload"));
            if (event != null) FcmBridge.deliver(this, event);
        } catch (Exception invalid) {
            // Unknown, corrupt, or retired recipient. No notification is generated.
        }
    }

    @Override
    public void onNewToken(String token) {
        try {
            String instance = FcmKeys.instance(this);
            if (instance != null) FcmBridge.deliver(this, Map.of("type", "fcmTokenChanged", "instance", instance));
        } catch (Exception invalid) {
            // The next reconciliation reads the current token for its recipient.
        }
    }
}
