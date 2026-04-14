import type { APIRoute } from "astro";

import { getAuth } from "../../../lib/auth";
import { proxyOpenIdConfiguration } from "../../../lib/openid-config";

type DpopJwk = {
  kty: "EC";
  crv: "P-256";
  x: string;
  y: string;
};

type DpopHeader = {
  typ?: string;
  alg?: string;
  jwk?: DpopJwk;
};

type DpopPayload = {
  htm?: string;
  htu?: string;
  iat?: number;
  jti?: string;
};

function decodeBase64Url(value: string): ArrayBuffer {
  const pad = value.length % 4;
  const normalized =
    value.replace(/-/g, "+").replace(/_/g, "/") +
    (pad === 0 ? "" : "=".repeat(4 - pad));
  const raw = atob(normalized);
  return Uint8Array.from(raw, (char) => char.charCodeAt(0)).buffer;
}

function parseJsonPart<T>(value: string): T | null {
  try {
    return JSON.parse(
      new TextDecoder().decode(new Uint8Array(decodeBase64Url(value))),
    ) as T;
  } catch {
    return null;
  }
}

async function validateDpopHeader(request: Request): Promise<boolean> {
  const proof = request.headers.get("DPoP")?.trim();
  if (!proof) {
    return false;
  }

  const parts = proof.split(".");
  if (parts.length !== 3) {
    return false;
  }

  const encodedHeader = parts[0];
  const encodedPayload = parts[1];
  const encodedSignature = parts[2];
  if (!encodedHeader || !encodedPayload || !encodedSignature) {
    return false;
  }
  const header = parseJsonPart<DpopHeader>(encodedHeader);
  const payload = parseJsonPart<DpopPayload>(encodedPayload);
  if (!header || !payload) {
    return false;
  }

  if (header.typ !== "dpop+jwt" || header.alg !== "ES256" || !header.jwk) {
    return false;
  }
  if (
    header.jwk.kty !== "EC" ||
    header.jwk.crv !== "P-256" ||
    !header.jwk.x ||
    !header.jwk.y
  ) {
    return false;
  }

  if (payload.htm !== request.method.toUpperCase() || payload.htu !== request.url) {
    return false;
  }
  if (typeof payload.iat !== "number" || !Number.isFinite(payload.iat)) {
    return false;
  }
  if (typeof payload.jti !== "string" || payload.jti.trim().length === 0) {
    return false;
  }

  const now = Math.floor(Date.now() / 1000);
  if (Math.abs(now - Math.floor(payload.iat)) > 300) {
    return false;
  }

  try {
    const key = await crypto.subtle.importKey(
      "jwk",
      {
        kty: "EC",
        crv: "P-256",
        x: header.jwk.x,
        y: header.jwk.y,
        ext: true,
      },
      { name: "ECDSA", namedCurve: "P-256" },
      false,
      ["verify"],
    );
    const valid = await crypto.subtle.verify(
      { name: "ECDSA", hash: "SHA-256" },
      key,
      decodeBase64Url(encodedSignature),
      new TextEncoder().encode(`${encodedHeader}.${encodedPayload}`),
    );
    return valid;
  } catch {
    return false;
  }
}

export const ALL: APIRoute = async ({ request }) => {
  const url = new URL(request.url);
  if (url.pathname.endsWith("/.well-known/openid-configuration")) {
    return proxyOpenIdConfiguration(request, url.pathname);
  }
  if (url.pathname.endsWith("/oauth2/token") && !(await validateDpopHeader(request))) {
    return new Response(
      JSON.stringify({
        error: "invalid_dpop_proof",
        error_description: "Invalid DPoP proof",
      }),
      {
        status: 400,
        headers: {
          "Content-Type": "application/json",
          "Cache-Control": "no-store",
          Pragma: "no-cache",
        },
      },
    );
  }

  const auth = await getAuth();
  return auth.handler(request);
};
