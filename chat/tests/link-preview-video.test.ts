import { describe, expect, test } from "bun:test";
import { associateDirectVideoPreviews } from "../src/lib/xmpp/link-preview-video";
import type { LinkPreview } from "../src/lib/chat-ui";
import type { SharedFileInfo } from "../src/lib/xmpp/types";

describe("associateDirectVideoPreviews", () => {
  test("attaches an inline video file-share to the matching link preview and removes it from shared files", () => {
    const previews: LinkPreview[] = [
      { originalUrl: "https://cdn.example.com/clip.mp4", normalizedUrl: "https://cdn.example.com/clip.mp4" },
    ];
    const sharedFiles: SharedFileInfo[] = [
      { url: "https://cdn.example.com/clip.mp4", mediaType: "video/mp4", size: 4096, disposition: "inline" },
    ];

    const result = associateDirectVideoPreviews(previews, sharedFiles);

    expect(result.linkPreviews[0].video).toEqual({
      url: "https://cdn.example.com/clip.mp4",
      mediaType: "video/mp4",
      size: 4096,
    });
    // The file-share is consumed by the preview, so the generic attachment
    // renderer never shows it (no double render, no preload auto-fetch).
    expect(result.sharedFiles).toHaveLength(0);
  });

  test("leaves unrelated inline video attachments untouched", () => {
    const previews: LinkPreview[] = [
      { originalUrl: "https://example.com/article", normalizedUrl: "https://example.com/article" },
    ];
    const sharedFiles: SharedFileInfo[] = [
      { url: "https://files.example.com/upload.mp4", mediaType: "video/mp4", disposition: "inline" },
    ];

    const result = associateDirectVideoPreviews(previews, sharedFiles);

    expect(result.linkPreviews[0].video).toBeUndefined();
    expect(result.sharedFiles).toHaveLength(1);
  });

  test("does not associate non-video file shares that happen to match a preview URL", () => {
    const previews: LinkPreview[] = [
      { originalUrl: "https://example.com/page", normalizedUrl: "https://example.com/page" },
    ];
    const sharedFiles: SharedFileInfo[] = [
      { url: "https://example.com/page", mediaType: "application/pdf", disposition: "attachment" },
    ];

    const result = associateDirectVideoPreviews(previews, sharedFiles);

    expect(result.linkPreviews[0].video).toBeUndefined();
    expect(result.sharedFiles).toHaveLength(1);
  });
});
