/** XEP-0363: HTTP File Upload for sharing images and files in chat. */
import type { WaddleClient } from "@waddle/xmpp-client-wasm";
import { markSensitiveUrlForTelemetry, reportError } from "@/lib/telemetry";
import type { WasmUploadSlot } from "./wasm-types";

export const MAX_FILE_UPLOAD_BYTES = 10 * 1024 * 1024;

interface UploadResult {
  getUrl: string;
  filename: string;
  contentType: string;
  size: number;
}

export interface UploadProgress {
  loaded: number;
  total: number;
}

interface SlotInfo {
  putUrl: string;
  putHeaders: Array<[string, string]>;
  getUrl: string;
}

export async function discoverUploadService(xmpp: WaddleClient): Promise<string | null> {
  return await xmpp.discover_upload_service();
}

export async function uploadFile(
  xmpp: WaddleClient,
  file: File | Blob,
  uploadDomain: string,
  onProgress?: (progress: UploadProgress) => void,
): Promise<UploadResult> {
  const filename = file instanceof File ? file.name : `attachment-${Date.now()}.bin`;
  const contentType = file.type || "application/octet-stream";
  const size = file.size;
  if (size === 0) throw new Error("Cannot upload an empty file");

  const slot = parseSlotResponse(
    await xmpp.request_upload_slot(uploadDomain, filename, BigInt(size), contentType) as WasmUploadSlot,
  );
  await uploadToSlot(file, slot.putUrl, slot.putHeaders, contentType, onProgress);
  return { getUrl: slot.getUrl, filename, contentType, size };
}

export function extractDroppedFiles(event: DragEvent): File[] {
  return Array.from(event.dataTransfer?.files ?? []);
}

function parseSlotResponse(response: WasmUploadSlot): SlotInfo {
  if (!response.put_url || !response.get_url) throw new Error("Upload slot missing PUT or GET URL");
  return {
    putUrl: response.put_url,
    getUrl: response.get_url,
    putHeaders: (response.put_headers ?? [])
      .filter((header) => header.name && header.value)
      .map((header) => [header.name, header.value]),
  };
}

async function uploadToSlot(
  file: File | Blob,
  putUrl: string,
  headers: Array<[string, string]>,
  contentType: string,
  onProgress?: (progress: UploadProgress) => void,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    markSensitiveUrlForTelemetry(putUrl);
    xhr.open("PUT", putUrl);
    xhr.setRequestHeader("Content-Type", contentType);
    for (const [name, value] of headers) xhr.setRequestHeader(name, value);
    if (onProgress) {
      xhr.upload.onprogress = (event) => {
        if (event.lengthComputable) onProgress({ loaded: event.loaded, total: event.total });
      };
    }
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        resolve();
        return;
      }
      const error = new Error(`Upload failed with HTTP ${xhr.status}`);
      reportError({
        kind: "upload",
        operation: "xep-0363-put",
        failure: "http-status",
        status: xhr.status,
      });
      reject(error);
    };
    xhr.onerror = () => {
      const error = new Error("Upload failed: network error");
      reportError({
        kind: "upload",
        operation: "xep-0363-put",
        failure: "network-error",
      });
      reject(error);
    };
    xhr.send(file);
  });
}
