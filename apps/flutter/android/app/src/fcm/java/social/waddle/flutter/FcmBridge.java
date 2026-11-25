package social.waddle.flutter;

import android.content.Context;
import android.os.Handler;
import android.os.Looper;
import com.google.android.gms.tasks.Tasks;
import com.google.firebase.messaging.FirebaseMessaging;
import io.flutter.embedding.engine.FlutterEngine;
import io.flutter.plugin.common.MethodChannel;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;

/** Hands authenticated recipient events to the single foreground or headless Dart owner. */
final class FcmBridge {
    private static final Handler MAIN = new Handler(Looper.getMainLooper());
    private static final ExecutorService TOKENS = Executors.newSingleThreadExecutor();
    private static final List<Pending> waiting = new ArrayList<>();
    private static MethodChannel channel;
    private static String readyInstance;

    private static final class Pending {
        final Map<String,Object> event;
        final CompletableFuture<Void> finished = new CompletableFuture<>();
        Pending(Map<String,Object> event) { this.event = event; }
    }

    static void register(FlutterEngine engine, Context context) {
        channel = new MethodChannel(engine.getDartExecutor().getBinaryMessenger(), "social.waddle.flutter/fcm");
        readyInstance = null;
        channel.setMethodCallHandler((call, result) -> {
            String instance = call.arguments instanceof String ? (String) call.arguments : null;
            switch(call.method) {
                case "available":
                    result.success(true);
                    return;
                case "ready":
                    readyInstance = instance;
                    result.success(null);
                    for (Pending item : new ArrayList<>(waiting)) dispatch(item);
                    return;
                case "subscribe":
                case "unsubscribe":
                    if (instance == null || instance.isEmpty() || instance.length() > 256) {
                        result.error("invalid_instance", "Invalid push recipient.", null);
                        return;
                    }
                    TOKENS.execute(() -> {
                        try {
                            Object response = null;
                            if ("subscribe".equals(call.method)) {
                                Map<String,String> keys = FcmKeys.subscribe(context, instance);
                                String token = Tasks.await(FirebaseMessaging.getInstance().getToken(), 15, TimeUnit.SECONDS);
                                keys.put("registration_token", token);
                                response = keys;
                            } else if (FcmKeys.retire(context, instance)) {
                                Tasks.await(FirebaseMessaging.getInstance().deleteToken(), 15, TimeUnit.SECONDS);
                            }
                            Object value = response;
                            MAIN.post(() -> result.success(value));
                        } catch (Exception failure) {
                            MAIN.post(() -> result.error("fcm_unavailable", "Google Play push is unavailable.", null));
                        }
                    });
                    return;
                default:
                    result.notImplemented();
            }
        });
    }

    // Firebase invokes services on a worker. Keep the high-priority delivery window
    // open until Dart has posted (or discarded) the notification.
    static void deliver(Context context, Map<String,Object> event) {
        Pending item = new Pending(event);
        MAIN.post(() -> {
            if (item.finished.isDone() || waiting.size() >= 16) { item.finished.complete(null); return; }
            WaddleEngine.getOrCreate(context, true);
            waiting.add(item);
            dispatch(item);
        });
        try {
            item.finished.get(15, TimeUnit.SECONDS);
        } catch (Exception expired) {
            item.finished.complete(null);
        } finally {
            MAIN.post(() -> waiting.remove(item));
        }
    }

    private static void dispatch(Pending item) {
        if (readyInstance == null || item.finished.isDone() || !waiting.remove(item)) return;
        if (!readyInstance.equals(item.event.get("instance"))) {
            item.finished.complete(null);
            return;
        }
        channel.invokeMethod("event", item.event, new MethodChannel.Result() {
            @Override public void success(Object value) { item.finished.complete(null); }
            @Override public void error(String code, String message, Object details) { item.finished.complete(null); }
            @Override public void notImplemented() { item.finished.complete(null); }
        });
    }
}
