import { isAudioFile, isImageFile, isPdfFile, isVideoFile } from "@/lib/chat-ui";

export type AttachmentPreviewKind = "image" | "video" | "audio" | "pdf" | "file";

export interface PendingAttachment {
  id: string;
  file: File | Blob;
  previewUrl: string;
  name: string;
  mediaType: string;
  size: number;
  previewKind: AttachmentPreviewKind;
}

export function attachmentName(file: File | Blob): string {
  return file instanceof File && file.name
    ? file.name
    : `attachment-${Date.now()}.bin`;
}

export function attachmentPreviewKind(mediaType?: string, name?: string): AttachmentPreviewKind {
  const candidate = name ?? "";
  if (isImageFile(mediaType, candidate)) return "image";
  if (isVideoFile(mediaType, candidate)) return "video";
  if (isAudioFile(mediaType, candidate)) return "audio";
  if (isPdfFile(mediaType, candidate)) return "pdf";
  return "file";
}

export function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
