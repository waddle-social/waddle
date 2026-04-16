/** XEP-0363: HTTP File Upload for sharing images and files in chat. */
import type { Agent } from "stanza";

export const MAX_IMAGE_UPLOAD_BYTES = 10 * 1024 * 1024; // 10 MB

export interface UploadResult {
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

const UPLOAD_FEATURE = "urn:xmpp:http:upload:0";

function hasUploadFeature(info: { features?: any[] }): boolean {
  return (
    info.features?.some(
      (f: any) => f === UPLOAD_FEATURE || f.var === UPLOAD_FEATURE,
    ) ?? false
  );
}

/**
 * Discover the HTTP Upload service JID via disco#items.
 * Looks for a service advertising urn:xmpp:http:upload:0.
 * Falls back to querying upload.{domain} directly if standard discovery fails.
 * Returns null if not found.
 */
export async function discoverUploadService(xmpp: Agent, domain: string): Promise<string | null> {
  // Primary path: standard XEP-0030 disco#items discovery
  try {
    const items = await xmpp.getDiscoItems(domain);
    const discoItems = items.items ?? [];

    if (discoItems.length === 0) {
      console.warn("[file-upload] disco#items to", domain, "returned zero items");
    }

    const results = await Promise.allSettled(
      discoItems.map(async (item) => {
        const info = await xmpp.getDiscoInfo(item.jid);
        return hasUploadFeature(info) ? item.jid : null;
      }),
    );

    for (const r of results) {
      if (r.status === "fulfilled" && r.value) return r.value;
    }

    const rejected = results.filter((r) => r.status === "rejected");
    if (rejected.length > 0) {
      console.warn(
        "[file-upload] disco#info failed for",
        rejected.length,
        "of",
        discoItems.length,
        "items:",
        rejected.map((r) => (r as PromiseRejectedResult).reason),
      );
    }
  } catch (err) {
    console.warn("[file-upload] disco#items discovery failed:", err);
  }

  // Fallback: directly query upload.{domain} (known server convention)
  const fallbackJid = `upload.${domain}`;
  try {
    const info = await xmpp.getDiscoInfo(fallbackJid);
    if (hasUploadFeature(info)) return fallbackJid;
    console.warn("[file-upload] fallback", fallbackJid, "lacks upload feature");
  } catch (err) {
    console.warn("[file-upload] fallback", fallbackJid, "failed:", err);
  }

  return null;
}

/**
 * Upload a file via XEP-0363 HTTP File Upload.
 *
 * 1. Requests an upload slot via XMPP IQ
 * 2. Uploads the file via HTTP PUT
 * 3. Returns the download URL
 */
export async function uploadFile(
  xmpp: Agent,
  file: File | Blob,
  uploadDomain: string,
  onProgress?: (progress: UploadProgress) => void,
): Promise<UploadResult> {
  const filename = file instanceof File ? file.name : `image-${Date.now()}.png`;
  const contentType = file.type || "application/octet-stream";
  const size = file.size;

  if (size === 0) {
    throw new Error("Cannot upload an empty file");
  }

  // Request upload slot via XEP-0363 IQ
  const slotResponse = await xmpp.sendIQ({
    to: uploadDomain,
    type: "get",
    httpUpload: {
      type: "request",
      name: filename,
      size,
      mediaType: contentType,
    },
  } as any);

  const slot = parseSlotResponse(slotResponse);

  await uploadToSlot(file, slot.putUrl, slot.putHeaders, contentType, onProgress);

  return {
    getUrl: slot.getUrl,
    filename,
    contentType,
    size,
  };
}

/**
 * Convert a clipboard paste event or drop event into a File.
 * Returns null if no image is found.
 */
export function extractImageFromEvent(event: ClipboardEvent | DragEvent): File | null {
  if (event instanceof ClipboardEvent) {
    const items = event.clipboardData?.items;
    if (!items) return null;
    for (let i = 0; i < items.length; i++) {
      if (items[i].type.startsWith("image/")) {
        return items[i].getAsFile();
      }
    }
  } else if (event instanceof DragEvent) {
    const files = event.dataTransfer?.files;
    if (!files) return null;
    for (let i = 0; i < files.length; i++) {
      if (files[i].type.startsWith("image/")) {
        return files[i];
      }
    }
  }
  return null;
}

function parseSlotResponse(response: any): SlotInfo {
  // stanza.js may parse the slot under httpUpload or the raw element tree
  const slot = response?.httpUpload ?? response?.slot;
  if (!slot) {
    throw new Error("Upload slot response missing: server did not return a valid slot");
  }

  const putUrl: string | undefined =
    slot.put?.url ?? slot.put?.href ?? (typeof slot.put === "string" ? slot.put : undefined);
  const getUrl: string | undefined =
    slot.get?.url ?? slot.get?.href ?? (typeof slot.get === "string" ? slot.get : undefined);

  if (!putUrl || !getUrl) {
    throw new Error("Upload slot missing PUT or GET URL");
  }

  // Extract optional headers the server requires on the PUT request
  const putHeaders: Array<[string, string]> = [];
  const rawHeaders = slot.put?.headers ?? slot.put?.header ?? [];
  const headerList = Array.isArray(rawHeaders) ? rawHeaders : [rawHeaders];
  for (const h of headerList) {
    if (h && typeof h.name === "string" && typeof h.value === "string") {
      putHeaders.push([h.name, h.value]);
    }
  }

  return { putUrl, getUrl, putHeaders };
}

async function uploadToSlot(
  file: File | Blob,
  putUrl: string,
  headers: Array<[string, string]>,
  contentType: string,
  onProgress?: (progress: UploadProgress) => void,
): Promise<void> {
  // Use XMLHttpRequest for progress reporting
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("PUT", putUrl);
    xhr.setRequestHeader("Content-Type", contentType);
    for (const [name, value] of headers) {
      xhr.setRequestHeader(name, value);
    }

    if (onProgress) {
      xhr.upload.onprogress = (e) => {
        if (e.lengthComputable) {
          onProgress({ loaded: e.loaded, total: e.total });
        }
      };
    }

    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        resolve();
      } else {
        reject(new Error(`Upload failed: ${xhr.status} ${xhr.statusText}`));
      }
    };
    xhr.onerror = () => reject(new Error("Upload failed: network error"));
    xhr.send(file);
  });
}
