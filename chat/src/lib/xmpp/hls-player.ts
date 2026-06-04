// Click-to-play attachment for native-video link previews. Progressive media and
// native-HLS-capable browsers (Safari/iOS) use the plain <video> src; HLS
// elsewhere uses a lazily-loaded hls.js so the library is only fetched on first
// HLS play. Video bytes are never proxied — playback streams from the origin.
import { isHlsMediaType } from "./native-video";

export type VideoPlaybackStrategy = "native-src" | "hls-js";

/// Pure decision: an HLS manifest needs hls.js unless the browser plays HLS
/// natively; everything else (progressive containers) uses the <video> src.
export function videoPlaybackStrategy(
  mediaType: string,
  canPlayNativeHls: boolean,
): VideoPlaybackStrategy {
  return isHlsMediaType(mediaType) && !canPlayNativeHls ? "hls-js" : "native-src";
}

export interface VideoAttachment {
  destroy(): void;
}

const NOOP_ATTACHMENT: VideoAttachment = { destroy() {} };

/// Attach a playable source to a freshly-mounted <video>. Returns a handle whose
/// `destroy()` MUST be called on unmount. `onFatalError` fires when playback
/// cannot start (hls.js unsupported, import failed, or a fatal media error) so
/// the caller can fall back to the link card.
export async function attachNativeVideo(
  video: HTMLVideoElement,
  url: string,
  mediaType: string,
  onFatalError: () => void,
): Promise<VideoAttachment> {
  const canPlayNativeHls = video.canPlayType("application/vnd.apple.mpegurl") !== "";
  if (videoPlaybackStrategy(mediaType, canPlayNativeHls) === "native-src") {
    video.src = url;
    return {
      destroy() {
        video.removeAttribute("src");
        try {
          video.load();
        } catch {
          // load() can throw on a detached element; nothing to recover.
        }
      },
    };
  }

  try {
    const { default: Hls } = await import("hls.js");
    if (!Hls.isSupported()) {
      onFatalError();
      return NOOP_ATTACHMENT;
    }
    const hls = new Hls({ enableWorker: true });
    hls.on(Hls.Events.ERROR, (_event, data) => {
      if (data.fatal) onFatalError();
    });
    hls.loadSource(url);
    hls.attachMedia(video);
    let destroyed = false;
    return {
      destroy() {
        if (destroyed) return;
        destroyed = true;
        hls.destroy();
      },
    };
  } catch {
    onFatalError();
    return NOOP_ATTACHMENT;
  }
}
