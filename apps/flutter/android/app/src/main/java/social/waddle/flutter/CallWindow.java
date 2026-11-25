package social.waddle.flutter;

import android.app.Activity;
import android.app.ActivityManager;
import android.app.PictureInPictureParams;
import android.content.pm.PackageManager;
import android.os.Handler;
import android.os.Looper;
import android.util.Rational;
import androidx.annotation.NonNull;
import io.flutter.embedding.engine.plugins.FlutterPlugin;
import io.flutter.embedding.engine.plugins.activity.ActivityAware;
import io.flutter.embedding.engine.plugins.activity.ActivityPluginBinding;
import io.flutter.plugin.common.MethodCall;
import io.flutter.plugin.common.MethodChannel;

/** The engine owns call eligibility; the attached activity owns the actual PiP state. */
public final class CallWindow implements FlutterPlugin, ActivityAware {
    private final Handler main = new Handler(Looper.getMainLooper());
    private final Runnable resolve = this::finishTransition;
    private Activity activity;
    private MethodChannel channel;
    private MethodChannel.Result pending;
    private boolean active;
    private boolean supported;
    private boolean requested;

    @Override
    public void onAttachedToEngine(@NonNull FlutterPluginBinding binding) {
        supported = binding.getApplicationContext().getPackageManager()
                .hasSystemFeature(PackageManager.FEATURE_PICTURE_IN_PICTURE);
        channel = new MethodChannel(binding.getBinaryMessenger(), "social.waddle.flutter/call_window");
        channel.setMethodCallHandler(this::handle);
    }

    @Override
    public void onDetachedFromEngine(@NonNull FlutterPluginBinding binding) {
        finishTransition();
        channel.setMethodCallHandler(null);
        channel = null;
        activity = null;
        active = false;
    }

    private boolean compact() {
        return activity != null && activity.isInPictureInPictureMode();
    }

    private void handle(MethodCall call, MethodChannel.Result result) {
        switch (call.method) {
            case "state":
                result.success(compact());
                break;
            case "setActive":
                if (Boolean.TRUE.equals(call.arguments) && !supported) {
                    result.error("pip_unavailable", "This device does not support picture-in-picture.", null);
                    break;
                }
                active = Boolean.TRUE.equals(call.arguments);
                if (!active && compact()) activity.finish();
                result.success(null);
                break;
            case "setCompact":
                change(Boolean.TRUE.equals(call.arguments), result);
                break;
            default:
                result.notImplemented();
        }
    }

    private void change(boolean enter, MethodChannel.Result result) {
        finishTransition();
        if (activity == null || activity.isFinishing() || compact() == enter || !active) {
            result.success(compact());
            return;
        }
        requested = enter;
        pending = result;
        try {
            if (enter) {
                PictureInPictureParams params = new PictureInPictureParams.Builder()
                        .setAspectRatio(new Rational(16, 9)).build();
                if (!activity.enterPictureInPictureMode(params)) {
                    finishTransition();
                    return;
                }
            } else {
                ActivityManager manager = activity.getSystemService(ActivityManager.class);
                for (ActivityManager.AppTask task : manager.getAppTasks()) {
                    if (task.getTaskInfo().id == activity.getTaskId()) {
                        task.moveToFront();
                        break;
                    }
                }
            }
            if (pending != null) main.postDelayed(resolve, 2000);
        } catch (IllegalStateException | SecurityException denied) {
            finishTransition();
        }
    }

    private void finishTransition() {
        main.removeCallbacks(resolve);
        if (pending != null) {
            MethodChannel.Result result = pending;
            pending = null;
            result.success(compact());
        }
    }

    void changed(Activity sender, boolean compact) {
        if (sender != activity) return;
        channel.invokeMethod("changed", compact);
        if (pending != null && compact == requested) finishTransition();
        if (compact && !active) activity.finish();
    }

    @Override
    public void onAttachedToActivity(@NonNull ActivityPluginBinding binding) {
        activity = binding.getActivity();
        changed(activity, compact());
    }

    @Override
    public void onDetachedFromActivity() {
        activity = null;
        finishTransition();
        if (channel != null) channel.invokeMethod("changed", false);
    }

    @Override
    public void onDetachedFromActivityForConfigChanges() {
        onDetachedFromActivity();
    }

    @Override
    public void onReattachedToActivityForConfigChanges(@NonNull ActivityPluginBinding binding) {
        onAttachedToActivity(binding);
    }
}
