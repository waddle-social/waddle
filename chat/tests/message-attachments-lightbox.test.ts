import { afterEach, describe, expect, it, mock } from "bun:test";
import { nextTick, ref } from "vue";
import { useMessageAttachments } from "@/channels/message-attachments";
import type { TimelineMessage, TimelineSharedFile } from "@/lib/chat-ui";

type MessageRef = Parameters<typeof useMessageAttachments>[0];

const pendingCleanups: Array<() => void> = [];

afterEach(() => {
  for (const cleanup of pendingCleanups) cleanup();
  pendingCleanups.length = 0;
});

function messageAttachments(message: MessageRef) {
  const attachments = useMessageAttachments(message);
  pendingCleanups.push(attachments.cleanup);
  return attachments;
}

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

function messageWithBody(body: string): TimelineMessage {
  return {
    id: "msg-1",
    author: "alice",
    body,
    createdAt: "2026-05-12T09:53:00Z",
    isSelf: false,
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
  it("opens an inline GIF body URL in a one-image lightbox", () => {
    const gifUrl = "https://media.giphy.com/media/example/giphy.gif";
    const attachments = messageAttachments(ref(messageWithBody(` ${gifUrl} `)));

    attachments.openGifLightbox();

    expect(attachments.lightboxOpen.value).toBe(true);
    expect(attachments.lightboxIndex.value).toBe(0);
    expect(attachments.lightboxImages.value).toHaveLength(1);
    expect(attachments.lightboxImages.value[0]!.url).toBe(gifUrl);
  });

  it("ignores inline GIF lightbox opens when the body is not an image URL", () => {
    const attachments = messageAttachments(ref(messageWithBody("hello")));

    attachments.openGifLightbox();

    expect(attachments.lightboxOpen.value).toBe(false);
    expect(attachments.lightboxImages.value).toEqual([]);
  });

  it("opens a plain image gallery at the clicked image", () => {
    const first = imageFile("https://example.com/a.png", "a.png");
    const second = imageFile("https://example.com/b.png", "b.png");
    const attachments = messageAttachments(ref(messageWithFiles([first, second])));

    attachments.openLightbox(second);

    expect(attachments.lightboxOpen.value).toBe(true);
    expect(attachments.lightboxIndex.value).toBe(1);
    expect(attachments.lightboxImages.value).toHaveLength(2);
    expect(attachments.lightboxImages.value[1]!.url).toBe(second.url);
  });

  it("opens the clicked duplicate-url image instead of the first matching URL", () => {
    const first = imageFile("https://example.com/same.png", "first.png");
    const second = imageFile("https://example.com/same.png", "second.png");
    const attachments = messageAttachments(ref(messageWithFiles([first, second])));

    attachments.openLightbox(second);

    expect(attachments.lightboxOpen.value).toBe(true);
    expect(attachments.lightboxIndex.value).toBe(1);
    expect(attachments.lightboxImages.value[attachments.lightboxIndex.value]!.name).toBe("second.png");
  });

  it("closes the lightbox when the selected duplicate-url image is removed", async () => {
    const first = imageFile("https://example.com/same.png", "first.png");
    const second = imageFile("https://example.com/same.png", "second.png");
    const message = ref(messageWithFiles([first, second]));
    const attachments = messageAttachments(message);

    attachments.openLightbox(second);
    message.value = messageWithFiles([first]);
    await nextTick();

    expect(attachments.lightboxOpen.value).toBe(false);
    expect(attachments.lightboxIndex.value).toBe(0);
  });

  it("closes instead of falling back to an indistinguishable duplicate", async () => {
    const first = imageFile("https://example.com/same.png", "same.png");
    const second = imageFile("https://example.com/same.png", "same.png");
    const message = ref(messageWithFiles([first, second]));
    const attachments = messageAttachments(message);

    attachments.openLightbox(second);
    message.value = messageWithFiles([first]);
    await nextTick();

    expect(attachments.lightboxOpen.value).toBe(false);
  });

  it("keeps the selected duplicate-url image stable when earlier duplicates change", async () => {
    const inserted = imageFile("https://example.com/same.png", "inserted.png");
    const first = imageFile("https://example.com/same.png", "first.png");
    const second = imageFile("https://example.com/same.png", "second.png");
    const message = ref(messageWithFiles([first, second]));
    const attachments = messageAttachments(message);

    attachments.openLightbox(second);
    message.value = messageWithFiles([inserted, first, second]);
    await nextTick();

    expect(attachments.lightboxOpen.value).toBe(true);
    expect(attachments.lightboxImages.value[attachments.lightboxIndex.value]!.name).toBe("second.png");

    message.value = messageWithFiles([second]);
    await nextTick();

    expect(attachments.lightboxOpen.value).toBe(true);
    expect(attachments.lightboxIndex.value).toBe(0);
    expect(attachments.lightboxImages.value[0]!.name).toBe("second.png");
  });

  it("silently ignores encrypted images whose decrypted URL is not ready", () => {
    const error = console.error;
    const consoleError = mock(() => {});
    console.error = consoleError;
    try {
      const file = pendingEncryptedImage("https://example.com/encrypted.png.enc", "encrypted.png");
      const attachments = messageAttachments(ref(messageWithFiles([file])));

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
    const attachments = messageAttachments(ref(messageWithFiles([plain, encrypted])));

    attachments.openLightbox(plain);

    expect(attachments.lightboxOpen.value).toBe(true);
    expect(attachments.lightboxIndex.value).toBe(0);
    expect(attachments.lightboxImages.value).toHaveLength(1);
    expect(attachments.lightboxImages.value[0]!.url).toBe(plain.url);
  });

  it("keeps the selected image stable when earlier images become resolvable", async () => {
    const first = imageFile("https://example.com/a.png", "a.png");
    const pending = pendingEncryptedImage("https://example.com/b.png.enc", "b.png");
    const third = imageFile("https://example.com/c.png", "c.png");
    const message = ref(messageWithFiles([first, pending, third]));
    const attachments = messageAttachments(message);

    attachments.openLightbox(third);

    expect(attachments.lightboxOpen.value).toBe(true);
    expect(attachments.lightboxIndex.value).toBe(1);
    expect(attachments.lightboxImages.value[attachments.lightboxIndex.value]!.url).toBe(third.url);

    message.value = messageWithFiles([
      first,
      imageFile("https://example.com/b.png", "b.png"),
      third,
    ]);
    await nextTick();

    expect(attachments.lightboxOpen.value).toBe(true);
    expect(attachments.lightboxIndex.value).toBe(2);
    expect(attachments.lightboxImages.value[attachments.lightboxIndex.value]!.url).toBe(third.url);
  });

  it("keeps a uniquely identifiable selection stable when shared file objects are recreated", async () => {
    const message = ref(messageWithFiles([
      imageFile("https://example.com/a.png", "a.png"),
      pendingEncryptedImage("https://example.com/b.png.enc", "b.png"),
      imageFile("https://example.com/c.png", "c.png"),
    ]));
    const attachments = messageAttachments(message);

    attachments.openLightbox(message.value.sharedFiles![2]!);
    message.value = messageWithFiles([
      imageFile("https://example.com/a.png", "a.png"),
      imageFile("https://example.com/b.png", "b.png"),
      imageFile("https://example.com/c.png", "c.png"),
    ]);
    await nextTick();

    expect(attachments.lightboxOpen.value).toBe(true);
    expect(attachments.lightboxIndex.value).toBe(2);
    expect(attachments.lightboxImages.value[attachments.lightboxIndex.value]!.url).toBe("https://example.com/c.png");
  });

  it("closes the lightbox when the selected image is removed", async () => {
    const first = imageFile("https://example.com/a.png", "a.png");
    const second = imageFile("https://example.com/b.png", "b.png");
    const message = ref(messageWithFiles([first, second]));
    const attachments = messageAttachments(message);

    attachments.openLightbox(second);
    message.value = messageWithFiles([first]);
    await nextTick();

    expect(attachments.lightboxOpen.value).toBe(false);
    expect(attachments.lightboxIndex.value).toBe(0);
  });
});
