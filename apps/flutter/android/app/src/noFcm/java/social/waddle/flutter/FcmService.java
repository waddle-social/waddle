package social.waddle.flutter;

import android.app.Service;
import android.content.Intent;
import android.os.IBinder;

/** This manifest component is disabled when the build has no FCM configuration. */
public final class FcmService extends Service {
    @Override public IBinder onBind(Intent intent) { return null; }
}
