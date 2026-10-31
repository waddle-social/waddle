package social.waddle.flutter;

import android.content.Context;
import android.content.Intent;
import android.os.Bundle;
import androidx.annotation.NonNull;
import io.flutter.embedding.android.FlutterActivity;
import io.flutter.embedding.engine.FlutterEngine;

/** Attaches the UI without replacing a Flutter-owned call or background session. */
public final class MainActivity extends FlutterActivity {
    private boolean reusedEngine;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        // A cached headless engine already initialized notification callbacks.
        // Deliver the first Activity intent through the normal plugin path too.
        if (reusedEngine && getIntent() != null && getFlutterEngine() != null
                && (getIntent().getFlags() & Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY) == 0) {
            getFlutterEngine().getActivityControlSurface().onNewIntent(getIntent());
        }
    }

    @Override
    @NonNull
    public FlutterEngine provideFlutterEngine(@NonNull Context context) {
        reusedEngine = WaddleEngine.isCached();
        return WaddleEngine.getOrCreate(context, false);
    }

    @Override
    public boolean shouldDestroyEngineWithHost() {
        return false;
    }
}
