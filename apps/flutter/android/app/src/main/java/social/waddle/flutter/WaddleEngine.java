package social.waddle.flutter;

import android.content.Context;
import io.flutter.embedding.engine.FlutterEngine;
import io.flutter.embedding.engine.FlutterEngineCache;
import io.flutter.embedding.engine.dart.DartExecutor;
import java.util.Collections;

/** Shares only this app's Flutter engine between its activity and push wakeups. */
public final class WaddleEngine {
    private static final String ENGINE_ID = "social.waddle.flutter.runtime";

    private WaddleEngine() {}

    public static boolean isCached() {
        synchronized (FlutterEngineCache.class) {
            return FlutterEngineCache.getInstance().get(ENGINE_ID) != null;
        }
    }

    public static FlutterEngine getOrCreate(Context context, boolean headless) {
        synchronized (FlutterEngineCache.class) {
            FlutterEngineCache cache = FlutterEngineCache.getInstance();
            FlutterEngine engine = cache.get(ENGINE_ID);
            if (engine == null) {
                engine = new FlutterEngine(context.getApplicationContext());
                cache.put(ENGINE_ID, engine);
                engine.getDartExecutor().executeDartEntrypoint(
                    DartExecutor.DartEntrypoint.createDefault(),
                    headless ? Collections.singletonList("--unifiedpush-bg") : Collections.emptyList());
            }
            return engine;
        }
    }
}
