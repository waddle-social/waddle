package social.waddle.flutter;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.ResultReceiver;
import androidx.core.content.ContextCompat;
import io.flutter.embedding.engine.FlutterEngine;
import io.flutter.plugin.common.MethodChannel;

/** Keeps MediaProjection active only while the Flutter call is sharing the screen. */
public final class ScreenCaptureService extends Service {
    private static final String CHANNEL_ID = "waddle-flutter-screen-share";
    private static final String STARTED = "started";
    private static final int NOTIFICATION_ID = 9004;

    static void register(FlutterEngine engine, Context context) {
        MethodChannel channel = new MethodChannel(
                engine.getDartExecutor().getBinaryMessenger(), "social.waddle.flutter/screen_capture");
        ContextCompat.registerReceiver(context, new BroadcastReceiver() {
            @Override
            public void onReceive(Context app, Intent intent) {
                String trackId = intent.getStringExtra("trackId");
                if (trackId != null) channel.invokeMethod("revoked", trackId);
            }
        }, new IntentFilter(context.getPackageName() + ".SCREEN_CAPTURE_REVOKED"),
                ContextCompat.RECEIVER_NOT_EXPORTED);
        channel.setMethodCallHandler((call, result) -> {
            Intent service = new Intent(context, ScreenCaptureService.class);
            switch (call.method) {
                case "start":
                    // On Android 14+, capture may begin only after startForeground succeeds.
                    service.putExtra(STARTED, new ResultReceiver(new Handler(Looper.getMainLooper())) {
                        @Override
                        protected void onReceiveResult(int code, Bundle data) {
                            if (code == 0) {
                                result.success(null);
                            } else {
                                result.error("screen_capture_denied", "Android could not start screen sharing.", null);
                            }
                        }
                    });
                    try {
                        context.startForegroundService(service);
                    } catch (RuntimeException denied) {
                        result.error("screen_capture_denied", "Android could not start screen sharing.", null);
                    }
                    break;
                case "stop":
                    context.stopService(service);
                    result.success(null);
                    break;
                default:
                    result.notImplemented();
            }
        });
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        ResultReceiver receiver = intent == null ? null : intent.getParcelableExtra(STARTED);
        if (receiver == null) {
            stopSelf(startId);
            return START_NOT_STICKY;
        }
        NotificationManager manager = getSystemService(NotificationManager.class);
        manager.createNotificationChannel(new NotificationChannel(
                CHANNEL_ID, "Screen sharing", NotificationManager.IMPORTANCE_LOW));
        PendingIntent open = PendingIntent.getActivity(this, 0, new Intent(this, MainActivity.class),
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        Notification notification = new Notification.Builder(this, CHANNEL_ID)
                .setSmallIcon(R.mipmap.ic_launcher)
                .setContentTitle("Waddle is sharing your screen")
                .setContentIntent(open)
                .setOngoing(true)
                .build();
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION);
            } else {
                startForeground(NOTIFICATION_ID, notification);
            }
            receiver.send(0, null);
        } catch (RuntimeException denied) {
            receiver.send(1, null);
            stopSelf(startId);
        }
        return START_NOT_STICKY;
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
