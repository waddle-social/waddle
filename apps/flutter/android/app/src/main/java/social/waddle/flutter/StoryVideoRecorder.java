package social.waddle.flutter;

import android.content.Context;
import android.media.MediaRecorder;
import android.os.Build;
import android.view.Surface;
import com.cloudwebrtc.webrtc.FlutterWebRTCPlugin;
import com.cloudwebrtc.webrtc.LocalTrack;
import com.cloudwebrtc.webrtc.utils.EglUtils;
import io.flutter.embedding.engine.FlutterEngine;
import io.flutter.plugin.common.MethodChannel;
import java.io.File;
import java.io.IOException;
import org.webrtc.EglBase;
import org.webrtc.EglRenderer;
import org.webrtc.GlRectDrawer;
import org.webrtc.VideoTrack;

/** Records the preview and microphone into one private MP4 without starting a call. */
final class StoryVideoRecorder {
    private final Context context;
    private MediaRecorder recorder;
    private EglRenderer renderer;
    private Surface surface;
    private VideoTrack track;
    private String recordingId;

    private StoryVideoRecorder(Context context) {
        this.context = context;
    }

    static void register(FlutterEngine engine, Context context) {
        StoryVideoRecorder camera = new StoryVideoRecorder(context);
        new MethodChannel(engine.getDartExecutor().getBinaryMessenger(),
                "social.waddle.flutter/story_video").setMethodCallHandler((call, result) -> {
            try {
                String id = call.argument("id");
                if ("start".equals(call.method)) {
                    Boolean portrait = call.argument("portrait");
                    camera.start(id, call.argument("trackId"), Boolean.TRUE.equals(portrait));
                } else if ("stop".equals(call.method)) {
                    camera.stop(id);
                } else {
                    result.notImplemented();
                    return;
                }
                result.success(null);
            } catch (IOException | RuntimeException error) {
                result.error("story_recording_failed", error.getMessage(), null);
            }
        });
    }

    @SuppressWarnings("deprecation")
    private void start(String id, String trackId, boolean portrait) throws IOException {
        if (recorder != null) throw new IllegalStateException("A recording is already active.");
        if (id == null || !id.matches("story-[A-Za-z0-9]+")) {
            throw new IOException("Invalid recording directory.");
        }
        File directory = new File(context.getCacheDir(), id).getCanonicalFile();
        if (!directory.isDirectory() || !context.getCacheDir().getCanonicalFile().equals(directory.getParentFile())) {
            throw new IOException("The recording directory is unavailable.");
        }
        FlutterWebRTCPlugin plugin = FlutterWebRTCPlugin.sharedSingleton;
        LocalTrack local = plugin == null ? null : plugin.getLocalTrack(trackId);
        if (local == null || !(local.track instanceof VideoTrack)) {
            throw new IllegalStateException("The camera track is unavailable.");
        }
        int width = portrait ? 480 : 640;
        int height = portrait ? 640 : 480;
        try {
            recorder = Build.VERSION.SDK_INT >= 31 ? new MediaRecorder(context) : new MediaRecorder();
            recorder.setAudioSource(MediaRecorder.AudioSource.MIC);
            recorder.setVideoSource(MediaRecorder.VideoSource.SURFACE);
            recorder.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4);
            recorder.setVideoEncoder(MediaRecorder.VideoEncoder.H264);
            recorder.setAudioEncoder(MediaRecorder.AudioEncoder.AAC);
            recorder.setVideoSize(width, height);
            recorder.setVideoFrameRate(15);
            recorder.setVideoEncodingBitRate(1600000);
            recorder.setAudioEncodingBitRate(96000);
            recorder.setAudioSamplingRate(48000);
            recorder.setMaxFileSize(10 * 1024 * 1024 - 128 * 1024);
            recorder.setOutputFile(new File(directory, "story.mp4").getPath());
            recorder.prepare();
            surface = recorder.getSurface();
            renderer = new EglRenderer("StoryRecorder");
            renderer.init(EglUtils.getRootEglBaseContext(), EglBase.CONFIG_RECORDABLE, new GlRectDrawer(), true);
            renderer.setLayoutAspectRatio((float) width / height);
            renderer.setFpsReduction(15);
            renderer.createEglSurface(surface);
            recorder.start();
            track = (VideoTrack) local.track;
            track.addSink(renderer);
            recordingId = id;
        } catch (IOException | RuntimeException error) {
            release();
            throw error;
        }
    }

    private void stop(String id) {
        if (recordingId == null || !recordingId.equals(id)) return;
        try {
            recorder.stop();
        } finally {
            release();
        }
    }

    private void release() {
        if (track != null) track.removeSink(renderer);
        if (renderer != null) renderer.release();
        if (surface != null) surface.release();
        if (recorder != null) recorder.release();
        track = null;
        renderer = null;
        surface = null;
        recorder = null;
        recordingId = null;
    }
}
