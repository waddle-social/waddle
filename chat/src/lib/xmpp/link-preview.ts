export type LinkPreviewLookupResult = LinkPreviewLookupReadyResult | LinkPreviewLookupUnsupportedResult;

export interface LinkPreviewLookupReadyResult {
  token: string;
  originalUrl: string;
  normalizedUrl: string;
  status: "ready";
  expiresAt: string;
  title?: string;
  description?: string;
}

export interface LinkPreviewLookupUnsupportedResult {
  originalUrl: string;
  status: "unsupported" | "blocked" | "failed";
}

const HTTPS_URL_RE = /https:\/\/[^\s<>"']+/gi;
const NS_WADDLE_LINK_PREVIEW = "urn:waddle:link-preview:0";
const LOOKUP_TIMEOUT_MS = 2_000;
const MAX_LINK_PREVIEW_TOKEN_BYTES = 4096;

type LinkPreviewIqClient = {
  send_raw_iq?: (xml: string) => Promise<string>;
  cancel_raw_iq?: (id: string) => Promise<void>;
};

export function firstEligibleHttpsUrl(body: string): string | null {
  HTTPS_URL_RE.lastIndex = 0;
  for (const match of body.matchAll(HTTPS_URL_RE)) {
    const raw = trimTrailingUrlPunctuation(match[0] ?? "");
    try {
      const url = new URL(raw);
      if (url.protocol === "https:" && url.hostname.includes(".")) return raw;
    } catch {
      // Keep scanning; malformed URL-like text is not eligible.
    }
  }
  return null;
}

export async function requestPlaintextLinkPreviewLookup(
  client: LinkPreviewIqClient | null | undefined,
  body: string,
  scopeJid: string,
): Promise<LinkPreviewLookupResult | null> {
  const originalUrl = firstEligibleHttpsUrl(body);
  const trimmedScopeJid = scopeJid.trim();
  if (!originalUrl || !trimmedScopeJid || typeof client?.send_raw_iq !== "function") return null;

  const id = crypto.randomUUID();
  const iq = buildLookupIqStanza(id, originalUrl, trimmedScopeJid);
  if (!iq) return null;
  let response: string;
  try {
    response = await withLookupTimeout(
      client.send_raw_iq(iq),
      () => client.cancel_raw_iq?.(id),
    );
  } catch {
    return null;
  }
  return parseLookupResponse(response, originalUrl);
}

function parseLookupResponse(xml: string, requestedUrl: string): LinkPreviewLookupResult | null {
  const doc = new DOMParser().parseFromString(xml, "text/xml");
  const lookup = doc.getElementsByTagNameNS(NS_WADDLE_LINK_PREVIEW, "lookup")[0];
  if (!lookup) return null;
  const status = lookup.getAttribute("status");
  if (status === "unsupported" || status === "blocked" || status === "failed") {
    return { status, originalUrl: requestedUrl };
  }
  if (status !== "ready") return null;
  const preview = Array.from(lookup.children).find(
    (child) => child.localName === "preview" && child.namespaceURI === NS_WADDLE_LINK_PREVIEW,
  );
  if (!preview) return null;
  const token = preview.getAttribute("token")?.trim();
  const originalUrl = preview.getAttribute("original-url")?.trim();
  const normalizedUrl = preview.getAttribute("normalized-url")?.trim();
  const expiresAt = preview.getAttribute("expires-at")?.trim();
  if (!token || !originalUrl || !normalizedUrl || !expiresAt) return null;
  if (token.length > MAX_LINK_PREVIEW_TOKEN_BYTES) return null;
  if (!urlsMatchSemantically(originalUrl, requestedUrl)) return null;
  if (!isEligiblePreviewUrl(originalUrl) || !isEligiblePreviewUrl(normalizedUrl)) return null;
  return {
    token,
    originalUrl,
    normalizedUrl,
    status: "ready",
    expiresAt,
    ...childText(preview, "title"),
    ...childText(preview, "description"),
  };
}

function isEligiblePreviewUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "https:" && url.hostname.includes(".");
  } catch {
    return false;
  }
}

function urlsMatchSemantically(a: string, b: string): boolean {
  const canonicalA = canonicalUrl(a);
  const canonicalB = canonicalUrl(b);
  return canonicalA && canonicalB
    ? canonicalA === canonicalB
    : a === b;
}

function canonicalUrl(value: string): string | null {
  try {
    return new URL(value).href;
  } catch {
    return null;
  }
}

function trimTrailingUrlPunctuation(url: string): string {
  return url.replace(/^[([<{]+/g, "").replace(/[)\]}>.,!?;:]+$/g, "");
}

function buildLookupIqStanza(id: string, originalUrl: string, scopeJid: string): string | null {
  const doc = newXmlDocument("iq");
  if (!doc) return null;

  const iq = doc.documentElement;
  iq.setAttribute("type", "get");
  iq.setAttribute("id", id);

  const lookup = doc.createElementNS(NS_WADDLE_LINK_PREVIEW, "lookup");
  appendTextElement(doc, lookup, "url", originalUrl);
  appendTextElement(doc, lookup, "scope", scopeJid);
  iq.appendChild(lookup);

  return new XMLSerializer().serializeToString(iq);
}

function newXmlDocument(rootName: string): XMLDocument | null {
  if (typeof document === "undefined" || typeof XMLSerializer === "undefined") return null;
  return document.implementation.createDocument("", rootName);
}

function appendTextElement(doc: XMLDocument, parent: Element, name: string, value: string): void {
  const child = doc.createElementNS(NS_WADDLE_LINK_PREVIEW, name);
  child.appendChild(doc.createTextNode(value));
  parent.appendChild(child);
}

async function withLookupTimeout(raw: Promise<string>, cancel: () => void | Promise<void> | undefined): Promise<string> {
  let didTimeout = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => {
      didTimeout = true;
      reject(new Error("link preview lookup timed out"));
    }, LOOKUP_TIMEOUT_MS);
  });
  try {
    return await Promise.race([raw, timeout]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
    if (didTimeout) {
      try {
        await cancel();
      } catch {
        // Best effort only; link-preview lookup must never block message send.
      }
    }
  }
}

function childText(parent: Element, name: "title" | "description"): Partial<LinkPreviewLookupResult> {
  const value = Array.from(parent.children)
    .find((child) => child.localName === name && child.namespaceURI === NS_WADDLE_LINK_PREVIEW)
    ?.textContent
    ?.trim();
  return value ? { [name]: value } : {};
}
