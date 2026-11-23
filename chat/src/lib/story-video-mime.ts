/**
 * Recording formats for a story video, most widely playable first.
 *
 * Apple devices play MP4 with H.264 and AAC and cannot decode WebM, so
 * MP4 comes first. H.264 and AAC are named because a bare `video/mp4`
 * lets a browser put codecs in the file that Apple devices cannot
 * play. A browser that records no MP4 (Firefox) falls back to WebM.
 */
const STORY_VIDEO_MIME_CANDIDATES = [
  "video/mp4;codecs=avc1.42E01E,mp4a.40.2",
  "video/mp4;codecs=avc1,mp4a.40.2",
  "video/mp4",
  "video/webm;codecs=vp9,opus",
  "video/webm;codecs=vp8,opus",
] as const;

/**
 * The first format the browser says it can record, or undefined to let
 * the browser choose. Only a format the browser reported as supported is
 * returned: older Safari throws when given a `mimeType` it does not list.
 */
export function pickStoryVideoMimeType(isSupported: (mime: string) => boolean): string | undefined {
  return STORY_VIDEO_MIME_CANDIDATES.find((mime) => isSupported(mime));
}
