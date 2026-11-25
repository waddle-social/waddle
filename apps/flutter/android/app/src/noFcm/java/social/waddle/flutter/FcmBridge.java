package social.waddle.flutter;

import android.content.Context;
import io.flutter.embedding.engine.FlutterEngine;

/** Builds without Firebase expose no FCM platform channel. */
final class FcmBridge {
    static void register(FlutterEngine engine, Context context) {}
}
