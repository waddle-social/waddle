import { describe, expect, it, mock } from "bun:test";
import { ref } from "vue";
import { useMessageAttachments } from "@/channels/message-attachments";
import type { TimelineMessage, TimelineSharedFile } from "@/lib/chat-ui";

function messageWithFiles(sharedFiles: TimelineSharedFile[]): TimelineMessage {
  return {
    id: "msg-1",
    author: "alice",
    body: "",
    createdAt: "2026-05-12T09:53:00Z",
    isSelf: false,
    sharedFiles,
  };
}

function imageFile(url: string, name: string): TimelineSharedFile {
  return {
    url,
    name,
    mediaType: "image/png",
    disposition: "inline",
  };
}

function pendingEncryptedImage(url: string, name: string): TimelineSharedFile {
  return {
    ...imageFile(url, name),
    encrypted: {
      cipher: "urn:xmpp:ciphers:aes-256-gcm-nopadding:0",
      keyB64: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
      ivB64: "AAAAAAAAAAAAAAAA",
      sources: [url],
    },
  };
}

describe("useMessageAttachments lightbox state", () => {
  it("opens a plain image gallery at the clicked image", () => {
    const first = imageFile("https://example.com/a.png", "a.png");
    const second = imageFile("https://example.com/b.png", "b.png");
    const attachments = useMessageAttachments(ref(messageWithFiles([first, second])));

    attachments.openLightbox(second);

    expect(attachments.lightboxOpen.value).toBe(true);
    expect(attachments.lightboxIndex.value).toBe(1);
    expect(attachments.lightboxImages.value).toHaveLength(2);
    expect(attachments.lightboxImages.value[1]!.url).toBe(second.url);
  });

  it("silently ignores encrypted images whose decrypted URL is not ready", () => {
    const error = console.error;
    const consoleError = mock(() => {});
    console.error = consoleError;
    try {
      const file = pendingEncryptedImage("https://example.com/encrypted.png.enc", "encrypted.png");
      const attachments = useMessageAttachments(ref(messageWithFiles([file])));

      attachments.openLightbox(file);

      expect(attachments.lightboxOpen.value).toBe(false);
      expect(consoleError).not.toHaveBeenCalled();
    } finally {
      console.error = error;
    }
  });

  it("excludes unresolved encrypted images without drifting the clicked index", () => {
    const plain = imageFile("https://example.com/plain.png", "plain.png");
    const encrypted = pendingEncryptedImage("https://example.com/encrypted.png.enc", "encrypted.png");
    const attachments = useMessageAttachments(ref(messageWithFiles([plain, encrypted])));

    attachments.openLightbox(plain);

    expect(attachments.lightboxOpen.value).toBe(true);
    expect(attachments.lightboxIndex.value).toBe(0);
    expect(attachments.lightboxImages.value).toHaveLength(1);
    expect(attachments.lightboxImages.value[0]!.url).toBe(plain.url);
  });
});
