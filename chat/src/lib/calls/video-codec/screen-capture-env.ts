/**
 * Whether the browser can be given an explicit screen-capture resolution.
 *
 * macOS Safari 17.x has a WebKit bug (https://bugs.webkit.org/show_bug.cgi?id=263015):
 * passing ANY resolution to `getDisplayMedia` collapses the capture to a low
 * resolution. livekit-client works around it by only injecting a default
 * resolution when the browser is NOT Safari-17-based — but it leaves an
 * explicitly-passed resolution untouched, so we must not pass one there
 * either, or we re-introduce the degraded capture. Elsewhere the bitrate
 * ceiling still bounds the encode, so leaving capture uncapped on Safari 17 is
 * safe.
 *
 * Pure over an injected user-agent so it is unit-tested without globals.
 */

export type ScreenCaptureEnv = {
  /** False on macOS Safari 17.x (see above); true everywhere else. */
  canConstrainResolution: boolean;
};

/** Pure: does this user-agent have the Safari-17 capture-resolution bug? */
export function hasSafari17CaptureBug(userAgent: string): boolean {
  // Apple Safari only — exclude every Chromium/Gecko engine that also carries
  // "Safari" (or runs on Apple hardware) in its UA.
  const isAppleSafari = /^((?!chrome|chromium|android|crios|fxios|edg).)*safari/i.test(userAgent);
  return isAppleSafari && /version\/17\./i.test(userAgent);
}

/** Read the real environment. Thin; the decision logic lives in the pure probe. */
export function currentScreenCaptureEnv(
  userAgent: string = typeof navigator === "undefined" ? "" : navigator.userAgent,
): ScreenCaptureEnv {
  return { canConstrainResolution: !hasSafari17CaptureBug(userAgent) };
}
