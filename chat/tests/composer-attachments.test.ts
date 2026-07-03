import { describe, expect, test } from "bun:test";
import {
  attachmentName,
  attachmentPreviewKind,
  formatFileSize,
} from "../src/components/chat/composer-attachments";

describe("attachmentPreviewKind", () => {
  test("maps media types onto preview kinds", () => {
    expect(attachmentPreviewKind("image/png", "shot.png")).toBe("image");
    expect(attachmentPreviewKind("video/mp4", "clip.mp4")).toBe("video");
    expect(attachmentPreviewKind("audio/mpeg", "song.mp3")).toBe("audio");
    expect(attachmentPreviewKind("application/pdf", "doc.pdf")).toBe("pdf");
    expect(attachmentPreviewKind("application/zip", "archive.zip")).toBe("file");
    expect(attachmentPreviewKind(undefined, undefined)).toBe("file");
  });
});

describe("formatFileSize", () => {
  test("scales through B / KB / MB", () => {
    expect(formatFileSize(512)).toBe("512 B");
    expect(formatFileSize(2048)).toBe("2.0 KB");
    expect(formatFileSize(1536)).toBe("1.5 KB");
    expect(formatFileSize(3 * 1024 * 1024)).toBe("3.0 MB");
  });
});

describe("attachmentName", () => {
  test("uses the File name when present", () => {
    const file = new File(["x"], "notes.txt", { type: "text/plain" });
    expect(attachmentName(file)).toBe("notes.txt");
  });

  test("falls back to a generated .bin name for anonymous blobs", () => {
    const blob = new Blob(["x"], { type: "application/octet-stream" });
    expect(attachmentName(blob)).toMatch(/^attachment-\d+\.bin$/);
  });
});
