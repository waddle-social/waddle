import { computed, getCurrentScope, onScopeDispose, ref, toRaw, watch, type Ref, type WatchStopHandle } from "vue";
import {
  isAudioFile,
  isImageFile,
  isImageUrl,
  isPdfFile,
  isVideoFile,
  type TimelineMessage,
  type TimelineSharedFile,
} from "@/lib/chat-ui";
import {
  decryptEncryptedAttachment,
  encryptedAttachmentKey,
  hasEncryptedAttachmentMetadata,
} from "@/lib/xmpp/encrypted-attachments";
import { safeAttachmentUrl } from "@/lib/attachment-url";

type ReadonlyRef<T> = Readonly<Ref<T>>;
// Internal gallery entry metadata. `sourceId` tracks the same attachment object
// across reactive recomputes, while `fingerprint` lets a uniquely identifiable
// replacement object keep the current lightbox selection stable.
type LightboxImage = {
  sourceId: string;
  fingerprint: string;
  url: string;
  name?: string;
  width?: number;
  height?: number;
};
type SelectedLightboxImage = Pick<LightboxImage, "sourceId" | "fingerprint"> & {
  fingerprintUnique: boolean;
};

export function useMessageAttachments(message: ReadonlyRef<TimelineMessage>) {
  const sharedFiles = computed(() => message.value.sharedFiles ?? []);
  const isInlineImageBodyUrl = computed(() => sharedFiles.value.length === 0 && isImageUrl(message.value.body));

  const imageAttachments = computed(() =>
    sharedFiles.value.filter((f) =>
      f.disposition === "inline" && isImageFile(f.mediaType, f.url),
    ),
  );
  const videoAttachments = computed(() =>
    sharedFiles.value.filter((f) =>
      f.disposition === "inline"
      && !isImageFile(f.mediaType, f.url)
      && isVideoFile(f.mediaType, f.url),
    ),
  );
  const audioAttachments = computed(() =>
    sharedFiles.value.filter((f) =>
      f.disposition === "inline"
      && !isImageFile(f.mediaType, f.url)
      && !isVideoFile(f.mediaType, f.url)
      && isAudioFile(f.mediaType, f.url),
    ),
  );
  const pdfAttachments = computed(() =>
    sharedFiles.value.filter((f) =>
      f.disposition === "inline"
      && !isImageFile(f.mediaType, f.url)
      && !isVideoFile(f.mediaType, f.url)
      && !isAudioFile(f.mediaType, f.url)
      && isPdfFile(f.mediaType, f.url),
    ),
  );
  const downloadableAttachments = computed(() =>
    sharedFiles.value.filter((f) =>
      f.disposition !== "inline"
      || (
        !isImageFile(f.mediaType, f.url)
        && !isVideoFile(f.mediaType, f.url)
        && !isAudioFile(f.mediaType, f.url)
        && !isPdfFile(f.mediaType, f.url)
      ),
    ),
  );
  const displayBody = computed(() => {
    const body = message.value.body;
    if (!body) return "";
    // XEP-0449 stickers carry a <body> alt-text element; suppress the text
    // bubble so only the sticker image renders (the template chains
    // v-else-if="isSticker" after v-if="displayBody").
    if (message.value.isSticker && imageAttachments.value.length > 0) return "";
    if (sharedFiles.value.length === 0) return isInlineImageBodyUrl.value ? "" : body;
    const matchesAttachment = sharedFiles.value.some((f) => f.url === body.trim());
    return matchesAttachment ? "" : body;
  });
  const inlineImageUrl = computed(() => (isInlineImageBodyUrl.value ? message.value.body.trim() : null));

  // Canonical per-message image lightbox state. Components rendering a
  // message body should use these refs and openLightbox(file) together so the
  // gallery is derived from the same resolved attachment URLs. Calling
  // openLightbox for an encrypted image whose decrypted blob URL is not ready
  // is a silent no-op; unresolved entries are excluded from the gallery.
  const lightboxOpenValue = ref(false);
  const lightboxOpen = computed({
    get: () => lightboxOpenValue.value,
    set: (open) => {
      lightboxOpenValue.value = open;
      if (open) startLightboxSelectionTracking();
      else stopLightboxSelectionTracking();
    },
  });
  // Meaningful only while lightboxOpen is true; the lightbox ignores it when
  // closed and openLightbox always seeds it before opening.
  const lightboxIndex = ref(0);
  const lightboxSelectedImage = ref<SelectedLightboxImage | null>(null);
  const decryptedAttachmentUrls = ref<Record<string, string>>({});
  const decryptedAttachmentErrors = ref<Record<string, string>>({});
  const decryptingAttachmentKeys = ref<Record<string, boolean>>({});
  const attachmentInstanceIds = new WeakMap<TimelineSharedFile, string>();
  let nextAttachmentInstanceId = 0;
  let stopLightboxImagesWatch: WatchStopHandle | null = null;
  let stopLightboxIndexWatch: WatchStopHandle | null = null;
  let disposed = false;

  function attachmentKey(file: TimelineSharedFile): string {
    return hasEncryptedAttachmentMetadata(file) ? encryptedAttachmentKey(file) : file.url;
  }

  // JSON tuple encoding keeps optional fields unambiguous without relying on a
  // separator character that could also appear in user-provided names.
  function attachmentFingerprint(file: TimelineSharedFile): string {
    return JSON.stringify([
      attachmentKey(file),
      file.name ?? "",
      file.width ?? null,
      file.height ?? null,
    ]);
  }

  function attachmentInstanceId(file: TimelineSharedFile): string {
    const identity = toRaw(file);
    const existing = attachmentInstanceIds.get(identity);
    if (existing) return existing;
    // The monotonic suffix only needs to be unique for this composable
    // instance; combining it with the attachment key keeps IDs readable.
    const id = `${attachmentKey(file)}:${nextAttachmentInstanceId}`;
    nextAttachmentInstanceId += 1;
    attachmentInstanceIds.set(identity, id);
    return id;
  }

  function lightboxImage(sourceId: string, fingerprint: string, url: string, file?: TimelineSharedFile): LightboxImage {
    const img: LightboxImage = { sourceId, fingerprint, url };
    if (file?.name) img.name = file.name;
    if (file?.width) img.width = file.width;
    if (file?.height) img.height = file.height;
    return img;
  }

  const attachmentRenderKeys = computed(() => {
    const occurrenceCounts = new Map<string, number>();
    const renderKeys = new WeakMap<TimelineSharedFile, string>();
    for (const attachment of imageAttachments.value) {
      const fingerprint = attachmentFingerprint(attachment);
      const occurrence = occurrenceCounts.get(fingerprint) ?? 0;
      occurrenceCounts.set(fingerprint, occurrence + 1);
      renderKeys.set(toRaw(attachment), `${fingerprint}:${occurrence}`);
    }
    return renderKeys;
  });

  function attachmentRenderKey(file: TimelineSharedFile): string {
    return attachmentRenderKeys.value.get(toRaw(file)) ?? attachmentInstanceId(file);
  }

  function setAttachmentFlag(key: string, value: boolean) {
    const next = { ...decryptingAttachmentKeys.value };
    if (value) next[key] = true;
    else delete next[key];
    decryptingAttachmentKeys.value = next;
  }

  function setAttachmentError(key: string, value?: string) {
    const next = { ...decryptedAttachmentErrors.value };
    if (value) next[key] = value;
    else delete next[key];
    decryptedAttachmentErrors.value = next;
  }

  function revokeAttachmentUrl(key: string) {
    const current = decryptedAttachmentUrls.value[key];
    if (!current) return;
    URL.revokeObjectURL(current);
    const next = { ...decryptedAttachmentUrls.value };
    delete next[key];
    decryptedAttachmentUrls.value = next;
  }

  // Single choke point for every render sink (img/video/audio/iframe/anchor/
  // lightbox): wire-provided URLs pass the scheme allowlist or resolve to null
  // so the attachment renders inert. Decrypted attachments resolve to a
  // locally-minted blob: URL, which the allowlist keeps working.
  function resolvedAttachmentUrl(file: TimelineSharedFile): string | null {
    if (!hasEncryptedAttachmentMetadata(file)) return safeAttachmentUrl(file.url);
    return decryptedAttachmentUrls.value[attachmentKey(file)] ?? null;
  }

  function attachmentError(file: TimelineSharedFile): string | null {
    return decryptedAttachmentErrors.value[attachmentKey(file)] ?? null;
  }

  function isDecryptingAttachment(file: TimelineSharedFile): boolean {
    return !!decryptingAttachmentKeys.value[attachmentKey(file)];
  }

  async function ensureAttachmentReady(file: TimelineSharedFile, persist = false): Promise<string | null> {
    if (disposed) return null;
    if (!hasEncryptedAttachmentMetadata(file)) return safeAttachmentUrl(file.url);
    if (typeof window === "undefined" || typeof URL === "undefined") return null;
    const key = attachmentKey(file);
    const existing = decryptedAttachmentUrls.value[key];
    if (existing) return existing;
    if (decryptingAttachmentKeys.value[key]) return null;

    setAttachmentFlag(key, true);
    setAttachmentError(key);
    try {
      const blob = await decryptEncryptedAttachment(file);
      const objectUrl = URL.createObjectURL(blob);
      if (disposed) {
        URL.revokeObjectURL(objectUrl);
        return null;
      }
      if (!persist) return objectUrl;
      const stillVisible = previewableAttachments.value.some((attachment) => attachmentKey(attachment) === key);
      if (!stillVisible) {
        URL.revokeObjectURL(objectUrl);
        return null;
      }
      decryptedAttachmentUrls.value = { ...decryptedAttachmentUrls.value, [key]: objectUrl };
      return objectUrl;
    } catch (error) {
      setAttachmentError(key, error instanceof Error ? error.message : "Couldn't decrypt attachment.");
      return null;
    } finally {
      setAttachmentFlag(key, false);
    }
  }

  const previewableAttachments = computed(() => [
    ...imageAttachments.value,
    ...videoAttachments.value,
    ...audioAttachments.value,
    ...pdfAttachments.value,
  ]);

  const stopPreviewableAttachmentsWatchHandle = watch(
    previewableAttachments,
    (attachments) => {
      if (typeof window === "undefined") return;
      const activeKeys = new Set(attachments.map((attachment) => attachmentKey(attachment)));
      for (const key of Object.keys(decryptedAttachmentUrls.value)) {
        if (!activeKeys.has(key)) revokeAttachmentUrl(key);
      }
      for (const attachment of attachments) {
        if (hasEncryptedAttachmentMetadata(attachment)) void ensureAttachmentReady(attachment, true);
      }
    },
    { immediate: true },
  );

  const attachmentLightboxImages = computed(() => {
    return imageAttachments.value.flatMap((f) => {
      const resolvedUrl = resolvedAttachmentUrl(f);
      if (!resolvedUrl) return [];
      return [lightboxImage(attachmentInstanceId(f), attachmentFingerprint(f), resolvedUrl, f)];
    });
  });

  const lightboxImages = computed(() => {
    const imageUrl = inlineImageUrl.value;
    const images = attachmentLightboxImages.value;
    if (!imageUrl) return images;
    return [
      lightboxImage(
        `inline-body-image:${imageUrl}`,
        JSON.stringify(["inline-body-image", imageUrl]),
        imageUrl,
      ),
      ...images,
    ];
  });

  // Returns an index only when the fingerprint appears exactly once; ambiguous
  // duplicates and missing entries both return -1 so the lightbox can close
  // rather than jump to a different attachment.
  function uniqueFingerprintIndex(images: LightboxImage[], fingerprint: string): number {
    let matchedIndex = -1;
    for (const [index, image] of images.entries()) {
      if (image.fingerprint !== fingerprint) continue;
      if (matchedIndex >= 0) return -1;
      matchedIndex = index;
    }
    return matchedIndex;
  }

  function syncLightboxIndex(images = lightboxImages.value) {
    const selected = lightboxSelectedImage.value;
    if (!lightboxOpen.value || !selected) return;
    let index = images.findIndex((image) => image.sourceId === selected.sourceId);
    if (index < 0 && selected.fingerprintUnique) {
      index = uniqueFingerprintIndex(images, selected.fingerprint);
    }
    if (index < 0) {
      lightboxOpen.value = false;
      lightboxSelectedImage.value = null;
      lightboxIndex.value = 0;
      return;
    }
    lightboxIndex.value = index;
    lightboxSelectedImage.value = selectedLightboxImageFor(images[index]!, images);
  }

  function selectedLightboxImageFor(image: LightboxImage, images = lightboxImages.value): SelectedLightboxImage {
    return {
      sourceId: image.sourceId,
      fingerprint: image.fingerprint,
      fingerprintUnique: uniqueFingerprintIndex(images, image.fingerprint) >= 0,
    };
  }

  function syncSelectedLightboxImage(index: number) {
    if (!lightboxOpen.value) return;
    const selected = lightboxImages.value[index];
    lightboxSelectedImage.value = selected
      ? selectedLightboxImageFor(selected)
      : null;
  }

  function startLightboxSelectionTracking() {
    if (stopLightboxImagesWatch) return;
    stopLightboxImagesWatch = watch(lightboxImages, syncLightboxIndex, { flush: "sync" });
    stopLightboxIndexWatch = watch(lightboxIndex, syncSelectedLightboxImage, { flush: "sync" });
  }

  function stopLightboxSelectionTracking() {
    stopLightboxImagesWatch?.();
    stopLightboxIndexWatch?.();
    stopLightboxImagesWatch = null;
    stopLightboxIndexWatch = null;
    lightboxSelectedImage.value = null;
  }

  function openLightbox(file: TimelineSharedFile) {
    const resolvedUrl = resolvedAttachmentUrl(file);
    if (!resolvedUrl) return;
    const sourceId = attachmentInstanceId(file);
    const index = lightboxImages.value.findIndex((image) => image.sourceId === sourceId);
    if (index < 0) return;
    const selected = lightboxImages.value[index]!;
    lightboxSelectedImage.value = selectedLightboxImageFor(selected);
    lightboxIndex.value = index;
    startLightboxSelectionTracking();
    lightboxOpen.value = true;
  }

  function openInlineImageLightbox() {
    const image = lightboxImages.value[0];
    if (!inlineImageUrl.value || !image) return;
    lightboxSelectedImage.value = selectedLightboxImageFor(image);
    lightboxIndex.value = 0;
    startLightboxSelectionTracking();
    lightboxOpen.value = true;
  }

  async function downloadAttachment(file: TimelineSharedFile) {
    const downloadUrl = await ensureAttachmentReady(file);
    if (!downloadUrl || typeof document === "undefined") return;
    const link = document.createElement("a");
    link.href = downloadUrl;
    link.download = file.name ?? "attachment";
    link.rel = "noopener noreferrer";
    document.body.appendChild(link);
    link.click();
    link.remove();
    if (hasEncryptedAttachmentMetadata(file) && !decryptedAttachmentUrls.value[attachmentKey(file)]) {
      setTimeout(() => URL.revokeObjectURL(downloadUrl), 60_000);
    }
  }

  function cleanup() {
    disposed = true;
    stopPreviewableAttachmentsWatchHandle();
    stopLightboxSelectionTracking();
    if (typeof URL === "undefined") return;
    for (const key of Object.keys(decryptedAttachmentUrls.value)) revokeAttachmentUrl(key);
  }

  // Component callers get automatic cleanup through Vue's active effect scope.
  // Component-free tests call the returned cleanup() explicitly when needed.
  if (getCurrentScope()) onScopeDispose(cleanup);

  return {
    sharedFiles,
    imageAttachments,
    videoAttachments,
    audioAttachments,
    pdfAttachments,
    downloadableAttachments,
    displayBody,
    isInlineImageBodyUrl,
    lightboxOpen,
    lightboxIndex,
    lightboxImages,
    attachmentKey,
    attachmentRenderKey,
    resolvedAttachmentUrl,
    attachmentError,
    isDecryptingAttachment,
    openLightbox,
    openInlineImageLightbox,
    downloadAttachment,
    cleanup,
  };
}

export function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
