import type { LinkPreview, LinkPreviewVideo } from "@/lib/chat-ui";
import { isVideoFile } from "@/lib/message-media";
import type { SharedFileInfo } from "./types";

/**
 * Tie a server-stamped XEP-0447 inline-video file-share to the XEP-0511 link
 * preview that describes the same URL.
 *
 * The server stamps a direct-video link preview as two conformant elements: an
 * XEP-0511 `<rdf:Description>` link card and an XEP-0447 inline `<file-sharing>`
 * whose source is the playable URL. The client surfaces these as a `LinkPreview`
 * and a `SharedFileInfo` respectively. Associating them here lets the dedicated
 * video preview card own the file-share (play-on-click, no preload) instead of
 * the generic attachment renderer, which would otherwise show it twice and
 * auto-fetch its metadata.
 *
 * Trust anchor: only the server can stamp the XEP-0511 card (client-authored
 * link metadata is stripped server-side), so a file-share is only promoted to a
 * preview video when its URL matches a server-stamped preview.
 */
export function associateDirectVideoPreviews(
  linkPreviews: LinkPreview[],
  sharedFiles: SharedFileInfo[],
): { linkPreviews: LinkPreview[]; sharedFiles: SharedFileInfo[] } {
  const consumed = new Set<SharedFileInfo>();

  const associated = linkPreviews.map((preview) => {
    if (preview.video) return preview;
    const match = sharedFiles.find(
      (file) =>
        !consumed.has(file)
        && file.disposition === "inline"
        && isVideoFile(file.mediaType, file.url)
        && matchesPreviewUrl(preview, file.url),
    );
    if (!match) return preview;
    consumed.add(match);
    return { ...preview, video: previewVideoFromShared(match) };
  });

  return {
    linkPreviews: associated,
    sharedFiles: sharedFiles.filter((file) => !consumed.has(file)),
  };
}

function matchesPreviewUrl(preview: LinkPreview, url: string): boolean {
  return url === (preview.normalizedUrl ?? preview.originalUrl) || url === preview.originalUrl;
}

function previewVideoFromShared(file: SharedFileInfo): LinkPreviewVideo {
  return {
    url: file.url,
    mediaType: file.mediaType ?? "video/mp4",
    ...(typeof file.size === "number" ? { size: file.size } : {}),
  };
}
