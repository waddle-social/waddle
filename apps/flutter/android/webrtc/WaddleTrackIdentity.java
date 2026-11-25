package org.webrtc;

/** RTP senders and media streams can expose different Java wrappers for a single native track. */
public final class WaddleTrackIdentity {
    private WaddleTrackIdentity() {}

    public static boolean same(MediaStreamTrack first, MediaStreamTrack second) {
        return first != null && second != null && !first.isDisposed() && !second.isDisposed()
                && first.getNativeMediaStreamTrack() == second.getNativeMediaStreamTrack();
    }
}
