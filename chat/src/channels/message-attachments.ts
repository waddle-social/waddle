import { computed, onBeforeUnmount, ref, watch, type Ref } from "vue";
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
    if (sharedFiles.value.length === 0) return isGif.value ? "" : body;
    const matchesAttachment = sharedFiles.value.some((f) => f.url === body.trim());
    return matchesAttachment ? "" : body;
  });

  const lightboxOpen = ref(false);
  const lightboxIndex = ref(0);
  const decryptedAttachmentUrls = ref<Record<string, string>>({});
  const decryptedAttachmentErrors = ref<Record<string, string>>({});
  const decryptingAttachmentKeys = ref<Record<string, boolean>>({});

  function attachmentKey(file: TimelineSharedFile): string {
    return hasEncryptedAttachmentMetadata(file) ? encryptedAttachmentKey(file) : file.url;
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

  watch(
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

  const lightboxImages = computed(() =>
    imageAttachments.value.flatMap((f) => {
      const resolvedUrl = resolvedAttachmentUrl(f);
      if (!resolvedUrl) return [];
      const img: { url: string; name?: string; width?: number; height?: number } = { url: resolvedUrl };
      if (f.name) img.name = f.name;
      if (f.width) img.width = f.width;
      if (f.height) img.height = f.height;
      return [img];
    }),
  );

  function openLightbox(file: TimelineSharedFile) {
    const resolvedUrl = resolvedAttachmentUrl(file);
    if (!resolvedUrl) return;
    const index = lightboxImages.value.findIndex((image) => image.url === resolvedUrl);
    if (index < 0) return;
    lightboxIndex.value = index;
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
    if (typeof URL === "undefined") return;
    for (const key of Object.keys(decryptedAttachmentUrls.value)) revokeAttachmentUrl(key);
  }

  onBeforeUnmount(cleanup);

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
