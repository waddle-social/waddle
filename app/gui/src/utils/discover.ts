/**
 * XEP-0156 endpoint discovery: given a domain, try to find the WebSocket URL
 * via .well-known/host-meta, then fall back to a convention-based default.
 */

interface Link {
  rel?: string;
  href?: string;
}

function parseHostMetaJson(json: unknown): string | null {
  if (!json || typeof json !== 'object') return null;
  const links = (json as { links?: Link[] }).links;
  if (!Array.isArray(links)) return null;

  for (const link of links) {
    if (
      link.rel === 'urn:xmpp:alt-connections:websocket' &&
      typeof link.href === 'string' &&
      link.href.startsWith('ws')
    ) {
      return link.href;
    }
  }
  return null;
}

function parseHostMetaXml(text: string): string | null {
  const parser = new DOMParser();
  const doc = parser.parseFromString(text, 'application/xml');
  const links = doc.querySelectorAll('Link');

  for (const link of links) {
    const rel = link.getAttribute('rel');
    const href = link.getAttribute('href');
    if (rel === 'urn:xmpp:alt-connections:websocket' && href?.startsWith('ws')) {
      return href;
    }
  }
  return null;
}

export async function discoverWebSocketEndpoint(domain: string): Promise<string> {
  // 1. Try JSON host-meta
  try {
    const jsonResponse = await fetch(`https://${domain}/.well-known/host-meta.json`, {
      signal: AbortSignal.timeout(5000),
    });
    if (jsonResponse.ok) {
      const json: unknown = await jsonResponse.json();
      const url = parseHostMetaJson(json);
      if (url) return url;
    }
  } catch {
    // Ignore — try XML next
  }

  // 2. Try XML host-meta
  try {
    const xmlResponse = await fetch(`https://${domain}/.well-known/host-meta`, {
      signal: AbortSignal.timeout(5000),
    });
    if (xmlResponse.ok) {
      const text = await xmlResponse.text();
      const url = parseHostMetaXml(text);
      if (url) return url;
    }
  } catch {
    // Ignore — use fallback
  }

  // 3. Convention-based fallback
  return `wss://${domain}:5281/xmpp-websocket`;
}
