package com.cloudwebrtc.webrtc;

import android.media.AudioFormat;
import android.os.Handler;
import android.os.Looper;
import io.flutter.plugin.common.BinaryMessenger;
import io.flutter.plugin.common.MethodCall;
import io.flutter.plugin.common.MethodChannel;
import java.nio.ByteBuffer;
import java.util.HashMap;
import java.util.HashSet;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.locks.ReentrantReadWriteLock;
import java.util.function.BooleanSupplier;
import org.webrtc.AudioTrack;
import org.webrtc.MediaStreamTrack;
import org.webrtc.PeerConnection;
import org.webrtc.RtpSender;
import org.webrtc.WaddleTrackIdentity;
import org.webrtc.audio.JavaAudioDeviceModule;

/** Exclusive microphone ownership for the PCM input of one WebRTC factory. */
final class WaddleAudioCapture implements JavaAudioDeviceModule.AudioBufferCallback {
    private final Map<String, AudioTrack> microphones = new HashMap<>();
    private final Map<String, PeerConnectionObserver> peers;
    private final BooleanSupplier recordsInput;
    private final Set<Object> recorders = new HashSet<>();
    private final ReentrantReadWriteLock delivery = new ReentrantReadWriteLock(true);
    private final ThreadLocal<AudioTrack> frameOwner = new ThreadLocal<>();
    private final MethodChannel channel;
    private final Handler main = new Handler(Looper.getMainLooper());
    private final ExecutorService loader = Executors.newSingleThreadExecutor();
    private int rate, channels, revision;
    private volatile boolean closed;
    private volatile String owner;
    private AudioTrack ownerTrack;
    // The capture thread and detachment share this monitor. Model loading runs outside it.
    private long handle;
    private boolean muted, discardFirst;

    WaddleAudioCapture(BinaryMessenger messenger, Map<String, PeerConnectionObserver> peers,
                       BooleanSupplier recordsInput) {
        this.peers = peers;
        this.recordsInput = recordsInput;
        this.channel = new MethodChannel(messenger, "social.waddle.flutter/audio_filter");
        channel.setMethodCallHandler(this::set);
    }

    void format(int sampleRate, int channelCount) {
        rate = sampleRate;
        channels = channelCount;
    }

    boolean leased() { return owner != null; }

    void add(AudioTrack track) { microphones.put(track.id(), track); }

    boolean allowsLocalSink(MediaStreamTrack track) {
        AudioTrack source = frameOwner.get();
        return source == null || WaddleTrackIdentity.same(source, track);
    }

    void setExternalRecording(Object recorder, boolean active) {
        if (active) {
            if (leased()) throw new IllegalStateException("The call owns the microphone.");
            recorders.add(recorder);
        } else {
            recorders.remove(recorder);
        }
    }

    void delivered(JavaAudioDeviceModule.SamplesReadyCallback callback,
                   JavaAudioDeviceModule.AudioSamples samples) {
        // WebRTC can skip onBuffer during shutdown while still notifying its sample observer.
        if (delivery.getReadHoldCount() == 0) return;
        try {
            if (!closed) callback.onWebRtcAudioRecordSamplesReady(samples);
        } finally {
            frameOwner.set(null);
            delivery.readLock().unlock();
        }
    }

    boolean reject(MediaStreamTrack track, MethodChannel.Result result) {
        return reject(track, null, null, result);
    }

    boolean reject(MediaStreamTrack track, PeerConnectionObserver target, String senderId,
                   MethodChannel.Result result) {
        if (!(track instanceof AudioTrack) || !leased()) return false;
        if (WaddleTrackIdentity.same(ownerTrack, track)
                && !competingSender(track, false, target, senderId)) return false;
        result.error("audio_filter_busy", "Another sender owns this microphone.", null);
        return true;
    }

    void remove(MediaStreamTrack track) {
        if (!(track instanceof AudioTrack) || track.isDisposed()) return;
        String id = track.id();
        if (!WaddleTrackIdentity.same(microphones.get(id), track)) return;
        microphones.remove(id);
        if (WaddleTrackIdentity.same(ownerTrack, track)) {
            // An RTP sender can retain its native track after its Flutter stream is disposed.
            track.setEnabled(false);
            clear();
        }
    }

    void enabled(MediaStreamTrack track, boolean enabled) {
        if (!WaddleTrackIdentity.same(ownerTrack, track)) return;
        delivery.writeLock().lock();
        try {
            muted = !enabled;
            discardFirst = true;
            if (handle != 0) reset(handle);
        } finally {
            delivery.writeLock().unlock();
        }
    }

    private boolean competingSender(MediaStreamTrack selected, boolean allowAttached,
                                    PeerConnectionObserver target, String senderId) {
        for (PeerConnectionObserver peer : peers.values()) {
            PeerConnection connection = peer.getPeerConnection();
            if (connection == null) continue;
            for (RtpSender sender : connection.getSenders()) {
                MediaStreamTrack track = sender.track();
                if (!(track instanceof AudioTrack)) continue;
                if (peer == target && sender.id().equals(senderId)) continue;
                if (!allowAttached || !WaddleTrackIdentity.same(selected, track)) return true;
                allowAttached = false;
            }
        }
        return false;
    }

    private void set(MethodCall call, MethodChannel.Result result) {
        if (!call.method.equals("set")) { result.notImplemented(); return; }
        Object trackArg = call.argument("trackId"), modeArg = call.argument("mode");
        if (!(trackArg instanceof String) || !(modeArg instanceof String)) {
            result.error("invalid_audio_filter", "Select a microphone and audio filter.", null);
            return;
        }
        String id = (String) trackArg, mode = (String) modeArg;
        if (mode.equals("off")) {
            if (id.equals(owner)) clear();
            result.success(null);
            return;
        }
        int effect = mode.equals("rnnoise") ? 1 : mode.equals("dtln") ? 2 : 0;
        AudioTrack track = microphones.get(id);
        if (effect == 0 || track == null || closed || rate == 0) {
            result.error("invalid_audio_filter", "This local microphone or filter is unavailable.", null);
            return;
        }
        if (microphones.size() != 1 || (leased() && !id.equals(owner))
                || competingSender(track, true, null, null)
                || recordsInput.getAsBoolean() || !recorders.isEmpty()) {
            result.error("audio_filter_busy", "Stop the other audio capture first.", null);
            return;
        }
        final int ticket = ++revision, sampleRate = rate, channelCount = channels;
        boolean enabled = track.enabled();
        delivery.writeLock().lock();
        try {
            owner = id;
            ownerTrack = track;
            muted = !enabled;
            discardFirst = true;
        } finally {
            delivery.writeLock().unlock();
        }
        loader.execute(() -> {
            long prepared;
            try {
                System.loadLibrary("waddle_flutter_runtime");
                prepared = create(effect, sampleRate, channelCount);
            } catch (LinkageError unavailable) {
                prepared = 0;
            }
            final long next = prepared;
            main.post(() -> {
                if (ticket != revision || !id.equals(owner)) {
                    if (next != 0) destroy(next);
                    result.error("stale_audio_filter", "The microphone changed.", null);
                    return;
                }
                if (next == 0) {
                    // Continue the previous mode, or remain silent until the caller stops or selects Off.
                    result.error("audio_filter_unavailable", "The audio filter could not initialize.", null);
                    return;
                }
                long previous;
                delivery.writeLock().lock();
                try {
                    previous = handle;
                    handle = next;
                    discardFirst = true;
                } finally {
                    delivery.writeLock().unlock();
                }
                if (previous != 0) destroy(previous);
                result.success(null);
            });
        });
    }

    @Override
    public long onBuffer(ByteBuffer pcm, int format, int nChannels,
                         int sampleRate, int bytes, long captureTimeNs) {
        // SamplesReadyCallback on this same thread follows nativeDataIsRecorded. Hold the
        // grant through native delivery so the next owner cannot inherit a predecessor's frame.
        delivery.readLock().lock();
        frameOwner.set(ownerTrack);
        if (owner == null && !closed) return captureTimeNs;
        // Silence covers pending initialization, muting, layout mismatch and DSP failure.
        // Discard the first read straddling any ownership or mute transition.
        boolean processed = false;
        if (!closed && !muted && !discardFirst && handle != 0 && pcm.isDirect() &&
                format == AudioFormat.ENCODING_PCM_16BIT && nChannels == channels &&
                sampleRate == rate && bytes == pcm.capacity()) {
            try {
                processed = process(handle, pcm, bytes);
            } catch (RuntimeException | LinkageError unavailable) {
                // The device still needs a complete silent frame when JNI cannot access its buffer.
            }
        }
        if (!processed) {
            for (int i = 0; i < pcm.capacity(); i++) pcm.put(i, (byte) 0);
        }
        discardFirst = false;
        return captureTimeNs;
    }

    private void clear() {
        ++revision;
        long previous;
        delivery.writeLock().lock();
        try {
            previous = handle;
            handle = 0;
            ownerTrack = null;
            owner = null;
        } finally {
            delivery.writeLock().unlock();
        }
        if (previous != 0) destroy(previous);
    }

    void close() {
        closed = true;
        if (ownerTrack != null) remove(ownerTrack);
        clear();
        microphones.clear();
        channel.setMethodCallHandler(null);
        loader.shutdown();
    }

    private static native long create(int mode, int sampleRate, int channels);
    private static native boolean process(long handle, ByteBuffer pcm, int bytes);
    private static native void reset(long handle);
    private static native void destroy(long handle);
}
