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

type ReadonlyRef<T> = Readonly<Ref<T>>;
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
  const isGif = computed(() => sharedFiles.value.length === 0 && isImageUrl(message.value.body));

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
    if (sharedFiles.value.length === 0) return isGif.value ? "" : body;
    const matchesAttachment = sharedFiles.value.some((f) => f.url === body.trim());
    return matchesAttachment ? "" : body;
  });

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

  function attachmentFingerprint(file: TimelineSharedFile): string {
    return [
      attachmentKey(file),
      file.name ?? "",
      file.width ?? "",
      file.height ?? "",
    ].join("\u0000");
  }

  function attachmentInstanceId(file: TimelineSharedFile): string {
    const identity = toRaw(file);
    const existing = attachmentInstanceIds.get(identity);
    if (existing) return existing;
    const id = `${attachmentKey(file)}:${nextAttachmentInstanceId}`;
    nextAttachmentInstanceId += 1;
    attachmentInstanceIds.set(identity, id);
    return id;
  }

  function attachmentRenderKey(file: TimelineSharedFile): string {
    const target = toRaw(file);
    const occurrenceCounts = new Map<string, number>();
    for (const attachment of imageAttachments.value) {
      const fingerprint = attachmentFingerprint(attachment);
      const occurrence = occurrenceCounts.get(fingerprint) ?? 0;
      occurrenceCounts.set(fingerprint, occurrence + 1);
      if (toRaw(attachment) === target) return `${fingerprint}:${occurrence}`;
    }
    return attachmentInstanceId(file);
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

  function resolvedAttachmentUrl(file: TimelineSharedFile): string | null {
    if (!hasEncryptedAttachmentMetadata(file)) return file.url;
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
    if (!hasEncryptedAttachmentMetadata(file)) return file.url;
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
      const stillVisible = imageAttachments.value.some((attachment) => attachmentKey(attachment) === key);
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

  const stopPreviewableAttachmentsWatch = watch(
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

  const lightboxImages = computed(() => {
    return imageAttachments.value.flatMap((f) => {
      const resolvedUrl = resolvedAttachmentUrl(f);
      if (!resolvedUrl) return [];
      const img: LightboxImage = {
        sourceId: attachmentInstanceId(f),
        fingerprint: attachmentFingerprint(f),
        url: resolvedUrl,
      };
      if (f.name) img.name = f.name;
      if (f.width) img.width = f.width;
      if (f.height) img.height = f.height;
      return [img];
    });
  });

  function syncLightboxIndex(images = lightboxImages.value) {
    const selected = lightboxSelectedImage.value;
    if (!lightboxOpen.value || !selected) return;
    let index = images.findIndex((image) => image.sourceId === selected.sourceId);
    if (index < 0 && selected.fingerprintUnique) {
      const matchingIndexes = images.flatMap((image, imageIndex) =>
        image.fingerprint === selected.fingerprint ? [imageIndex] : [],
      );
      if (matchingIndexes.length === 1) index = matchingIndexes[0]!;
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
      fingerprintUnique: images.filter((candidate) => candidate.fingerprint === image.fingerprint).length === 1,
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
    stopPreviewableAttachmentsWatch();
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
    isGif,
    lightboxOpen,
    lightboxIndex,
    lightboxImages,
    attachmentKey,
    attachmentRenderKey,
    resolvedAttachmentUrl,
    attachmentError,
    isDecryptingAttachment,
    openLightbox,
    downloadAttachment,
    cleanup,
  };
}

export function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
