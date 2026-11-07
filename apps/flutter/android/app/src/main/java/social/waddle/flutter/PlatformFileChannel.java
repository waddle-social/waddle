package social.waddle.flutter;

import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.ContentResolver;
import android.content.Context;
import android.net.Uri;
import io.flutter.embedding.engine.FlutterEngine;
import io.flutter.plugin.common.BinaryMessenger;
import io.flutter.plugin.common.MethodChannel;
import io.flutter.plugin.common.StandardMethodCodec;
import java.io.IOException;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;

/** Reads clipboard images and resolves metadata for selected content. */
final class PlatformFileChannel {
    private static final String CHANNEL = "social.waddle.flutter/platform_files";
    private static final int MAX_CLIPBOARD_BYTES = 10 * 1024 * 1024 - 16;

    private PlatformFileChannel() {}

    static void register(FlutterEngine engine, Context context) {
        ContentResolver resolver = context.getContentResolver();
        BinaryMessenger messenger = engine.getDartExecutor().getBinaryMessenger();
        // Cleanup must not wait for a stalled content provider.
        BinaryMessenger.TaskQueue queue = messenger.makeBackgroundTaskQueue(
                new BinaryMessenger.TaskQueueOptions().setIsSerial(false));
        MethodChannel channel = new MethodChannel(messenger, CHANNEL, StandardMethodCodec.INSTANCE, queue);
        channel.setMethodCallHandler((call, result) -> {
            if ("clipboardImage".equals(call.method)) {
                readClipboardImage(context, resolver, result);
                return;
            }
            if ("removePickedCopies".equals(call.method)) {
                if (!(call.arguments instanceof List<?>)) {
                    result.error("invalid_argument", "Expected a list of cache paths.", null);
                    return;
                }
                result.success(PickerCache.removeCopies(context.getCacheDir(), (List<?>) call.arguments));
                return;
            }
            if (!"mimeTypes".equals(call.method)) {
                result.notImplemented();
                return;
            }
            if (!(call.arguments instanceof List<?>)) {
                result.error("invalid_argument", "Expected a list of content URIs.", null);
                return;
            }
            List<String> types = new ArrayList<>();
            for (Object value : (List<?>) call.arguments) {
                String type = null;
                if (value instanceof String) {
                    try {
                        Uri uri = Uri.parse((String) value);
                        if ("content".equals(uri.getScheme()) && uri.getAuthority() != null) {
                            type = resolver.getType(uri);
                        }
                    } catch (RuntimeException ignored) {
                        // The temporary grant may have expired or the provider may be unavailable.
                    }
                }
                types.add(type);
            }
            result.success(types);
        });
    }

    private static void readClipboardImage(
            Context context, ContentResolver resolver, MethodChannel.Result result) {
        try {
            ClipboardManager clipboard = (ClipboardManager) context.getSystemService(Context.CLIPBOARD_SERVICE);
            ClipData clip = clipboard == null ? null : clipboard.getPrimaryClip();
            if (clip == null || clip.getItemCount() == 0) {
                result.success(null);
                return;
            }
            Uri uri = clip.getItemAt(0).getUri();
            if (uri == null || !"content".equals(uri.getScheme())) {
                result.success(null);
                return;
            }
            String mime = resolver.getType(uri);
            if (mime == null || !mime.toLowerCase(Locale.ROOT).startsWith("image/")) {
                result.success(null);
                return;
            }
            try (InputStream stream = resolver.openInputStream(uri)) {
                if (stream == null) {
                    result.success(null);
                    return;
                }
                Map<String, Object> image = new HashMap<>();
                image.put("bytes", BoundedInput.read(stream, MAX_CLIPBOARD_BYTES));
                image.put("mimeType", mime);
                result.success(image);
            }
        } catch (BoundedInput.LimitExceeded ignored) {
            result.error("file_too_large", "The clipboard image exceeds the upload limit.", null);
        } catch (IOException | RuntimeException ignored) {
            result.error("clipboard_unavailable", "Could not read the clipboard image.", null);
        }
    }
}
