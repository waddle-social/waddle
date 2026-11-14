package social.waddle.flutter;

import android.content.Context;
import io.flutter.embedding.engine.FlutterEngine;
import org.unifiedpush.flutter.connector.UnifiedPushService;

/** Lets an incoming notification attach UI to the same Dart call/session owner. */
public final class FlutterPushService extends UnifiedPushService {
    @Override
    public FlutterEngine getEngine(Context context) {
        return WaddleEngine.getOrCreate(context, true);
    }
}
