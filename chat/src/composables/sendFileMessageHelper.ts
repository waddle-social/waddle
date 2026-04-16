import type { Ref } from "vue";
import type { TimelineMessage } from "@/lib/chat-ui";
import type { UploadProgress } from "@/lib/xmpp/file-upload";
import { MAX_IMAGE_UPLOAD_BYTES } from "@/lib/xmpp/file-upload";

export async function executeSendFileMessage(opts: {
  file: File | Blob;
  username: string;
  uploadProgress: Ref<{ uploading: boolean; progress: number; filename: string }>;
  messages: Ref<TimelineMessage[]>;
  actionError: Ref<string>;
  clearActionError: () => void;
  scrollToBottom: () => Promise<void>;
  normalizeError: (e: unknown) => string;
  doUpload: (
    file: File | Blob,
    onProgress: (p: UploadProgress) => void,
  ) => Promise<{ msgId: string; fileUrl: string } | null>;
}): Promise<void> {
  const { file, username, uploadProgress, messages, actionError, clearActionError, scrollToBottom, normalizeError, doUpload } = opts;

  if (file.size > MAX_IMAGE_UPLOAD_BYTES) {
    actionError.value = `File too large (${(file.size / 1024 / 1024).toFixed(1)} MB). Maximum upload size is 10 MB.`;
    return;
  }

  const filename = file instanceof File ? file.name : `image-${Date.now()}.png`;
  uploadProgress.value = { uploading: true, progress: 0, filename };
  clearActionError();

  try {
    const result = await doUpload(file, (p) => {
      uploadProgress.value = {
        uploading: true,
        progress: p.total > 0 ? p.loaded / p.total : 0,
        filename,
      };
    });

    if (result) {
      messages.value = [
        ...messages.value,
        {
          id: result.msgId,
          author: username,
          body: result.fileUrl,
          createdAt: new Date().toISOString(),
          isSelf: true,
          deliveryStatus: "sending",
        },
      ];
      void scrollToBottom();
    }
  } catch (e) {
    actionError.value = normalizeError(e);
  } finally {
    uploadProgress.value = { uploading: false, progress: 0, filename: "" };
  }
}
