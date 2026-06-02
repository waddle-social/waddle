import type { ComposerLinkPreviewSendPayload } from "@/lib/link-preview-composer";
import type { RichMessage } from "@/lib/rich-message/types";

interface PreparedComposerSend extends RichMessage {
  files?: Array<File | Blob>;
  linkPreview?: ComposerLinkPreviewSendPayload;
}

export async function prepareComposerSendEvent({
  serialized,
  files,
  linkPreviewForBody,
}: {
  serialized: RichMessage;
  files: Array<File | Blob>;
  linkPreviewForBody: (body: string) => Promise<ComposerLinkPreviewSendPayload | undefined>;
}): Promise<PreparedComposerSend> {
  const linkPreview = await linkPreviewForBody(serialized.body);
  return {
    body: serialized.body,
    markup: serialized.markup,
    references: serialized.references,
    files: files.length > 0 ? files : undefined,
    linkPreview,
  };
}
