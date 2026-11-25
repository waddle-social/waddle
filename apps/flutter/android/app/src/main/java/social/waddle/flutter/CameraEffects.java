package social.waddle.flutter;

import android.os.Handler;
import android.os.Looper;
import androidx.annotation.Keep;
import com.cloudwebrtc.webrtc.FlutterWebRTCPlugin;
import com.cloudwebrtc.webrtc.LocalTrack;
import com.cloudwebrtc.webrtc.video.LocalVideoTrack;
import io.flutter.embedding.engine.FlutterEngine;
import io.flutter.plugin.common.MethodCall;
import io.flutter.plugin.common.MethodChannel;
import java.nio.ByteBuffer;
import java.util.Arrays;
import java.util.HashMap;
import java.util.Map;
import org.webrtc.JavaI420Buffer;
import org.webrtc.VideoFrame;
import org.webrtc.VideoSink;
import org.webrtc.VideoTrack;

/** Transforms the local camera before its frames reach either the preview or sender. */
@Keep
final class CameraEffects {
    private final Handler main = new Handler(Looper.getMainLooper());
    private final Map<String, Processor> processors = new HashMap<>();
    private static final byte[] BLACK = new byte[1024];
    private static final byte[] NEUTRAL = new byte[1024];
    static {
        Arrays.fill(BLACK, (byte) 16);
        Arrays.fill(NEUTRAL, (byte) 128);
    }

    static void register(FlutterEngine engine) {
        CameraEffects effects = new CameraEffects();
        new MethodChannel(engine.getDartExecutor().getBinaryMessenger(),
                "social.waddle.flutter/camera_effects").setMethodCallHandler(effects::set);
    }

    private void set(MethodCall call, MethodChannel.Result result) {
        if (!"set".equals(call.method)) {
            result.notImplemented();
            return;
        }
        String id = call.argument("trackId");
        String mode = call.argument("mode");
        if ("off".equals(mode)) {
            Processor previous = processors.remove(id);
            if (previous != null) previous.close();
            result.success(null);
            return;
        }
        FlutterWebRTCPlugin plugin = FlutterWebRTCPlugin.sharedSingleton;
        LocalTrack local = plugin == null || id == null ? null : plugin.getLocalTrack(id);
        if (!(local instanceof LocalVideoTrack camera) || !camera.isPrimaryCamera) {
            result.error("camera_unavailable", "This track is not a local camera source.", null);
            return;
        }
        byte[] image = call.argument("rgba");
        int width = dimension(call, "width"), height = dimension(call, "height");
        if (!("blur".equals(mode) && image == null && width == 0 && height == 0) &&
                !("image".equals(mode) && image != null && width > 0 && height > 0 &&
                width <= 1024 && height <= 1024 && image.length == width * height * 4)) {
            result.error("invalid_effect", "Choose a blur or a background image.", null);
            return;
        }
        Processor next = new Processor(id, camera, image, width, height, result);
        Processor previous = processors.put(id, next);
        camera.addProcessor(next);
        // Before publication WebRTC drops frames if there are no sinks. This
        // local sink establishes the first transformed frame without a sender.
        ((VideoTrack) camera.track).addSink(next.probe);
        if (previous != null) previous.close();
        main.postDelayed(next.timeout, 10000);
    }

    private static int dimension(MethodCall call, String key) {
        Object value = call.argument(key);
        return value == null ? 0 : value instanceof Integer ? (Integer) value : -1;
    }

    private final class Processor implements LocalVideoTrack.ExternalVideoFrameProcessing {
        final String id;
        final LocalVideoTrack track;
        final int width, height;
        final Runnable timeout = () -> complete(false);
        final VideoSink probe = frame -> {};
        private MethodChannel.Result ready;
        // Only the capture thread touches these until removeProcessor has returned.
        private byte[] image;
        private boolean initialized;
        private long handle;
        private boolean reported;

        Processor(String id, LocalVideoTrack track, byte[] image, int width, int height, MethodChannel.Result ready) {
            this.id = id;
            this.track = track;
            this.image = image;
            this.width = width;
            this.height = height;
            this.ready = ready;
        }

        @Override
        public VideoFrame onFrame(VideoFrame frame) {
            int w = frame.getBuffer().getWidth(), h = frame.getBuffer().getHeight();
            JavaI420Buffer dest = JavaI420Buffer.allocate(w, h);
            VideoFrame.I420Buffer src = null;
            boolean applied = false;
            try {
                if (!initialized) {
                    initialized = true;
                    System.loadLibrary("waddle_flutter_runtime");
                    handle = create(image, width, height);
                    image = null;
                }
                if (handle != 0) src = frame.getBuffer().toI420();
                if (src != null) {
                    applied = process(handle, w, h, frame.getRotation(),
                            src.getDataY(), src.getStrideY(), src.getDataU(), src.getStrideU(), src.getDataV(), src.getStrideV(),
                            dest.getDataY(), dest.getStrideY(), dest.getDataU(), dest.getStrideU(), dest.getDataV(), dest.getStrideV());
                }
            } catch (RuntimeException | LinkageError unavailable) {
                // An enabled camera effect must never pass an unprocessed frame.
            } finally {
                if (src != null) src.release();
            }
            if (!applied) {
                fill(dest.getDataY(), BLACK);
                fill(dest.getDataU(), NEUTRAL);
                fill(dest.getDataV(), NEUTRAL);
            }
            if (!reported) {
                reported = true;
                final boolean success = applied;
                main.post(() -> complete(success));
            }
            return new VideoFrame(dest, frame.getRotation(), frame.getTimestampNs());
        }

        void complete(boolean success) {
            if (ready == null) return;
            main.removeCallbacks(timeout);
            ((VideoTrack) track.track).removeSink(probe);
            MethodChannel.Result result = ready;
            ready = null;
            if (success) {
                result.success(null);
            } else {
                if (processors.remove(id, this)) close();
                result.error("camera_effect_unavailable", "The camera background could not be applied.", null);
            }
        }

        void close() {
            main.removeCallbacks(timeout);
            // The plugin holds this same lock while processing the current frame.
            track.removeProcessor(this);
            if (handle != 0) {
                destroy(handle);
                handle = 0;
            }
            if (ready != null) complete(false);
        }
    }

    private static void fill(ByteBuffer buffer, byte[] block) {
        while (buffer.hasRemaining()) buffer.put(block, 0, Math.min(buffer.remaining(), block.length));
    }

    private static native long create(byte[] rgba, int width, int height);
    private static native boolean process(long handle, int width, int height, int rotation,
            ByteBuffer y, int sy, ByteBuffer u, int su, ByteBuffer v, int sv,
            ByteBuffer dy, int dsy, ByteBuffer du, int dsu, ByteBuffer dv, int dsv);
    private static native void destroy(long handle);
}
